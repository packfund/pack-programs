//! The public mint: `mint_binder_dbc` (before the migration) and `mint_binder_damm` (after it). One instruction, one
//! transaction: pay, sync the pot, swap SOL for $PACK by CPI and burn the balance difference (P2-00), create the Core asset
//! and the `Binder` record, count it, share genesis at the 100th public mint. If any part fails nothing happens.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use anchor_spl::token::{Token, TokenAccount as SplTokenAccount};
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use crate::constants::*;
use crate::core_cpi;
use crate::cpi_lite;
use crate::errors::BinderError;
use crate::events::{BinderMinted, GenesisShared, PotSynced, RoundClosed};
use crate::math::{next_public_number, public_left, tier_of};
use crate::pot::{
    check_identity, genesis_window_end, register_mint, require_synced, share_genesis,
    sync_pot_state,
};
use crate::state::{Binder, Config, State};
use crate::util::{binder_name, binder_uri, pot_rent_min};
use crate::venue::{buy_and_burn, FlowAccounts};

/// The venue part of a mint: which program, the exact account list of its `swap2` and the mode (PartialFill on DBC,
/// ExactIn on DAMM v2: never an input).
pub struct VenueCall<'info> {
    pub program: Pubkey,
    pub metas: Vec<AccountMeta>,
    pub infos: Vec<AccountInfo<'info>>,
    pub mode: u8,
    /// `Config.phase` this venue is for.
    pub phase: u8,
    /// 0 = DBC, 1 = DAMM v2 (for the event).
    pub venue: u8,
}

/// The accounts both mint instructions share (their `Accounts` structs use the same field names).
pub struct MintAccounts<'a, 'info> {
    pub buyer: &'a Signer<'info>,
    pub config: &'a Account<'info, Config>,
    pub state: &'a mut Account<'info, State>,
    pub pot: &'a SystemAccount<'info>,
    pub treasury: &'a SystemAccount<'info>,
    pub swapper: &'a UncheckedAccount<'info>,
    pub collection_authority: &'a UncheckedAccount<'info>,
    pub pack_mint: &'a InterfaceAccount<'info, Mint>,
    pub swapper_pack: &'a mut InterfaceAccount<'info, TokenAccount>,
    pub swapper_wsol: &'a mut Account<'info, SplTokenAccount>,
    pub collection: &'a UncheckedAccount<'info>,
    pub asset: &'a Signer<'info>,
    pub binder: &'a mut Account<'info, Binder>,
    pub core_program: &'a UncheckedAccount<'info>,
    pub pack_token_program: &'a Interface<'info, TokenInterface>,
    pub token_program: &'a Program<'info, Token>,
    pub system_program: &'a Program<'info, System>,
}

/// The events of a mint, in the order they are emitted.
pub struct MintEvents {
    pub synced: Option<PotSynced>,
    pub closed: Vec<RoundClosed>,
    pub minted: BinderMinted,
    pub genesis: Option<GenesisShared>,
}

/// The checks that decide whether a public mint may start, and what it will cost: `(tier, burn lamports, number)`.
/// Nothing has moved when this returns.
pub fn precheck(
    config: &Config,
    state: &State,
    phase: u8,
    min_out: u64,
) -> Result<(usize, u64, u16)> {
    require!(!config.cancelled, BinderError::Cancelled);
    require!(!config.paused, BinderError::Paused);
    require!(state.opened_at > 0, BinderError::NotOpen);
    require!(
        state.reserve_minted == TEAM_COUNT,
        BinderError::ReserveIncomplete
    );
    require!(public_left(state.public_minted), BinderError::SoldOut);
    require!(min_out > 0, BinderError::ZeroAmount);
    require!(config.phase == phase, BinderError::WrongPhase);
    let tier = tier_of(state.public_minted);
    let burn = *config
        .burn_by_tier
        .get(tier)
        .ok_or_else(|| error!(BinderError::SoldOut))?;
    require!(
        burn >= config.min_burn_lamports && burn <= config.max_burn_lamports,
        BinderError::BurnOutOfBounds
    );
    let number = next_public_number(&config.team_mask, &state.minted_mask, state.next_public)
        .ok_or_else(|| error!(BinderError::SoldOut))?;
    Ok((tier, burn, number))
}

pub fn run_mint<'a, 'info>(
    m: MintAccounts<'a, 'info>,
    venue: VenueCall<'info>,
    min_out: u64,
    binder_bump: u8,
) -> Result<MintEvents> {
    let MintAccounts {
        buyer,
        config,
        state,
        pot,
        treasury,
        swapper,
        collection_authority,
        pack_mint,
        swapper_pack,
        swapper_wsol,
        collection,
        asset,
        binder,
        core_program,
        pack_token_program,
        token_program,
        system_program,
    } = m;

    let (tier, burn, number) = precheck(config, state, venue.phase, min_out)?;

    // 1. the payment: 0.30 SOL to the pot and 0.20 SOL to the Treasury (fixed, spec 8)
    cpi_lite::system_transfer(
        &buyer.to_account_info(),
        &pot.to_account_info(),
        POT_LAMPORTS,
        &[],
    )?;
    cpi_lite::system_transfer(
        &buyer.to_account_info(),
        &treasury.to_account_info(),
        TREASURY_LAMPORTS,
        &[],
    )?;

    // 2. the sync, BEFORE the new Binder is counted: the mint's own 0.30 SOL goes to the Binders that exist
    let rent_min = pot_rent_min()?;
    let pot_info = pot.to_account_info();
    let sync = sync_pot_state(state, pot_info.lamports(), rent_min, config.round_threshold)?;
    // a Binder is counted only when no income is left to realize later: the client puts as many sync_pot in front as needed (R1)
    require_synced(pot_info.lamports(), rent_min, state)?;

    // 3. buy $PACK by CPI with the swapper as signer and burn exactly the balance difference
    let swap = buy_and_burn(
        FlowAccounts {
            buyer,
            swapper,
            swapper_bump: config.swapper_bump,
            swapper_pack,
            swapper_wsol,
            pack_mint,
            pack_token_program,
            token_program,
            system_program,
        },
        &venue.program,
        venue.mode,
        burn,
        min_out,
        venue.metas,
        &venue.infos,
    )?;

    // 4. the Core asset: the program's own name and URI, owned by the buyer
    core_cpi::create_asset(
        &core_program.to_account_info(),
        &asset.to_account_info(),
        &collection.to_account_info(),
        &collection_authority.to_account_info(),
        &buyer.to_account_info(),
        &buyer.to_account_info(),
        &system_program.to_account_info(),
        binder_name(number),
        binder_uri(&config.base_uri, number),
        config.authority_bump,
    )?;

    // 4b. the 24 hour window (review R2): a mint that comes after it closes the genesis set itself, BEFORE the new Binder is
    // counted, so the result does not depend on who calls `finalize_genesis` first and a bundled [mint, finalize] cannot get in
    let mut genesis = None;
    if !state.genesis_shared && Clock::get()?.unix_timestamp >= genesis_window_end(state)? {
        let g = share_genesis(state)?;
        genesis = Some(GenesisShared {
            eligible: g.eligible,
            per_binder: g.per_binder,
            remainder: g.remainder,
            trigger: 2,
        });
    }

    // 5. count it: counters, bitmap, cursor, genesis eligibility, and the record with the accumulator snapshot
    let registered = register_mint(state, number, false)?;
    binder.asset = asset.key();
    binder.number = number;
    binder.seq = registered.seq;
    binder.is_team = false;
    binder.genesis_eligible = registered.genesis_eligible;
    binder.genesis_claimed = false;
    binder.forfeited = false;
    binder.acc_snapshot = state.acc_per_binder;
    binder.sol_claimed = 0;
    binder.bump = binder_bump;

    // 6. the 100th public mint shares genesis, inside this instruction
    if state.public_minted == GENESIS_PUBLIC_MINTS && !state.genesis_shared {
        let g = share_genesis(state)?;
        genesis = Some(GenesisShared {
            eligible: g.eligible,
            per_binder: g.per_binder,
            remainder: g.remainder,
            trigger: 0,
        });
    }

    check_identity(pot_info.lamports(), rent_min, state)?;

    Ok(MintEvents {
        synced: sync.synced,
        closed: sync.closed,
        minted: BinderMinted {
            number,
            seq: registered.seq,
            asset: asset.key(),
            buyer: buyer.key(),
            tier: u8::try_from(tier).unwrap_or(u8::MAX),
            burn_lamports: burn,
            wsol_spent: swap.wsol_spent,
            received: swap.received,
            burned: swap.received,
            refunded_lamports: swap.refunded_lamports,
            pot_in: POT_LAMPORTS,
            treasury_in: TREASURY_LAMPORTS,
            venue: venue.venue,
            genesis_eligible: registered.genesis_eligible,
        },
        genesis,
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// DBC (before the migration)
// ---------------------------------------------------------------------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct MintBinderDbc<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
    #[account(mut, seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: SystemAccount<'info>,
    #[account(mut, address = config.treasury @ BinderError::InvalidConfigKey)]
    pub treasury: SystemAccount<'info>,
    /// CHECK: the swapper PDA: system-owned, the owner of the two token accounts and the `payer` of the venue's swap.
    #[account(seeds = [SWAPPER_SEED], bump = config.swapper_bump)]
    pub swapper: UncheckedAccount<'info>,
    /// CHECK: the collection authority PDA, which signs the Core creation.
    #[account(seeds = [AUTHORITY_SEED], bump = config.authority_bump)]
    pub collection_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        address = config.pack_mint @ BinderError::MintNotPinned,
        constraint = *pack_mint.to_account_info().owner == pack_token_program.key() @ BinderError::WrongTokenProgram
    )]
    pub pack_mint: Box<InterfaceAccount<'info, Mint>>,
    /// The swapper's associated $PACK account, pinned in Config at init.
    #[account(
        mut,
        address = config.swapper_pack_ata @ BinderError::WrongTokenAccount,
        constraint = swapper_pack.mint == pack_mint.key() @ BinderError::WrongTokenAccount,
        constraint = swapper_pack.owner == swapper.key() @ BinderError::WrongTokenAccount,
        constraint = *swapper_pack.to_account_info().owner == pack_token_program.key() @ BinderError::WrongTokenProgram
    )]
    pub swapper_pack: Box<InterfaceAccount<'info, TokenAccount>>,
    /// The swapper's associated wSOL account (created idempotently by the client in the same transaction).
    #[account(
        mut,
        address = config.swapper_wsol_ata @ BinderError::WrongTokenAccount,
        constraint = swapper_wsol.mint == WSOL_MINT @ BinderError::WrongTokenAccount,
        constraint = swapper_wsol.owner == swapper.key() @ BinderError::WrongTokenAccount
    )]
    pub swapper_wsol: Box<Account<'info, SplTokenAccount>>,
    /// CHECK: the wSOL mint, a constant address.
    #[account(address = WSOL_MINT @ BinderError::MintNotPinned)]
    pub wsol_mint: UncheckedAccount<'info>,
    /// CHECK: the DBC program; a wrong program would receive the swapper PDA's signature, so it is a constant address.
    #[account(address = DBC_PROGRAM @ BinderError::WrongVenueProgram, executable)]
    pub dbc_program: UncheckedAccount<'info>,
    /// CHECK: the DBC `pool_authority`, a constant address.
    #[account(address = DBC_POOL_AUTHORITY @ BinderError::WrongAuthority)]
    pub dbc_pool_authority: UncheckedAccount<'info>,
    /// CHECK: the DBC `event_authority`, a constant address.
    #[account(address = DBC_EVENT_AUTHORITY @ BinderError::WrongAuthority)]
    pub dbc_event_authority: UncheckedAccount<'info>,
    /// CHECK: the pinned DBC config.
    #[account(address = config.dbc_config @ BinderError::PoolNotPinned)]
    pub dbc_config: UncheckedAccount<'info>,
    /// CHECK: the pinned DBC pool.
    #[account(mut, address = config.dbc_pool @ BinderError::PoolNotPinned)]
    pub pool: UncheckedAccount<'info>,
    /// CHECK: the pinned DBC base vault.
    #[account(mut, address = config.dbc_base_vault @ BinderError::VaultNotPinned)]
    pub base_vault: UncheckedAccount<'info>,
    /// CHECK: the pinned DBC quote vault.
    #[account(mut, address = config.dbc_quote_vault @ BinderError::VaultNotPinned)]
    pub quote_vault: UncheckedAccount<'info>,
    /// CHECK: the pinned Core collection.
    #[account(mut, address = config.collection @ BinderError::WrongCollection)]
    pub collection: UncheckedAccount<'info>,
    /// The new Binder asset: a fresh keypair that signs the transaction.
    #[account(mut)]
    pub asset: Signer<'info>,
    #[account(
        init,
        payer = buyer,
        space = 8 + Binder::INIT_SPACE,
        seeds = [BINDER_SEED, asset.key().as_ref()],
        bump
    )]
    pub binder: Box<Account<'info, Binder>>,
    /// CHECK: the Metaplex Core program, a constant address.
    #[account(address = crate::core_cpi::CORE_ID @ BinderError::WrongCoreProgram)]
    pub core_program: UncheckedAccount<'info>,
    #[account(address = config.pack_token_program @ BinderError::WrongTokenProgram)]
    pub pack_token_program: Interface<'info, TokenInterface>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handle_mint_binder_dbc(ctx: Context<MintBinderDbc>, min_out: u64) -> Result<()> {
    let binder_bump = ctx.bumps.binder;
    let a = &mut *ctx.accounts;
    // the exact account list of DBC `swap2` in IDL order; `payer` is the swapper PDA, the only signer
    let swapper = a.swapper.key();
    let metas = vec![
        AccountMeta::new_readonly(DBC_POOL_AUTHORITY, false),
        AccountMeta::new_readonly(a.dbc_config.key(), false),
        AccountMeta::new(a.pool.key(), false),
        AccountMeta::new(a.swapper_wsol.key(), false), // input: wSOL
        AccountMeta::new(a.swapper_pack.key(), false), // output: $PACK
        AccountMeta::new(a.base_vault.key(), false),
        AccountMeta::new(a.quote_vault.key(), false),
        AccountMeta::new_readonly(a.pack_mint.key(), false), // base mint
        AccountMeta::new_readonly(WSOL_MINT, false),         // quote mint
        AccountMeta::new_readonly(swapper, true),            // payer
        AccountMeta::new_readonly(a.pack_token_program.key(), false), // token_base_program
        AccountMeta::new_readonly(a.token_program.key(), false), // token_quote_program
        AccountMeta::new_readonly(DBC_PROGRAM, false),       // referral_token_account: None
        AccountMeta::new_readonly(DBC_EVENT_AUTHORITY, false),
        AccountMeta::new_readonly(DBC_PROGRAM, false),
    ];
    let infos = vec![
        a.dbc_pool_authority.to_account_info(),
        a.dbc_config.to_account_info(),
        a.pool.to_account_info(),
        a.swapper_wsol.to_account_info(),
        a.swapper_pack.to_account_info(),
        a.base_vault.to_account_info(),
        a.quote_vault.to_account_info(),
        a.pack_mint.to_account_info(),
        a.wsol_mint.to_account_info(),
        a.swapper.to_account_info(),
        a.pack_token_program.to_account_info(),
        a.token_program.to_account_info(),
        a.dbc_program.to_account_info(),
        a.dbc_event_authority.to_account_info(),
        a.dbc_program.to_account_info(),
    ];
    let events = run_mint(
        MintAccounts {
            buyer: &a.buyer,
            config: &a.config,
            state: &mut a.state,
            pot: &a.pot,
            treasury: &a.treasury,
            swapper: &a.swapper,
            collection_authority: &a.collection_authority,
            pack_mint: &a.pack_mint,
            swapper_pack: &mut a.swapper_pack,
            swapper_wsol: &mut a.swapper_wsol,
            collection: &a.collection,
            asset: &a.asset,
            binder: &mut a.binder,
            core_program: &a.core_program,
            pack_token_program: &a.pack_token_program,
            token_program: &a.token_program,
            system_program: &a.system_program,
        },
        VenueCall {
            program: DBC_PROGRAM,
            metas,
            infos,
            mode: MODE_PARTIAL_FILL,
            phase: PHASE_DBC,
            venue: 0,
        },
        min_out,
        binder_bump,
    )?;
    if let Some(e) = events.synced {
        emit_cpi!(e);
    }
    for e in events.closed {
        emit_cpi!(e);
    }
    emit_cpi!(events.minted);
    if let Some(e) = events.genesis {
        emit_cpi!(e);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// DAMM v2 (after the migration)
// ---------------------------------------------------------------------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct MintBinderDamm<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
    #[account(mut, seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: SystemAccount<'info>,
    #[account(mut, address = config.treasury @ BinderError::InvalidConfigKey)]
    pub treasury: SystemAccount<'info>,
    /// CHECK: the swapper PDA, see `MintBinderDbc`.
    #[account(seeds = [SWAPPER_SEED], bump = config.swapper_bump)]
    pub swapper: UncheckedAccount<'info>,
    /// CHECK: the collection authority PDA, which signs the Core creation.
    #[account(seeds = [AUTHORITY_SEED], bump = config.authority_bump)]
    pub collection_authority: UncheckedAccount<'info>,
    #[account(
        mut,
        address = config.pack_mint @ BinderError::MintNotPinned,
        constraint = *pack_mint.to_account_info().owner == pack_token_program.key() @ BinderError::WrongTokenProgram
    )]
    pub pack_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        address = config.swapper_pack_ata @ BinderError::WrongTokenAccount,
        constraint = swapper_pack.mint == pack_mint.key() @ BinderError::WrongTokenAccount,
        constraint = swapper_pack.owner == swapper.key() @ BinderError::WrongTokenAccount,
        constraint = *swapper_pack.to_account_info().owner == pack_token_program.key() @ BinderError::WrongTokenProgram
    )]
    pub swapper_pack: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        address = config.swapper_wsol_ata @ BinderError::WrongTokenAccount,
        constraint = swapper_wsol.mint == WSOL_MINT @ BinderError::WrongTokenAccount,
        constraint = swapper_wsol.owner == swapper.key() @ BinderError::WrongTokenAccount
    )]
    pub swapper_wsol: Box<Account<'info, SplTokenAccount>>,
    /// CHECK: the wSOL mint, a constant address.
    #[account(address = WSOL_MINT @ BinderError::MintNotPinned)]
    pub wsol_mint: UncheckedAccount<'info>,
    /// CHECK: the DAMM v2 program, a constant address (see `MintBinderDbc.dbc_program`).
    #[account(address = DAMM_PROGRAM @ BinderError::WrongVenueProgram, executable)]
    pub damm_program: UncheckedAccount<'info>,
    /// CHECK: the DAMM v2 `pool_authority`, a constant address.
    #[account(address = DAMM_POOL_AUTHORITY @ BinderError::WrongAuthority)]
    pub damm_pool_authority: UncheckedAccount<'info>,
    /// CHECK: the DAMM v2 `event_authority`, a constant address.
    #[account(address = DAMM_EVENT_AUTHORITY @ BinderError::WrongAuthority)]
    pub damm_event_authority: UncheckedAccount<'info>,
    /// CHECK: the pinned DAMM v2 pool (set once by `set_damm`).
    #[account(mut, address = config.damm_pool @ BinderError::PoolNotPinned)]
    pub pool: UncheckedAccount<'info>,
    /// CHECK: the pinned token A vault ($PACK).
    #[account(mut, address = config.damm_vault_a @ BinderError::VaultNotPinned)]
    pub token_a_vault: UncheckedAccount<'info>,
    /// CHECK: the pinned token B vault (wSOL).
    #[account(mut, address = config.damm_vault_b @ BinderError::VaultNotPinned)]
    pub token_b_vault: UncheckedAccount<'info>,
    /// CHECK: the pinned Core collection.
    #[account(mut, address = config.collection @ BinderError::WrongCollection)]
    pub collection: UncheckedAccount<'info>,
    #[account(mut)]
    pub asset: Signer<'info>,
    #[account(
        init,
        payer = buyer,
        space = 8 + Binder::INIT_SPACE,
        seeds = [BINDER_SEED, asset.key().as_ref()],
        bump
    )]
    pub binder: Box<Account<'info, Binder>>,
    /// CHECK: the Metaplex Core program, a constant address.
    #[account(address = crate::core_cpi::CORE_ID @ BinderError::WrongCoreProgram)]
    pub core_program: UncheckedAccount<'info>,
    #[account(address = config.pack_token_program @ BinderError::WrongTokenProgram)]
    pub pack_token_program: Interface<'info, TokenInterface>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handle_mint_binder_damm(ctx: Context<MintBinderDamm>, min_out: u64) -> Result<()> {
    let binder_bump = ctx.bumps.binder;
    let a = &mut *ctx.accounts;
    // DAMM v2 `swap2` account list in IDL order; token A = $PACK, token B = wSOL
    let swapper = a.swapper.key();
    let metas = vec![
        AccountMeta::new_readonly(DAMM_POOL_AUTHORITY, false),
        AccountMeta::new(a.pool.key(), false),
        AccountMeta::new(a.swapper_wsol.key(), false), // input: wSOL
        AccountMeta::new(a.swapper_pack.key(), false), // output: $PACK
        AccountMeta::new(a.token_a_vault.key(), false),
        AccountMeta::new(a.token_b_vault.key(), false),
        AccountMeta::new_readonly(a.pack_mint.key(), false), // token A mint
        AccountMeta::new_readonly(WSOL_MINT, false),         // token B mint
        AccountMeta::new_readonly(swapper, true),            // payer
        AccountMeta::new_readonly(a.pack_token_program.key(), false), // token_a_program
        AccountMeta::new_readonly(a.token_program.key(), false), // token_b_program
        AccountMeta::new_readonly(DAMM_PROGRAM, false),      // referral_token_account: None
        AccountMeta::new_readonly(DAMM_EVENT_AUTHORITY, false),
        AccountMeta::new_readonly(DAMM_PROGRAM, false),
    ];
    let infos = vec![
        a.damm_pool_authority.to_account_info(),
        a.pool.to_account_info(),
        a.swapper_wsol.to_account_info(),
        a.swapper_pack.to_account_info(),
        a.token_a_vault.to_account_info(),
        a.token_b_vault.to_account_info(),
        a.pack_mint.to_account_info(),
        a.wsol_mint.to_account_info(),
        a.swapper.to_account_info(),
        a.pack_token_program.to_account_info(),
        a.token_program.to_account_info(),
        a.damm_program.to_account_info(),
        a.damm_event_authority.to_account_info(),
        a.damm_program.to_account_info(),
    ];
    let events = run_mint(
        MintAccounts {
            buyer: &a.buyer,
            config: &a.config,
            state: &mut a.state,
            pot: &a.pot,
            treasury: &a.treasury,
            swapper: &a.swapper,
            collection_authority: &a.collection_authority,
            pack_mint: &a.pack_mint,
            swapper_pack: &mut a.swapper_pack,
            swapper_wsol: &mut a.swapper_wsol,
            collection: &a.collection,
            asset: &a.asset,
            binder: &mut a.binder,
            core_program: &a.core_program,
            pack_token_program: &a.pack_token_program,
            token_program: &a.token_program,
            system_program: &a.system_program,
        },
        VenueCall {
            program: DAMM_PROGRAM,
            metas,
            infos,
            mode: MODE_EXACT_IN,
            phase: PHASE_DAMM,
            venue: 1,
        },
        min_out,
        binder_bump,
    )?;
    if let Some(e) = events.synced {
        emit_cpi!(e);
    }
    for e in events.closed {
        emit_cpi!(e);
    }
    emit_cpi!(events.minted);
    if let Some(e) = events.genesis {
        emit_cpi!(e);
    }
    Ok(())
}
