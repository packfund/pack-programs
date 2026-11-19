//! Constants of `pack_binder` (docs/validation/P2-01_BINDER_DESIGN.md section 3.1: "Constants in code, not in `Config`").

use anchor_lang::prelude::*;

// ---- PDA seeds ---------------------------------------------------------------------------------------------------
pub const CONFIG_SEED: &[u8] = b"config";
pub const STATE_SEED: &[u8] = b"state";
pub const PENDING_SEED: &[u8] = b"pending";
/// System-owned, no data: the Binder pot lamports and the royalty receiver (60%).
pub const POT_SEED: &[u8] = b"pot";
/// System-owned, no data: owns exactly two token accounts and holds nothing between transactions (P2-00).
pub const SWAPPER_SEED: &[u8] = b"swapper";
/// System-owned, no data: the update authority of the Core collection.
pub const AUTHORITY_SEED: &[u8] = b"authority";
pub const BINDER_SEED: &[u8] = b"binder";
/// System-owned, no data: the authority of a Binder's prize token accounts and Core prizes.
pub const VAULT_SEED: &[u8] = b"vault";

// ---- supply and numbering (spec 8) -------------------------------------------------------------------------------
pub const MAX_SUPPLY: u16 = 1_000;
pub const TEAM_COUNT: u8 = 30;
pub const PUBLIC_MAX: u16 = 970;
/// A price tier is 100 PUBLIC mints; team mints never count (owner decision 2026-11-02).
pub const TIER_SIZE: u16 = 100;
pub const TIERS: usize = 10;
/// A 1,000-bit bitmap: bit `number - 1`.
pub const MASK_BYTES: usize = 125;
/// The most numbers one `mint_reserve` call mints (transaction size, design section 7).
pub const MAX_RESERVE_PER_CALL: usize = 5;
/// Public mints that take part in genesis (with the 30 team Binders).
pub const GENESIS_PUBLIC_MINTS: u16 = 100;

// ---- money (spec 8: the 0.5 SOL payment is fixed) ----------------------------------------------------------------
pub const POT_LAMPORTS: u64 = 300_000_000;
pub const TREASURY_LAMPORTS: u64 = 200_000_000;
/// The accumulator scale: `acc_per_binder` and `owed_scaled` are lamports x 10^12.
pub const SCALE: u128 = 1_000_000_000_000;
/// Basis points of the factor: 11,000 = 1.1.
pub const FACTOR_DENOMINATOR: u128 = 10_000;
/// Rounds: the DEFAULT threshold is 5 SOL; `init_config` accepts 0.1 to 100 SOL.
pub const MIN_ROUND_THRESHOLD: u64 = 100_000_000;
pub const MAX_ROUND_THRESHOLD: u64 = 100_000_000_000;
/// `sync_pot` does at most this many round steps per call; the rest stays income for the next call. A mint refuses
/// (`SyncIncomplete`) while any income is left, so the client puts as many `sync_pot` instructions in front as the backlog
/// needs (review R1).
pub const SYNC_STEPS: u8 = 4;

/// The burn amount of a tier lies in this range and so do the bounds `init_config` accepts (SECURITY: 0.1 to 2.0 SOL).
pub const MIN_BURN_FLOOR: u64 = 100_000_000;
pub const MAX_BURN_CEIL: u64 = 2_000_000_000;

/// The team numbers (spec 8): #100, #15, #67 and #86 plus 26 numbers from #2 to #99; #1 and the one-of-ones stay public
/// (review R4). `set_team_numbers` takes only numbers in this range and requires the four named ones.
pub const TEAM_NUMBER_MIN: u16 = 2;
pub const TEAM_NUMBER_MAX: u16 = 100;
pub const TEAM_REQUIRED: [u16; 4] = [15, 67, 86, 100];

/// What Metaplex Core leaves of an asset after `BurnV1` (mpl-core 0.12.1 and the cloned binary, `forfeit.test.ts`): an account
/// still owned by Core, resized to ONE byte, whose value is `Key::Uninitialized` (the first variant of `Key`, so 0). It keeps its
/// rent, so it never disappears and its address cannot be created again.
pub const CORE_KEY_UNINITIALIZED: u8 = 0;

// ---- royalty (owner decision 2026-10-28: 5%, 3% pot / 2% Treasury) ------------------------------------------------
pub const ROYALTY_BPS: u16 = 500;
pub const ROYALTY_POT_PERCENT: u8 = 60;
pub const ROYALTY_TREASURY_PERCENT: u8 = 40;
pub const MAX_RULES: usize = 8;

// ---- time -------------------------------------------------------------------------------------------------------
/// The timelock of a queued change (DEFAULT 24 h): the same constant in every build (design D16).
pub const TIMELOCK_SECS: i64 = 86_400;
/// The genesis window after the opening (24 h).
pub const GENESIS_WINDOW_SECS: i64 = 86_400;

// ---- metadata ---------------------------------------------------------------------------------------------------
pub const MAX_URI_BYTES: usize = 96;
pub const MIN_URI_BYTES: usize = 8;
pub const COLLECTION_NAME: &str = "Pack Binders";

// ---- phases -----------------------------------------------------------------------------------------------------
pub const PHASE_DBC: u8 = 0;
pub const PHASE_DAMM: u8 = 1;
pub const ROUND_SOL: u8 = 0;
pub const ROUND_CARD: u8 = 1;

// ---- venues (P2-00; the program ids, authorities and discriminators are constants, never inputs) ---------------------
/// Meteora Dynamic Bonding Curve.
pub const DBC_PROGRAM: Pubkey = pubkey!("dbcij3LWUppWqq96dh6gJWwBifmcGfLSB5D4DuSMaqN");
/// The DBC `pool_authority` of `swap2`.
pub const DBC_POOL_AUTHORITY: Pubkey = pubkey!("FhVo3mqL8PW5pH5U2CN4XE33DokiyZnUwuGpH2hmHLuM");
/// `["__event_authority"]` of the DBC program.
pub const DBC_EVENT_AUTHORITY: Pubkey = pubkey!("8Ks12pbrD6PXxfty1hVQiE9sc289zgU1zHkvXhrSdriF");
/// Meteora DAMM v2 (`cp_amm`).
pub const DAMM_PROGRAM: Pubkey = pubkey!("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
pub const DAMM_POOL_AUTHORITY: Pubkey = pubkey!("HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC");
pub const DAMM_EVENT_AUTHORITY: Pubkey = pubkey!("3rmHSu74h1ZcmAisVcWerTCiRDQbUrBKmcwptYGjHfet");
/// The DAMM v2 config of the DBC migration for the Pack preset (`MigrationFeeOption::FixedBps100`). Its
/// `pool_creator_authority` is the DBC pool authority, so only the migration can create a pool on it (P2-00).
pub const DAMM_MIGRATION_CONFIG: Pubkey = pubkey!("Hv8Lmzmnju6m7kcokVKvwqz7QPmdX9XfKjJsXz8RXcjp");
/// Wrapped SOL, the quote asset of both venues for $PACK.
pub const WSOL_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");
/// Anchor discriminator of `swap2` on both programs (identical in both IDLs): sha256("global:swap2")[..8].
pub const SWAP2_DISCRIMINATOR: [u8; 8] = [65, 75, 63, 76, 235, 91, 91, 136];
/// `SwapMode` of both venues: 0 exact in, 1 partial fill, 2 exact out (never used).
pub const MODE_EXACT_IN: u8 = 0;
pub const MODE_PARTIAL_FILL: u8 = 1;

// ---- pinned account layouts of the venues (packages/adapters/meteora-dbc/src/constants.ts, equal to the IDLs) ----------
/// Anchor account discriminators.
pub const DBC_POOL_DISCRIMINATOR: [u8; 8] = [213, 224, 5, 209, 98, 69, 119, 92];
pub const DBC_CONFIG_DISCRIMINATOR: [u8; 8] = [26, 108, 14, 123, 116, 230, 129, 43];
pub const DAMM_POOL_DISCRIMINATOR: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
/// DBC `VirtualPool`: 424 bytes.
pub const DBC_POOL_SIZE: usize = 424;
pub const DBC_POOL_CONFIG: usize = 72;
pub const DBC_POOL_BASE_MINT: usize = 136;
pub const DBC_POOL_BASE_VAULT: usize = 168;
pub const DBC_POOL_QUOTE_VAULT: usize = 200;
/// DBC `PoolConfig`: 1,048 bytes (`ConfigWithTransferHook` has 1,128 and is refused).
pub const DBC_CONFIG_SIZE: usize = 1_048;
pub const DBC_CONFIG_QUOTE_MINT: usize = 8;
pub const DBC_CONFIG_BASE_FEE_MODE: usize = 130;
pub const DBC_CONFIG_COLLECT_FEE_MODE: usize = 232;
/// `BaseFeeMode`: 0 linear and 1 exponential fee scheduler; 2 is the deprecated rate limiter.
pub const BASE_FEE_MODE_RATE_LIMITER: u8 = 2;
/// DAMM v2 `Pool`: 1,112 bytes.
pub const DAMM_POOL_SIZE: usize = 1_112;
pub const DAMM_POOL_TOKEN_A_MINT: usize = 168;
pub const DAMM_POOL_TOKEN_B_MINT: usize = 200;
pub const DAMM_POOL_TOKEN_A_VAULT: usize = 232;
pub const DAMM_POOL_TOKEN_B_VAULT: usize = 264;
