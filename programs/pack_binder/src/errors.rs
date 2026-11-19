//! Error codes of `pack_binder`. Anchor numbers them from 6000 in declaration order; append new codes at the END so
//! existing numbers stay stable for clients and the indexer. (The P2-00 proof program's codes are not reused.)

use anchor_lang::prelude::*;

#[error_code]
pub enum BinderError {
    // ---- authority -------------------------------------------------------------------------------------------
    #[msg("The signer is not the upgrade authority of this program")]
    NotUpgradeAuthority,
    #[msg("The signer is not the admin set in Config")]
    NotAdmin,
    #[msg("The signer is not the reserve signer set in Config")]
    NotReserveSigner,
    #[msg("The signer does not own this Binder")]
    NotAssetOwner,
    #[msg("The signer is not the card wallet set in Config")]
    NotCardWallet,

    // ---- config and pins -------------------------------------------------------------------------------------
    #[msg("A config key is the default (all zero) key or is not allowed")]
    InvalidConfigKey,
    #[msg("A pinned account does not match what the venue or the mint says")]
    PinMismatch,
    #[msg("The mint is not the pinned $PACK mint")]
    MintNotPinned,
    #[msg("The pool or config is not the pinned one")]
    PoolNotPinned,
    #[msg("The vault is not the pinned one")]
    VaultNotPinned,
    #[msg("The venue program is not the pinned program")]
    WrongVenueProgram,
    #[msg("A pool authority, event authority or program account is not the expected constant")]
    WrongAuthority,
    #[msg("A token account is not the swapper's pinned associated account, or does not belong to the signer")]
    WrongTokenAccount,
    #[msg("The token program does not own the account or is not the pinned one")]
    WrongTokenProgram,

    // ---- state machine ---------------------------------------------------------------------------------------
    #[msg("This venue is not the active one (see Config.phase)")]
    WrongPhase,
    #[msg("Public mints are paused")]
    Paused,
    #[msg("The Binder is cancelled")]
    Cancelled,
    #[msg("The mint is not open yet: no team Binder has been minted")]
    NotOpen,
    #[msg("The mint is already open")]
    AlreadyOpen,
    #[msg("Public mints and genesis wait until all 30 team Binders exist")]
    ReserveIncomplete,
    #[msg("All 30 team Binders already exist")]
    ReserveComplete,
    #[msg("Every public Binder has been minted")]
    SoldOut,

    // ---- numbering -------------------------------------------------------------------------------------------
    #[msg("This number is not a team number")]
    NotTeamNumber,
    #[msg("A team number cannot go to a public mint")]
    TeamNumberNotMinted,
    #[msg("This number already exists")]
    NumberTaken,
    #[msg("The team numbers have not been set")]
    TeamNumbersNotSet,
    #[msg("The team numbers are already set")]
    TeamNumbersAlreadySet,
    #[msg("The team numbers are not 30 distinct numbers from #2 to #100 that include #15, #67, #86 and #100")]
    InvalidTeamNumbers,

    // ---- metadata --------------------------------------------------------------------------------------------
    #[msg("The art is locked: the mint is open")]
    ArtLocked,
    #[msg("The art (base URI) has not been set")]
    ArtNotSet,
    #[msg("The base URI is not 8 to 96 ASCII bytes ending with a slash")]
    InvalidUri,

    // ---- swap ------------------------------------------------------------------------------------------------
    #[msg("An amount is zero")]
    ZeroAmount,
    #[msg("The swap delivered less than min_out")]
    SlippageExceeded,
    #[msg("The swap delivered no $PACK")]
    NothingReceived,
    #[msg("The venue spent no wSOL")]
    NothingSpent,
    #[msg("The venue spent more wSOL than the buyer offered")]
    Overspent,
    #[msg("The burn did not bring the $PACK balance back")]
    BurnMismatch,
    #[msg("The burn amount is outside the bounds of Config")]
    BurnOutOfBounds,

    // ---- queued changes --------------------------------------------------------------------------------------
    #[msg("The factor is below 1.0 or the schedule overflows")]
    InvalidFactor,
    #[msg("Nothing is queued")]
    NothingQueued,
    #[msg("A change is already queued")]
    AlreadyQueued,
    #[msg("The timelock of the queued change has not passed")]
    TimelockActive,
    #[msg(
        "The rule set is empty of the System Program, has too many entries or repeats a program"
    )]
    InvalidRuleSet,

    // ---- genesis, claims, pot --------------------------------------------------------------------------------
    #[msg("Genesis is already shared")]
    GenesisAlreadyShared,
    #[msg("The 24 hour genesis window has not passed")]
    GenesisWindowOpen,
    #[msg("There is nothing to claim or withdraw")]
    NothingToClaim,
    #[msg("The pot does not hold the amount above its rent minimum")]
    InsufficientPot,
    #[msg("An internal invariant of the pot accounting is broken")]
    InvariantBroken,
    #[msg("Arithmetic overflow")]
    MathOverflow,

    // ---- venues and collection -------------------------------------------------------------------------------
    #[msg("The DAMM v2 pins are already set")]
    DammAlreadySet,
    #[msg("The DAMM v2 pool or its vaults do not exist yet or do not match")]
    DammNotReady,
    #[msg("The collection is already created")]
    CollectionAlreadySet,
    #[msg("The collection is not the pinned one or is not a Binder asset")]
    WrongCollection,
    #[msg("The Core program account is not the Metaplex Core program")]
    WrongCoreProgram,

    // ---- arguments -------------------------------------------------------------------------------------------
    #[msg("An argument is out of its range")]
    InvalidArgument,

    // ---- sync (review R1) -----------------------------------------------------------------------------------------
    #[msg("Pot income is not fully synced: put sync_pot instructions before this one")]
    SyncIncomplete,

    // ---- burned Binders (owner decision 2026-11-09, review R6) -----------------------------------------------------
    #[msg("The asset is not a burned Core asset")]
    AssetNotBurned,
    #[msg("This Binder was already forfeited")]
    AlreadyForfeited,
}
