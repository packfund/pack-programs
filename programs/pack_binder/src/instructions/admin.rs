//! The admin instructions (signer must equal `Config.admin`, a Squads vault on mainnet) and the permissionless applies of
//! queued changes. The admin can never move a holder's money: it can set the art and the team numbers before the opening,
//! pin the DAMM v2 pool once, queue a price change or a rule-set change behind the 24 hour timelock, pause public mints,
//! rotate itself, and cancel the Binder before the opening (the pot then goes to the fixed buyback wallet).

use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenAccount;

use crate::constants::*;
use crate::core_cpi;
use crate::errors::BinderError;
use crate::events::*;
use crate::math::{apply_schedule, burn_tiers, team_mask_from, tier_of, tiers_within_bounds};
use crate::pot::free_income;
use crate::state::{Config, Pending, State};
use crate::util::{pot_rent_min, pot_transfer, valid_base_uri, valid_rule_set};
use crate::venue::{derive_damm, parse_damm_pool};

// ---------------------------------------------------------------------------------------------------------------------
// art and team numbers (before the opening)
// ---------------------------------------------------------------------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct SetArt<'info> {
    #[account(constraint = admin.key() == config.admin @ BinderError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
}

/// Sets the metadata folder and the art manifest hash. Only before the opening: the first `mint_reserve` locks both.
pub fn handle_set_art(ctx: Context<SetArt>, base_uri: String, art_hash: [u8; 32]) -> Result<()> {
    let config = &mut ctx.accounts.config;
    require!(!config.art_locked, BinderError::ArtLocked);
    require!(!config.cancelled, BinderError::Cancelled);
    require!(valid_base_uri(&base_uri), BinderError::InvalidUri);
    config.base_uri = base_uri.clone();
    config.art_hash = art_hash;
    emit_cpi!(ArtSet { base_uri, art_hash });
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct SetTeamNumbers<'info> {
    #[account(constraint = admin.key() == config.admin @ BinderError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
}

/// Sets the 30 team numbers once, after the P2-08 draw and before the opening. The draw proof stays off-chain.
pub fn handle_set_team_numbers(ctx: Context<SetTeamNumbers>, numbers: [u16; 30]) -> Result<()> {
    let config = &mut ctx.accounts.config;
    require!(!config.team_set, BinderError::TeamNumbersAlreadySet);
    require!(ctx.accounts.state.opened_at == 0, BinderError::AlreadyOpen);
    require!(!config.cancelled, BinderError::Cancelled);
    config.team_mask =
        team_mask_from(&numbers).ok_or_else(|| error!(BinderError::InvalidTeamNumbers))?;
    config.team_set = true;
    emit_cpi!(TeamNumbersSet { count: TEAM_COUNT });
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// the DAMM v2 pins (after the migration, once)
// ---------------------------------------------------------------------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct SetDamm<'info> {
    #[account(constraint = admin.key() == config.admin @ BinderError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: the DAMM v2 pool the DBC migration created, owned by the DAMM v2 program; its address is re-derived.
    #[account(owner = DAMM_PROGRAM @ BinderError::WrongVenueProgram)]
    pub damm_pool: UncheckedAccount<'info>,
    #[account(
        constraint = damm_vault_a.mint == config.pack_mint @ BinderError::VaultNotPinned,
        constraint = damm_vault_a.owner == DAMM_POOL_AUTHORITY @ BinderError::VaultNotPinned
    )]
    pub damm_vault_a: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        constraint = damm_vault_b.mint == WSOL_MINT @ BinderError::VaultNotPinned,
        constraint = damm_vault_b.owner == DAMM_POOL_AUTHORITY @ BinderError::VaultNotPinned
    )]
    pub damm_vault_b: Box<InterfaceAccount<'info, TokenAccount>>,
}

/// Pins the DAMM v2 pool and its two vaults ONCE, by DERIVING them from the migration config and the two mints (design
/// D9), and checks that the pool exists and agrees. Flips `phase` to DAMM v2. No pool address is an input.
pub fn handle_set_damm(ctx: Context<SetDamm>) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(!a.config.cancelled, BinderError::Cancelled);
    require!(a.config.phase == PHASE_DBC, BinderError::DammAlreadySet);
    require_keys_eq!(
        a.config.damm_pool,
        Pubkey::default(),
        BinderError::DammAlreadySet
    );
    let (pool, vault_a, vault_b) = derive_damm(&a.config.pack_mint);
    require_keys_eq!(a.damm_pool.key(), pool, BinderError::PoolNotPinned);
    require_keys_eq!(a.damm_vault_a.key(), vault_a, BinderError::VaultNotPinned);
    require_keys_eq!(a.damm_vault_b.key(), vault_b, BinderError::VaultNotPinned);
    let view = parse_damm_pool(&a.damm_pool.try_borrow_data()?)?;
    require_keys_eq!(
        view.token_a_mint,
        a.config.pack_mint,
        BinderError::MintNotPinned
    );
    require_keys_eq!(view.token_b_mint, WSOL_MINT, BinderError::MintNotPinned);
    require_keys_eq!(view.token_a_vault, vault_a, BinderError::VaultNotPinned);
    require_keys_eq!(view.token_b_vault, vault_b, BinderError::VaultNotPinned);
    let config = &mut a.config;
    config.damm_pool = pool;
    config.damm_vault_a = vault_a;
    config.damm_vault_b = vault_b;
    config.phase = PHASE_DAMM;
    emit_cpi!(DammSet {
        pool,
        vault_a,
        vault_b
    });
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// the price change (queue, cancel, apply)
// ---------------------------------------------------------------------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct QueuePriceChange<'info> {
    #[account(constraint = admin.key() == config.admin @ BinderError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
    #[account(mut, seeds = [PENDING_SEED], bump = config.pending_bump)]
    pub pending: Box<Account<'info, Pending>>,
}

/// Queues a new base and factor behind the timelock. The tiers above the one being minted must stay inside the bounds.
pub fn handle_queue_price_change(
    ctx: Context<QueuePriceChange>,
    base_lamports: u64,
    factor_bps: u32,
) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(!a.pending.price_active, BinderError::AlreadyQueued);
    require!(factor_bps >= 10_000, BinderError::InvalidFactor);
    let tiers =
        burn_tiers(base_lamports, factor_bps).ok_or_else(|| error!(BinderError::InvalidFactor))?;
    require!(
        tiers_within_bounds(
            &tiers,
            tier_of(a.state.public_minted) + 1,
            a.config.min_burn_lamports,
            a.config.max_burn_lamports
        ),
        BinderError::BurnOutOfBounds
    );
    let eta = Clock::get()?
        .unix_timestamp
        .checked_add(TIMELOCK_SECS)
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    a.pending.price_active = true;
    a.pending.price_base = base_lamports;
    a.pending.price_factor_bps = factor_bps;
    a.pending.price_eta = eta;
    emit_cpi!(PriceChangeQueued {
        base_lamports,
        factor_bps,
        eta
    });
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct CancelPriceChange<'info> {
    #[account(constraint = admin.key() == config.admin @ BinderError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [PENDING_SEED], bump = config.pending_bump)]
    pub pending: Box<Account<'info, Pending>>,
}

pub fn handle_cancel_price_change(ctx: Context<CancelPriceChange>) -> Result<()> {
    let p = &mut ctx.accounts.pending;
    require!(p.price_active, BinderError::NothingQueued);
    let (base_lamports, factor_bps) = (p.price_base, p.price_factor_bps);
    p.price_active = false;
    p.price_base = 0;
    p.price_factor_bps = 0;
    p.price_eta = 0;
    emit_cpi!(PriceChangeCancelled {
        base_lamports,
        factor_bps
    });
    Ok(())
}

/// Permissionless once the eta has passed. Replaces only the tiers above the one being minted.
#[event_cpi]
#[derive(Accounts)]
pub struct ApplyPriceChange<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
    #[account(mut, seeds = [PENDING_SEED], bump = config.pending_bump)]
    pub pending: Box<Account<'info, Pending>>,
}

pub fn handle_apply_price_change(ctx: Context<ApplyPriceChange>) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(a.pending.price_active, BinderError::NothingQueued);
    require!(
        Clock::get()?.unix_timestamp >= a.pending.price_eta,
        BinderError::TimelockActive
    );
    let new = burn_tiers(a.pending.price_base, a.pending.price_factor_bps)
        .ok_or_else(|| error!(BinderError::InvalidFactor))?;
    let from_tier = tier_of(a.state.public_minted) + 1;
    // re-checked: the bounds are Config's and the tier may have moved since the queue
    require!(
        tiers_within_bounds(
            &new,
            from_tier,
            a.config.min_burn_lamports,
            a.config.max_burn_lamports
        ),
        BinderError::BurnOutOfBounds
    );
    let merged = apply_schedule(&a.config.burn_by_tier, &new, a.state.public_minted);
    a.config.burn_by_tier = merged;
    a.config.base_lamports = a.pending.price_base;
    a.config.factor_bps = a.pending.price_factor_bps;
    let (base_lamports, factor_bps) = (a.pending.price_base, a.pending.price_factor_bps);
    a.pending.price_active = false;
    a.pending.price_base = 0;
    a.pending.price_factor_bps = 0;
    a.pending.price_eta = 0;
    emit_cpi!(PriceChangeApplied {
        from_tier: u8::try_from(from_tier.min(TIERS)).unwrap_or(u8::MAX),
        base_lamports,
        factor_bps,
        burn_by_tier: merged,
    });
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// the royalty rule set (queue, cancel, apply)
// ---------------------------------------------------------------------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct QueueRuleSet<'info> {
    #[account(constraint = admin.key() == config.admin @ BinderError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [PENDING_SEED], bump = config.pending_bump)]
    pub pending: Box<Account<'info, Pending>>,
}

/// Queues a new allow list (the System Program required, at most 8). Only the rule set can change: the royalty bps and the
/// creators are constants the program rebuilds itself when it applies the change.
pub fn handle_queue_rule_set(ctx: Context<QueueRuleSet>, rules: Vec<Pubkey>) -> Result<()> {
    let p = &mut ctx.accounts.pending;
    require!(!p.rules_active, BinderError::AlreadyQueued);
    require!(valid_rule_set(&rules), BinderError::InvalidRuleSet);
    let eta = Clock::get()?
        .unix_timestamp
        .checked_add(TIMELOCK_SECS)
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    let mut stored = [Pubkey::default(); MAX_RULES];
    stored[..rules.len()].copy_from_slice(&rules);
    p.rules_active = true;
    p.rules_eta = eta;
    let count = u8::try_from(rules.len()).map_err(|_| error!(BinderError::InvalidRuleSet))?;
    p.rules_count = count;
    p.rules = stored;
    emit_cpi!(RuleSetQueued { count, eta });
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct CancelRuleSet<'info> {
    #[account(constraint = admin.key() == config.admin @ BinderError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [PENDING_SEED], bump = config.pending_bump)]
    pub pending: Box<Account<'info, Pending>>,
}

pub fn handle_cancel_rule_set(ctx: Context<CancelRuleSet>) -> Result<()> {
    let p = &mut ctx.accounts.pending;
    require!(p.rules_active, BinderError::NothingQueued);
    let count = p.rules_count;
    p.rules_active = false;
    p.rules_eta = 0;
    p.rules_count = 0;
    p.rules = [Pubkey::default(); MAX_RULES];
    emit_cpi!(RuleSetCancelled { count });
    Ok(())
}

/// Permissionless once the eta has passed: rebuilds the Royalties plugin (500 bps, the same two creators, the queued list)
/// and writes it to the collection through Core, signed by the `["authority"]` PDA. The caller pays any rent change.
#[event_cpi]
#[derive(Accounts)]
pub struct ApplyRuleSet<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [PENDING_SEED], bump = config.pending_bump)]
    pub pending: Box<Account<'info, Pending>>,
    /// CHECK: the pinned Core collection.
    #[account(mut, address = config.collection @ BinderError::WrongCollection)]
    pub collection: UncheckedAccount<'info>,
    /// CHECK: the collection authority PDA.
    #[account(seeds = [AUTHORITY_SEED], bump = config.authority_bump)]
    pub collection_authority: UncheckedAccount<'info>,
    /// CHECK: the pot vault PDA, the first royalty creator.
    #[account(seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: UncheckedAccount<'info>,
    /// CHECK: the Metaplex Core program, a constant address.
    #[account(address = crate::core_cpi::CORE_ID @ BinderError::WrongCoreProgram)]
    pub core_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handle_apply_rule_set(ctx: Context<ApplyRuleSet>) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(a.pending.rules_active, BinderError::NothingQueued);
    require!(
        Clock::get()?.unix_timestamp >= a.pending.rules_eta,
        BinderError::TimelockActive
    );
    let count = a.pending.rules_count;
    let rules: Vec<Pubkey> = a.pending.rules[..usize::from(count)].to_vec();
    require!(valid_rule_set(&rules), BinderError::InvalidRuleSet);
    let royalties = core_cpi::royalties(a.pot.key(), a.config.treasury, rules);
    core_cpi::update_collection_royalties(
        &a.core_program.to_account_info(),
        &a.collection.to_account_info(),
        &a.payer.to_account_info(),
        &a.collection_authority.to_account_info(),
        &a.system_program.to_account_info(),
        royalties,
        a.config.authority_bump,
    )?;
    a.pending.rules_active = false;
    a.pending.rules_eta = 0;
    a.pending.rules_count = 0;
    a.pending.rules = [Pubkey::default(); MAX_RULES];
    emit_cpi!(RuleSetApplied { count });
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// pause and admin rotation
// ---------------------------------------------------------------------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct SetPaused<'info> {
    #[account(constraint = admin.key() == config.admin @ BinderError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
}

/// Stops (or resumes) PUBLIC mints only. Claims, syncs, genesis, withdrawals and team mints keep working.
pub fn handle_set_paused(ctx: Context<SetPaused>, paused: bool) -> Result<()> {
    ctx.accounts.config.paused = paused;
    emit_cpi!(PausedSet { paused });
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct ProposeAdmin<'info> {
    #[account(constraint = admin.key() == config.admin @ BinderError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [PENDING_SEED], bump = config.pending_bump)]
    pub pending: Box<Account<'info, Pending>>,
}

pub fn handle_propose_admin(ctx: Context<ProposeAdmin>, new_admin: Pubkey) -> Result<()> {
    require!(
        new_admin != Pubkey::default(),
        BinderError::InvalidConfigKey
    );
    ctx.accounts.pending.pending_admin = new_admin;
    emit_cpi!(AdminProposed { new_admin });
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct AcceptAdmin<'info> {
    #[account(constraint = new_admin.key() == pending.pending_admin @ BinderError::NotAdmin)]
    pub new_admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [PENDING_SEED], bump = config.pending_bump)]
    pub pending: Box<Account<'info, Pending>>,
}

pub fn handle_accept_admin(ctx: Context<AcceptAdmin>) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(
        a.pending.pending_admin != Pubkey::default(),
        BinderError::NotAdmin
    );
    a.config.admin = a.pending.pending_admin;
    a.pending.pending_admin = Pubkey::default();
    let admin = a.config.admin;
    emit_cpi!(AdminAccepted { admin });
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// cancel (only before the opening) and the sweep that follows it
// ---------------------------------------------------------------------------------------------------------------------

#[event_cpi]
#[derive(Accounts)]
pub struct CancelBinder<'info> {
    #[account(constraint = admin.key() == config.admin @ BinderError::NotAdmin)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
    #[account(mut, seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: SystemAccount<'info>,
    #[account(mut, address = config.buyback_wallet @ BinderError::InvalidConfigKey)]
    pub buyback_wallet: SystemAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// The Binder is cancelled: only before the first `mint_reserve`. Mints are refused for good and the whole free pot goes to
/// the fixed buyback wallet (the keeper burns $PACK with the buyback pattern). After the opening the pot belongs to the
/// Binders and this is refused.
pub fn handle_cancel_binder(ctx: Context<CancelBinder>) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(!a.config.cancelled, BinderError::Cancelled);
    require!(a.state.opened_at == 0, BinderError::AlreadyOpen);
    a.config.cancelled = true;
    let rent_min = pot_rent_min()?;
    let swept = free_income(a.pot.to_account_info().lamports(), rent_min, &a.state)?;
    pot_transfer(
        &a.system_program,
        &a.pot.to_account_info(),
        &a.buyback_wallet.to_account_info(),
        swept,
        a.config.pot_bump,
    )?;
    emit_cpi!(BinderCancelled { swept });
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct SweepToBuyback<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
    #[account(mut, seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: SystemAccount<'info>,
    #[account(mut, address = config.buyback_wallet @ BinderError::InvalidConfigKey)]
    pub buyback_wallet: SystemAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// Permissionless, valid only after a cancel: later inflows to the pot (a royalty, a keeper sweep) take the same path.
pub fn handle_sweep_to_buyback(ctx: Context<SweepToBuyback>) -> Result<()> {
    let a = &ctx.accounts;
    require!(a.config.cancelled, BinderError::NothingToClaim);
    let rent_min = pot_rent_min()?;
    let amount = free_income(a.pot.to_account_info().lamports(), rent_min, &a.state)?;
    require!(amount > 0, BinderError::NothingToClaim);
    pot_transfer(
        &a.system_program,
        &a.pot.to_account_info(),
        &a.buyback_wallet.to_account_info(),
        amount,
        a.config.pot_bump,
    )?;
    emit_cpi!(SweptToBuyback { amount });
    Ok(())
}
