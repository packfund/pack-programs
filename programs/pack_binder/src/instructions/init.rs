//! `init_config` and `init_collection`: the two setup instructions, signed only by the program's upgrade authority (the
//! `pack_bond` pattern: nobody else can front-run the initialisation and install their own admin or collection).

use anchor_lang::prelude::*;
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use crate::constants::*;
use crate::core_cpi;
use crate::cpi_lite;
use crate::errors::BinderError;
use crate::events::{CollectionCreated, ConfigInitialized};
use crate::math::{burn_tiers, tiers_within_bounds};
use crate::pack::validate_pack_mint;
use crate::state::{Config, Pending, State};
use crate::util::{pot_rent_min, valid_rule_set};
use crate::venue::{check_dbc_config, parse_dbc_pool};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitConfigArgs {
    /// Squads vault on mainnet.
    pub admin: Pubkey,
    /// The owner wallet, the only signer of `mint_reserve`.
    pub reserve_signer: Pubkey,
    /// The Treasury, the owner of every team Binder.
    pub reserve_recipient: Pubkey,
    /// The Treasury, the payee of the 0.20 SOL and the 40% royalty creator.
    pub treasury: Pubkey,
    pub buyback_wallet: Pubkey,
    pub card_wallet: Pubkey,
    pub base_lamports: u64,
    pub factor_bps: u32,
    pub min_burn_lamports: u64,
    pub max_burn_lamports: u64,
    pub round_threshold: u64,
}

/// Creates `Config`, `State` and `Pending` and funds the pot vault to its rent minimum. Verifies on chain every account it
/// pins: the $PACK mint, the DBC pool, its config and its two vaults.
#[event_cpi]
#[derive(Accounts)]
pub struct InitConfig<'info> {
    /// The upgrade authority of this program. Pays the rent of the accounts and of the pot vault.
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(init, payer = authority, space = 8 + Config::INIT_SPACE, seeds = [CONFIG_SEED], bump)]
    pub config: Box<Account<'info, Config>>,

    #[account(init, payer = authority, space = 8 + State::INIT_SPACE, seeds = [STATE_SEED], bump)]
    pub state: Box<Account<'info, State>>,

    #[account(init, payer = authority, space = 8 + Pending::INIT_SPACE, seeds = [PENDING_SEED], bump)]
    pub pending: Box<Account<'info, Pending>>,

    /// The Binder pot vault: a system-owned PDA with no data, funded here to its rent minimum.
    #[account(mut, seeds = [POT_SEED], bump)]
    pub pot: SystemAccount<'info>,

    /// CHECK: the swapper PDA; only its bump and its associated token addresses are recorded.
    #[account(seeds = [SWAPPER_SEED], bump)]
    pub swapper: UncheckedAccount<'info>,

    /// CHECK: the collection authority PDA; only its bump is recorded.
    #[account(seeds = [AUTHORITY_SEED], bump)]
    pub collection_authority: UncheckedAccount<'info>,

    pub pack_mint: Box<InterfaceAccount<'info, Mint>>,

    /// CHECK: the DBC pool of $PACK, owned by the DBC program, read at its pinned offsets.
    #[account(owner = DBC_PROGRAM @ BinderError::WrongVenueProgram)]
    pub dbc_pool: UncheckedAccount<'info>,

    /// CHECK: the DBC config of the pool, owned by the DBC program, read at its pinned offsets.
    #[account(owner = DBC_PROGRAM @ BinderError::WrongVenueProgram)]
    pub dbc_config: UncheckedAccount<'info>,

    #[account(constraint = dbc_base_vault.mint == pack_mint.key() @ BinderError::VaultNotPinned)]
    pub dbc_base_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(constraint = dbc_quote_vault.mint == WSOL_MINT @ BinderError::VaultNotPinned)]
    pub dbc_quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// CHECK: the Treasury (the payee of the 0.20 SOL and of every team Binder): a system-owned account or one that does not
    /// exist yet, never a program account (review R5). Its key must equal the `treasury` argument.
    #[account(owner = system_program::ID @ BinderError::InvalidConfigKey)]
    pub treasury: UncheckedAccount<'info>,

    /// CHECK: the card wallet: system-owned or not yet existing (review R5). Its key must equal the `card_wallet` argument.
    #[account(owner = system_program::ID @ BinderError::InvalidConfigKey)]
    pub card_wallet: UncheckedAccount<'info>,

    /// CHECK: the buyback wallet: system-owned or not yet existing (review R5). Its key must equal the `buyback_wallet` argument.
    #[account(owner = system_program::ID @ BinderError::InvalidConfigKey)]
    pub buyback_wallet: UncheckedAccount<'info>,

    #[account(
        constraint = crate::loader_state::programdata_address(&this_program.to_account_info())? == Some(program_data.key())
            @ BinderError::NotUpgradeAuthority
    )]
    pub this_program: Program<'info, crate::program::PackBinder>,

    #[account(
        constraint = program_data.upgrade_authority_address == Some(authority.key())
            @ BinderError::NotUpgradeAuthority
    )]
    pub program_data: Account<'info, crate::loader_state::ProgramDataAuthority>,

    pub pack_token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handle_init_config(ctx: Context<InitConfig>, args: InitConfigArgs) -> Result<()> {
    let a = &mut *ctx.accounts;
    let pot_key = a.pot.key();
    for key in [
        args.admin,
        args.reserve_signer,
        args.reserve_recipient,
        args.treasury,
        args.buyback_wallet,
        args.card_wallet,
    ] {
        require!(key != Pubkey::default(), BinderError::InvalidConfigKey);
    }
    // the Treasury is both the payee and the owner of the team Binders (design 3.1: equal at init), and the three payee
    // accounts passed are the arguments (review R5): a payee that is a program account would make every mint fail
    require_keys_eq!(
        args.reserve_recipient,
        args.treasury,
        BinderError::InvalidConfigKey
    );
    require_keys_eq!(
        a.treasury.key(),
        args.treasury,
        BinderError::InvalidConfigKey
    );
    require_keys_eq!(
        a.card_wallet.key(),
        args.card_wallet,
        BinderError::InvalidConfigKey
    );
    require_keys_eq!(
        a.buyback_wallet.key(),
        args.buyback_wallet,
        BinderError::InvalidConfigKey
    );
    // a payee must never be a program PDA of this program
    for key in [
        args.reserve_recipient,
        args.treasury,
        args.buyback_wallet,
        args.card_wallet,
    ] {
        require!(
            key != pot_key
                && key != a.swapper.key()
                && key != a.collection_authority.key()
                && key != a.config.key()
                && key != a.state.key(),
            BinderError::InvalidConfigKey
        );
    }

    // $PACK: its token program owns it, no freeze authority, only metadata extensions
    require_keys_eq!(
        *a.pack_mint.to_account_info().owner,
        a.pack_token_program.key(),
        BinderError::WrongTokenProgram
    );
    validate_pack_mint(&a.pack_mint)?;

    // the DBC pins, read at their pinned offsets
    let pool = parse_dbc_pool(&a.dbc_pool.try_borrow_data()?)?;
    require_keys_eq!(pool.config, a.dbc_config.key(), BinderError::PoolNotPinned);
    require_keys_eq!(
        pool.base_mint,
        a.pack_mint.key(),
        BinderError::MintNotPinned
    );
    require_keys_eq!(
        pool.base_vault,
        a.dbc_base_vault.key(),
        BinderError::VaultNotPinned
    );
    require_keys_eq!(
        pool.quote_vault,
        a.dbc_quote_vault.key(),
        BinderError::VaultNotPinned
    );
    check_dbc_config(&a.dbc_config.try_borrow_data()?)?;

    // the schedule: every tier inside the bounds, the factor not below 1.0
    require!(args.factor_bps >= 10_000, BinderError::InvalidFactor);
    // the bounds stay inside 0.1 to 2.0 SOL (SECURITY, review R5): they cannot change after init
    require!(
        args.min_burn_lamports >= MIN_BURN_FLOOR
            && args.min_burn_lamports <= args.max_burn_lamports
            && args.max_burn_lamports <= MAX_BURN_CEIL,
        BinderError::BurnOutOfBounds
    );
    let tiers = burn_tiers(args.base_lamports, args.factor_bps)
        .ok_or_else(|| error!(BinderError::InvalidFactor))?;
    require!(
        tiers_within_bounds(&tiers, 0, args.min_burn_lamports, args.max_burn_lamports),
        BinderError::BurnOutOfBounds
    );
    require!(
        (MIN_ROUND_THRESHOLD..=MAX_ROUND_THRESHOLD).contains(&args.round_threshold),
        BinderError::InvalidArgument
    );

    // the pot vault holds its rent minimum from the start, so a first, small royalty can never leave it rent-paying
    let rent_min = pot_rent_min()?;
    if a.pot.lamports() < rent_min {
        cpi_lite::system_transfer(
            &a.authority.to_account_info(),
            &a.pot.to_account_info(),
            rent_min - a.pot.lamports(),
            &[],
        )?;
    }

    let swapper_pack_ata = get_associated_token_address_with_program_id(
        &a.swapper.key(),
        &a.pack_mint.key(),
        &a.pack_token_program.key(),
    );
    let swapper_wsol_ata = get_associated_token_address_with_program_id(
        &a.swapper.key(),
        &WSOL_MINT,
        &anchor_spl::token::ID,
    );

    let config = &mut a.config;
    config.admin = args.admin;
    config.reserve_signer = args.reserve_signer;
    config.reserve_recipient = args.reserve_recipient;
    config.treasury = args.treasury;
    config.buyback_wallet = args.buyback_wallet;
    config.card_wallet = args.card_wallet;
    config.pack_mint = a.pack_mint.key();
    config.pack_token_program = a.pack_token_program.key();
    config.swapper_pack_ata = swapper_pack_ata;
    config.swapper_wsol_ata = swapper_wsol_ata;
    config.dbc_config = a.dbc_config.key();
    config.dbc_pool = a.dbc_pool.key();
    config.dbc_base_vault = a.dbc_base_vault.key();
    config.dbc_quote_vault = a.dbc_quote_vault.key();
    config.damm_pool = Pubkey::default();
    config.damm_vault_a = Pubkey::default();
    config.damm_vault_b = Pubkey::default();
    config.collection = Pubkey::default();
    config.art_hash = [0u8; 32];
    config.base_uri = String::new();
    config.phase = PHASE_DBC;
    config.paused = false;
    config.cancelled = false;
    config.team_set = false;
    config.art_locked = false;
    config.team_mask = [0u8; MASK_BYTES];
    config.base_lamports = args.base_lamports;
    config.factor_bps = args.factor_bps;
    config.min_burn_lamports = args.min_burn_lamports;
    config.max_burn_lamports = args.max_burn_lamports;
    config.burn_by_tier = tiers;
    config.round_threshold = args.round_threshold;
    config.config_bump = ctx.bumps.config;
    config.state_bump = ctx.bumps.state;
    config.pending_bump = ctx.bumps.pending;
    config.swapper_bump = ctx.bumps.swapper;
    config.authority_bump = ctx.bumps.collection_authority;
    config.pot_bump = ctx.bumps.pot;

    let state: &mut State = &mut a.state;
    *state = State::new(ctx.bumps.state);

    let pending = &mut a.pending;
    pending.pending_admin = Pubkey::default();
    pending.price_active = false;
    pending.price_base = 0;
    pending.price_factor_bps = 0;
    pending.price_eta = 0;
    pending.rules_active = false;
    pending.rules_eta = 0;
    pending.rules_count = 0;
    pending.rules = [Pubkey::default(); MAX_RULES];
    pending.bump = ctx.bumps.pending;

    let event = ConfigInitialized {
        admin: config.admin,
        reserve_signer: config.reserve_signer,
        reserve_recipient: config.reserve_recipient,
        treasury: config.treasury,
        buyback_wallet: config.buyback_wallet,
        card_wallet: config.card_wallet,
        pack_mint: config.pack_mint,
        dbc_pool: config.dbc_pool,
        base_lamports: config.base_lamports,
        factor_bps: config.factor_bps,
        burn_by_tier: config.burn_by_tier,
        round_threshold: config.round_threshold,
    };
    emit_cpi!(event);
    Ok(())
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitCollectionArgs {
    /// The collection's own metadata URI.
    pub uri: String,
    /// The royalty allow list: the System Program plus the verified marketplaces (design D3), at most 8.
    pub rules: Vec<Pubkey>,
}

/// Creates the Core collection (once) with the Royalties plugin: 500 bps, the pot vault 60 / the Treasury 40, and the
/// allow list; update authority = the `["authority"]` PDA.
#[event_cpi]
#[derive(Accounts)]
pub struct InitCollection<'info> {
    /// The upgrade authority of this program. Pays the collection's rent.
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(mut, seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,

    /// The new collection: a fresh keypair that signs the transaction.
    #[account(mut)]
    pub collection: Signer<'info>,

    /// CHECK: the collection authority PDA, the update authority of the collection.
    #[account(seeds = [AUTHORITY_SEED], bump = config.authority_bump)]
    pub collection_authority: UncheckedAccount<'info>,

    /// CHECK: the pot vault PDA, the first royalty creator.
    #[account(seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: UncheckedAccount<'info>,

    /// CHECK: the Metaplex Core program, a constant address.
    #[account(address = crate::core_cpi::CORE_ID @ BinderError::WrongCoreProgram)]
    pub core_program: UncheckedAccount<'info>,

    #[account(
        constraint = crate::loader_state::programdata_address(&this_program.to_account_info())? == Some(program_data.key())
            @ BinderError::NotUpgradeAuthority
    )]
    pub this_program: Program<'info, crate::program::PackBinder>,

    #[account(
        constraint = program_data.upgrade_authority_address == Some(authority.key())
            @ BinderError::NotUpgradeAuthority
    )]
    pub program_data: Account<'info, crate::loader_state::ProgramDataAuthority>,

    pub system_program: Program<'info, System>,
}

pub fn handle_init_collection(
    ctx: Context<InitCollection>,
    args: InitCollectionArgs,
) -> Result<()> {
    let a = &mut *ctx.accounts;
    require_keys_eq!(
        a.config.collection,
        Pubkey::default(),
        BinderError::CollectionAlreadySet
    );
    require!(valid_rule_set(&args.rules), BinderError::InvalidRuleSet);
    require!(args.uri.len() <= 200, BinderError::InvalidUri);
    let rules_len =
        u8::try_from(args.rules.len()).map_err(|_| error!(BinderError::InvalidRuleSet))?;
    let royalties = core_cpi::royalties(a.pot.key(), a.config.treasury, args.rules);
    core_cpi::create_collection(
        &a.core_program.to_account_info(),
        &a.collection.to_account_info(),
        &a.collection_authority.to_account_info(),
        &a.authority.to_account_info(),
        &a.system_program.to_account_info(),
        args.uri,
        royalties,
    )?;
    a.config.collection = a.collection.key();
    let event = CollectionCreated {
        collection: a.collection.key(),
        royalty_bps: ROYALTY_BPS,
        pot_creator: a.pot.key(),
        treasury_creator: a.config.treasury,
        rule_set_len: rules_len,
    };
    emit_cpi!(event);
    Ok(())
}
