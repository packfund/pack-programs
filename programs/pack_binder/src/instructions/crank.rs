//! The permissionless cranks (`sync_pot`, `finalize_genesis`) and the card wallet's `withdraw_card_budget`.

use anchor_lang::prelude::*;

use crate::constants::*;
use crate::errors::BinderError;
use crate::events::{CardBudgetWithdrawn, GenesisShared};
use crate::pot::{check_identity, genesis_window_end, share_genesis, sync_pot_state};
use crate::state::{Config, State};
use crate::util::{pot_rent_min, pot_transfer};

#[event_cpi]
#[derive(Accounts)]
pub struct SyncPot<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
    #[account(seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: SystemAccount<'info>,
}

/// Anyone can call it: realizes the pot's free income for the Binders that exist (design 5.3). A no-op when there is
/// nothing to realize or no Binder yet.
pub fn handle_sync_pot(ctx: Context<SyncPot>) -> Result<()> {
    let a = &mut *ctx.accounts;
    let rent_min = pot_rent_min()?;
    let lamports = a.pot.to_account_info().lamports();
    let events = sync_pot_state(&mut a.state, lamports, rent_min, a.config.round_threshold)?;
    check_identity(lamports, rent_min, &a.state)?;
    if let Some(e) = events.synced {
        emit_cpi!(e);
    }
    for e in events.closed {
        emit_cpi!(e);
    }
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct FinalizeGenesis<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
    #[account(seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: SystemAccount<'info>,
}

/// Shares genesis 24 hours after the public mints opened (the 30th team Binder, review R3) when the 100th public mint and a
/// mint after the window have not done it, among the eligible Binders minted at that moment. A public mint after this is NOT
/// eligible, whatever its number. Needs all 30 team Binders (design D15).
pub fn handle_finalize_genesis(ctx: Context<FinalizeGenesis>) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(a.state.opened_at > 0, BinderError::NotOpen);
    require!(!a.state.genesis_shared, BinderError::GenesisAlreadyShared);
    require!(
        a.state.reserve_minted == TEAM_COUNT,
        BinderError::ReserveIncomplete
    );
    require!(
        Clock::get()?.unix_timestamp >= genesis_window_end(&a.state)?,
        BinderError::GenesisWindowOpen
    );
    let g = share_genesis(&mut a.state)?;
    check_identity(
        a.pot.to_account_info().lamports(),
        pot_rent_min()?,
        &a.state,
    )?;
    emit_cpi!(GenesisShared {
        eligible: g.eligible,
        per_binder: g.per_binder,
        remainder: g.remainder,
        trigger: 1,
    });
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct WithdrawCardBudget<'info> {
    /// The card wallet fixed in Config: the signer and the destination.
    #[account(mut, constraint = card_wallet.key() == config.card_wallet @ BinderError::NotCardWallet)]
    pub card_wallet: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
    #[account(mut, seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: SystemAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// Sends a closed card budget to the card wallet, which buys the card and runs the raffle off-chain (P2-03). The
/// destination is the signer and equals the fixed Config address: nothing can be redirected.
pub fn handle_withdraw_card_budget(ctx: Context<WithdrawCardBudget>, amount: u64) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(amount > 0, BinderError::ZeroAmount);
    require!(
        amount <= a.state.card_withdrawable,
        BinderError::NothingToClaim
    );
    a.state.card_withdrawable = a
        .state
        .card_withdrawable
        .checked_sub(amount)
        .ok_or_else(|| error!(BinderError::InvariantBroken))?;
    pot_transfer(
        &a.system_program,
        &a.pot.to_account_info(),
        &a.card_wallet.to_account_info(),
        amount,
        a.config.pot_bump,
    )?;
    check_identity(
        a.pot.to_account_info().lamports(),
        pot_rent_min()?,
        &a.state,
    )?;
    let remaining = a.state.card_withdrawable;
    emit_cpi!(CardBudgetWithdrawn { amount, remaining });
    Ok(())
}
