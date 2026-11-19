//! `forfeit_burned`: a Binder whose Core asset was burned can never be claimed, so anyone may take it out of the active set and
//! return its unclaimed shares to the pot as income for the Binders that remain (owner decision 2026-11-09, review R6; the
//! keeper runs it automatically, P2-03). It moves no lamport: the reserve shrinks, so the same pot lamports become free income.

use anchor_lang::prelude::*;

use crate::constants::*;
use crate::core_cpi::is_burned_asset;
use crate::errors::BinderError;
use crate::events::BinderForfeited;
use crate::pot::{check_identity, forfeit, sync_pot_state};
use crate::state::{Binder, Config, State};
use crate::util::pot_rent_min;

#[event_cpi]
#[derive(Accounts)]
pub struct ForfeitBurned<'info> {
    /// CHECK: the tombstone Core leaves after `BurnV1` (an account of the Core program, one byte, `Key::Uninitialized`), checked in
    /// the handler. Its address ties it to the `Binder` record by the seeds below: only an asset the program minted has one.
    pub asset: UncheckedAccount<'info>,
    #[account(mut, seeds = [BINDER_SEED, asset.key().as_ref()], bump = binder.bump)]
    pub binder: Box<Account<'info, Binder>>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
    #[account(seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: SystemAccount<'info>,
}

pub fn handle_forfeit_burned(ctx: Context<ForfeitBurned>) -> Result<()> {
    let a = &mut *ctx.accounts;
    require!(!a.binder.forfeited, BinderError::AlreadyForfeited);
    require!(
        is_burned_asset(&a.asset.to_account_info())?,
        BinderError::AssetNotBurned
    );
    require_keys_eq!(a.binder.asset, a.asset.key(), BinderError::AssetNotBurned);

    // Order (the burned Binder must not be paid any part of what it releases):
    // 1. sync: income that arrived before this call is credited to the active Binders, the burned one still among them, so
    //    its accumulator share is current. It is released in step 2 together with the rest of its share, never paid.
    //    A sync that stops at its step cap leaves free income that a later sync credits to the Binders active THEN.
    // 2. release its unclaimed accumulator and genesis shares and drop it from the active count. The released amount is free
    //    income from here on and only the next sync credits it, with the burned Binder already out of the divisor.
    let rent_min = pot_rent_min()?;
    let pot_lamports = a.pot.to_account_info().lamports();
    let events = sync_pot_state(
        &mut a.state,
        pot_lamports,
        rent_min,
        a.config.round_threshold,
    )?;
    let released = forfeit(
        &mut a.state,
        a.binder.acc_snapshot,
        a.binder.genesis_eligible,
        a.binder.genesis_claimed,
    )?;
    a.binder.forfeited = true;
    check_identity(pot_lamports, rent_min, &a.state)?;

    let forfeited = BinderForfeited {
        number: a.binder.number,
        asset: a.asset.key(),
        released_acc: u64::try_from(released.released_acc_scaled / SCALE).unwrap_or(u64::MAX),
        released_genesis: released.released_genesis,
        active: a.state.active,
    };
    if let Some(e) = events.synced {
        emit_cpi!(e);
    }
    for e in events.closed {
        emit_cpi!(e);
    }
    emit_cpi!(forfeited);
    Ok(())
}
