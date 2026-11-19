//! The holders' instructions: `claim_vault` (the accumulator share and the genesis share, in lamports) and the two vault
//! withdrawals (tokens and Core prizes). Only the CURRENT owner of the Binder's Core asset can call them: ownership is read
//! from the asset on every call, so a sale moves the unclaimed shares and the vault contents to the buyer.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use crate::constants::*;
use crate::core_cpi::{parse_base_asset, read_binder_asset, transfer_prize};
use crate::cpi_lite;
use crate::errors::BinderError;
use crate::events::{VaultClaimed, VaultWithdrawn};
use crate::pot::{apply_claim, check_identity, claim_amounts, genesis_pending, sync_pot_state};
use crate::state::{Binder, Config, State};
use crate::util::{pot_rent_min, pot_transfer};

#[event_cpi]
#[derive(Accounts)]
pub struct ClaimVault<'info> {
    /// The current owner of the Binder asset: signs and receives.
    #[account(mut)]
    pub owner: Signer<'info>,
    /// CHECK: the Binder's Core asset; owner, key byte and collection are read in the handler.
    pub asset: UncheckedAccount<'info>,
    #[account(mut, seeds = [BINDER_SEED, asset.key().as_ref()], bump = binder.bump)]
    pub binder: Box<Account<'info, Binder>>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,
    #[account(mut, seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: SystemAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handle_claim_vault(ctx: Context<ClaimVault>) -> Result<()> {
    let a = &mut *ctx.accounts;
    // a forfeited Binder was burned: nothing of it can be claimed (its asset is gone anyway)
    require!(!a.binder.forfeited, BinderError::AlreadyForfeited);
    let view = read_binder_asset(&a.asset.to_account_info(), &a.config.collection)?;
    require_keys_eq!(view.owner, a.owner.key(), BinderError::NotAssetOwner);
    require_keys_eq!(a.binder.asset, a.asset.key(), BinderError::NotAssetOwner);

    // the latest income first, so the claim is up to date
    let rent_min = pot_rent_min()?;
    let events = sync_pot_state(
        &mut a.state,
        a.pot.to_account_info().lamports(),
        rent_min,
        a.config.round_threshold,
    )?;

    let pending = genesis_pending(
        &a.state,
        a.binder.genesis_eligible,
        a.binder.genesis_claimed,
    );
    let amounts = claim_amounts(&a.state, a.binder.acc_snapshot, pending)?;
    let total = amounts.total()?;
    require!(total > 0, BinderError::NothingToClaim);
    apply_claim(&mut a.state, &amounts)?;
    a.binder.acc_snapshot = amounts.new_snapshot;
    if pending {
        a.binder.genesis_claimed = true;
    }
    a.binder.sol_claimed = a
        .binder
        .sol_claimed
        .checked_add(total)
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    pot_transfer(
        &a.system_program,
        &a.pot.to_account_info(),
        &a.owner.to_account_info(),
        total,
        a.config.pot_bump,
    )?;
    check_identity(a.pot.to_account_info().lamports(), rent_min, &a.state)?;

    let claimed = VaultClaimed {
        asset: a.asset.key(),
        owner: a.owner.key(),
        acc_share: amounts.acc_share,
        genesis_share: amounts.genesis_share,
    };
    if let Some(e) = events.synced {
        emit_cpi!(e);
    }
    for e in events.closed {
        emit_cpi!(e);
    }
    emit_cpi!(claimed);
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct VaultWithdrawToken<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    /// CHECK: the Binder's Core asset.
    pub asset: UncheckedAccount<'info>,
    #[account(seeds = [BINDER_SEED, asset.key().as_ref()], bump = binder.bump)]
    pub binder: Box<Account<'info, Binder>>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: the vault PDA of this Binder, the authority of its prize token accounts.
    #[account(seeds = [VAULT_SEED, asset.key().as_ref()], bump)]
    pub vault: UncheckedAccount<'info>,
    #[account(
        constraint = *mint.to_account_info().owner == token_program.key() @ BinderError::WrongTokenProgram
    )]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        constraint = vault_token.owner == vault.key() @ BinderError::WrongTokenAccount,
        constraint = vault_token.mint == mint.key() @ BinderError::WrongTokenAccount
    )]
    pub vault_token: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = owner_token.owner == owner.key() @ BinderError::WrongTokenAccount,
        constraint = owner_token.mint == mint.key() @ BinderError::WrongTokenAccount
    )]
    pub owner_token: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

/// Moves `amount` of a token from the Binder's vault to the owner's token account. The vault PDA signs.
pub fn handle_vault_withdraw_token(ctx: Context<VaultWithdrawToken>, amount: u64) -> Result<()> {
    let a = &ctx.accounts;
    let view = read_binder_asset(&a.asset.to_account_info(), &a.config.collection)?;
    require_keys_eq!(view.owner, a.owner.key(), BinderError::NotAssetOwner);
    require_keys_eq!(a.binder.asset, a.asset.key(), BinderError::NotAssetOwner);
    require!(amount > 0, BinderError::ZeroAmount);
    require!(amount <= a.vault_token.amount, BinderError::NothingToClaim);
    let asset_key = a.asset.key();
    let bump = [ctx.bumps.vault];
    let seeds: &[&[u8]] = &[VAULT_SEED, asset_key.as_ref(), &bump];
    cpi_lite::token_transfer_checked(
        &a.token_program.key(),
        &a.vault_token.to_account_info(),
        &a.mint.to_account_info(),
        &a.owner_token.to_account_info(),
        &a.vault.to_account_info(),
        amount,
        a.mint.decimals,
        &[seeds],
    )?;
    let event = VaultWithdrawn {
        asset: asset_key,
        owner: a.owner.key(),
        kind: 0,
        item: a.mint.key(),
        amount,
    };
    emit_cpi!(event);
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct VaultWithdrawCore<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    /// CHECK: the Binder's Core asset.
    pub asset: UncheckedAccount<'info>,
    #[account(seeds = [BINDER_SEED, asset.key().as_ref()], bump = binder.bump)]
    pub binder: Box<Account<'info, Binder>>,
    #[account(seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: the vault PDA of this Binder: the owner of the prize and the signer of its transfer.
    #[account(seeds = [VAULT_SEED, asset.key().as_ref()], bump)]
    pub vault: UncheckedAccount<'info>,
    /// CHECK: the prize: a Core asset owned by the vault PDA; read in the handler, moved by Core.
    #[account(mut)]
    pub prize: UncheckedAccount<'info>,
    /// CHECK: the prize's own collection, if it has one (the Core program id when it has none).
    pub prize_collection: UncheckedAccount<'info>,
    /// CHECK: the Metaplex Core program, a constant address.
    #[account(address = crate::core_cpi::CORE_ID @ BinderError::WrongCoreProgram)]
    pub core_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

/// Moves a Core prize owned by the Binder's vault to the Binder's owner (Core `TransferV1`, the vault PDA signs). It fails
/// if the prize's own plugins block the transfer; the prize then stays in the vault.
pub fn handle_vault_withdraw_core(ctx: Context<VaultWithdrawCore>) -> Result<()> {
    let a = &ctx.accounts;
    let view = read_binder_asset(&a.asset.to_account_info(), &a.config.collection)?;
    require_keys_eq!(view.owner, a.owner.key(), BinderError::NotAssetOwner);
    require_keys_eq!(a.binder.asset, a.asset.key(), BinderError::NotAssetOwner);
    require_keys_eq!(
        *a.prize.owner,
        crate::core_cpi::CORE_ID,
        BinderError::WrongCoreProgram
    );
    let prize = parse_base_asset(&a.prize.try_borrow_data()?)
        .ok_or_else(|| error!(BinderError::WrongTokenAccount))?;
    require_keys_eq!(prize.owner, a.vault.key(), BinderError::WrongTokenAccount);
    let asset_key = a.asset.key();
    let bump = [ctx.bumps.vault];
    let seeds: &[&[u8]] = &[VAULT_SEED, asset_key.as_ref(), &bump];
    let prize_collection = a.prize_collection.to_account_info();
    let none = *prize_collection.key == crate::core_cpi::CORE_ID;
    transfer_prize(
        &a.core_program.to_account_info(),
        &a.prize.to_account_info(),
        if none { None } else { Some(&prize_collection) },
        &a.owner.to_account_info(),
        &a.vault.to_account_info(),
        &a.owner.to_account_info(),
        &a.system_program.to_account_info(),
        seeds,
    )?;
    let event = VaultWithdrawn {
        asset: asset_key,
        owner: a.owner.key(),
        kind: 1,
        item: a.prize.key(),
        amount: 1,
    };
    emit_cpi!(event);
    Ok(())
}
