//! `mint_reserve`: the 30 team Binders, free, signed by the owner wallet, minted to the Treasury. The first call opens the
//! mint (`opened_at`), locks the art and locks the genesis amount. No payment, no swap, no burn.

use anchor_lang::prelude::*;

use crate::constants::*;
use crate::core_cpi;
use crate::cpi_lite;
use crate::errors::BinderError;
use crate::events::{MintOpened, PublicMintsOpened, ReserveMinted};
use crate::math::bit_get;
use crate::pot::{
    check_identity, genesis_window_end, lock_genesis, register_mint, require_synced, sync_pot_state,
};
use crate::state::{Binder, Config, State};
use crate::util::{binder_name, binder_uri, pot_rent_min};

/// Creates (or completes the funding of) a `Binder` PDA by hand and writes its record. Used where the accounts come as
/// `remaining_accounts` (a batch of team mints); a public mint uses Anchor's `init`. The PDA is re-derived here, so the
/// address cannot be anything but `["binder", asset]`.
pub fn create_binder_record<'info>(
    // the system program account stays a parameter: the CPIs below need it among the accounts of the instruction
    _system_program: &Program<'info, System>,
    payer: &AccountInfo<'info>,
    binder_info: &AccountInfo<'info>,
    record: &Binder,
    program_id: &Pubkey,
) -> Result<()> {
    let (expected, bump) =
        Pubkey::find_program_address(&[BINDER_SEED, record.asset.as_ref()], program_id);
    require_keys_eq!(binder_info.key(), expected, BinderError::PinMismatch);
    require!(binder_info.data_is_empty(), BinderError::NumberTaken);
    require_keys_eq!(
        *binder_info.owner,
        system_program::ID,
        BinderError::NumberTaken
    );
    let space = 8 + Binder::INIT_SPACE;
    let space_u64 = u64::try_from(space).map_err(|_| error!(BinderError::MathOverflow))?;
    let rent = Rent::get()?.minimum_balance(space);
    let bump_seed = [bump];
    let seeds: &[&[u8]] = &[BINDER_SEED, record.asset.as_ref(), &bump_seed];
    if binder_info.lamports() == 0 {
        cpi_lite::system_create_account(payer, binder_info, rent, space_u64, program_id, &[seeds])?;
    } else {
        // someone sent lamports to the address first: top it up, then allocate and assign (what Anchor's `init` does)
        let missing = rent.saturating_sub(binder_info.lamports());
        if missing > 0 {
            cpi_lite::system_transfer(payer, binder_info, missing, &[])?;
        }
        cpi_lite::system_allocate(binder_info, space_u64, &[seeds])?;
        cpi_lite::system_assign(binder_info, program_id, &[seeds])?;
    }
    let mut data = binder_info.try_borrow_mut_data()?;
    let mut dst: &mut [u8] = &mut data;
    record.try_serialize(&mut dst)?;
    Ok(())
}

#[event_cpi]
#[derive(Accounts)]
pub struct MintReserve<'info> {
    /// The owner wallet: pays the rents of the Binders and signs.
    #[account(mut, constraint = signer.key() == config.reserve_signer @ BinderError::NotReserveSigner)]
    pub signer: Signer<'info>,

    #[account(mut, seeds = [CONFIG_SEED], bump = config.config_bump)]
    pub config: Box<Account<'info, Config>>,

    #[account(mut, seeds = [STATE_SEED], bump = config.state_bump)]
    pub state: Box<Account<'info, State>>,

    /// The pot vault (read for the genesis amount and the sync).
    #[account(seeds = [POT_SEED], bump = config.pot_bump)]
    pub pot: SystemAccount<'info>,

    /// CHECK: the collection authority PDA, which signs the Core creation.
    #[account(seeds = [AUTHORITY_SEED], bump = config.authority_bump)]
    pub collection_authority: UncheckedAccount<'info>,

    /// CHECK: the pinned Core collection.
    #[account(mut, address = config.collection @ BinderError::WrongCollection)]
    pub collection: UncheckedAccount<'info>,

    /// CHECK: the Treasury, fixed in Config: the owner of every team Binder.
    #[account(address = config.reserve_recipient @ BinderError::InvalidConfigKey)]
    pub recipient: UncheckedAccount<'info>,

    /// CHECK: the Metaplex Core program, a constant address.
    #[account(address = crate::core_cpi::CORE_ID @ BinderError::WrongCoreProgram)]
    pub core_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    // remaining_accounts: for each number, the new asset (a fresh keypair that signed the transaction) and its `Binder` PDA
}

pub fn handle_mint_reserve<'info>(
    ctx: Context<'info, MintReserve<'info>>,
    numbers: Vec<u16>,
) -> Result<()> {
    let program_id = *ctx.program_id;
    let remaining = ctx.remaining_accounts;
    let a = &mut *ctx.accounts;
    require!(!a.config.cancelled, BinderError::Cancelled);
    require!(a.config.team_set, BinderError::TeamNumbersNotSet);
    require!(!a.config.base_uri.is_empty(), BinderError::ArtNotSet);
    require!(
        !numbers.is_empty() && numbers.len() <= MAX_RESERVE_PER_CALL,
        BinderError::InvalidArgument
    );
    require!(
        remaining.len() == numbers.len() * 2,
        BinderError::InvalidArgument
    );
    require!(
        usize::from(a.state.reserve_minted) + numbers.len() <= usize::from(TEAM_COUNT),
        BinderError::ReserveComplete
    );
    // every number is a team number (a public number can never be minted here) and not yet minted
    for n in &numbers {
        require!(bit_get(&a.config.team_mask, *n), BinderError::NotTeamNumber);
        require!(!bit_get(&a.state.minted_mask, *n), BinderError::NumberTaken);
    }

    let now = Clock::get()?.unix_timestamp;
    let rent_min = pot_rent_min()?;
    let pot_info = a.pot.to_account_info();
    let mut opened: Option<MintOpened> = None;
    if a.state.opened_at == 0 {
        // the opening: the art is locked and the pot's free balance is locked as the genesis amount (nothing is owed yet)
        a.state.opened_at = now;
        a.config.art_locked = true;
        let locked = lock_genesis(&mut a.state, pot_info.lamports(), rent_min)?;
        opened = Some(MintOpened {
            opened_at: now,
            genesis_locked: locked,
        });
    }
    // income that arrived since the last sync goes to the Binders that exist, before the new ones are counted
    let events = sync_pot_state(
        &mut a.state,
        pot_info.lamports(),
        rent_min,
        a.config.round_threshold,
    )?;
    // a Binder is counted only when no income is left to realize later (review R1): sync_pot in front as needed
    require_synced(pot_info.lamports(), rent_min, &a.state)?;

    let core_program = a.core_program.to_account_info();
    let collection = a.collection.to_account_info();
    let authority = a.collection_authority.to_account_info();
    let signer = a.signer.to_account_info();
    let recipient = a.recipient.to_account_info();
    let system_program_info = a.system_program.to_account_info();
    let mut minted_events: Vec<ReserveMinted> = Vec::with_capacity(numbers.len());
    for (i, number) in numbers.iter().enumerate() {
        let asset = &remaining[2 * i];
        let binder_info = &remaining[2 * i + 1];
        require!(
            asset.is_signer && asset.is_writable,
            BinderError::PinMismatch
        );
        let registered = register_mint(&mut a.state, *number, true)?;
        core_cpi::create_asset(
            &core_program,
            asset,
            &collection,
            &authority,
            &signer,
            &recipient,
            &system_program_info,
            binder_name(*number),
            binder_uri(&a.config.base_uri, *number),
            a.config.authority_bump,
        )?;
        let (_, bump) =
            Pubkey::find_program_address(&[BINDER_SEED, asset.key.as_ref()], &program_id);
        let record = Binder {
            asset: asset.key(),
            number: *number,
            seq: registered.seq,
            is_team: true,
            genesis_eligible: registered.genesis_eligible,
            genesis_claimed: false,
            forfeited: false,
            acc_snapshot: a.state.acc_per_binder,
            sol_claimed: 0,
            bump,
        };
        create_binder_record(
            &a.system_program,
            &signer,
            binder_info,
            &record,
            &program_id,
        )?;
        minted_events.push(ReserveMinted {
            number: *number,
            seq: registered.seq,
            asset: asset.key(),
            owner: a.config.reserve_recipient,
        });
    }
    // the 30th team Binder opens the public mints and starts the 24 hour genesis window (review R3); the first call of the
    // team batch only locked the art and the genesis amount (`opened_at`)
    let mut public_open: Option<PublicMintsOpened> = None;
    if a.state.reserve_minted == TEAM_COUNT && a.state.public_open_at == 0 {
        a.state.public_open_at = now;
        public_open = Some(PublicMintsOpened {
            public_open_at: now,
            genesis_window_ends: genesis_window_end(&a.state)?,
        });
    }
    check_identity(pot_info.lamports(), rent_min, &a.state)?;

    if let Some(e) = opened {
        emit_cpi!(e);
    }
    if let Some(e) = public_open {
        emit_cpi!(e);
    }
    if let Some(e) = events.synced {
        emit_cpi!(e);
    }
    for e in events.closed {
        emit_cpi!(e);
    }
    for e in minted_events {
        emit_cpi!(e);
    }
    Ok(())
}
