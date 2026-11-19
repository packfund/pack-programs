//! Accounts of `pack_binder` (docs/validation/P2-01_BINDER_DESIGN.md section 3).
//!
//! `Config` is cold (pins and parameters, read by every mint), `State` is hot (counters, bitmap, accumulator, genesis,
//! rounds), `Pending` holds the queued changes so the mint does not deserialize them, `Binder` is one record per
//! Binder. The pot (`["pot"]`), the swapper, the collection authority and every asset vault are system-owned PDAs with
//! no data: they appear in the seeds only.

use anchor_lang::prelude::*;

use crate::constants::{MASK_BYTES, MAX_RULES, MAX_URI_BYTES, TIERS};

/// `Config`: the singleton `["config"]`. Written by `init_config`, `init_collection` and the admin instructions.
#[account]
#[derive(InitSpace)]
pub struct Config {
    /// Squads vault on mainnet. Admin instructions and nothing else; it can never move a holder's money.
    pub admin: Pubkey,
    /// The owner wallet: the only signer of `mint_reserve`. Fixed at init.
    pub reserve_signer: Pubkey,
    /// The Treasury: the owner of every team Binder. Fixed at init.
    pub reserve_recipient: Pubkey,
    /// Receives the 0.20 SOL of a mint and 40% of every royalty. Fixed at init.
    pub treasury: Pubkey,
    /// The only destination of a cancel. Fixed at init.
    pub buyback_wallet: Pubkey,
    /// The only destination of the card budget. Fixed at init.
    pub card_wallet: Pubkey,
    pub pack_mint: Pubkey,
    pub pack_token_program: Pubkey,
    /// The swapper PDA's associated $PACK and wSOL accounts, computed once at init (P2-00: a key compare per mint).
    pub swapper_pack_ata: Pubkey,
    pub swapper_wsol_ata: Pubkey,
    pub dbc_config: Pubkey,
    pub dbc_pool: Pubkey,
    pub dbc_base_vault: Pubkey,
    pub dbc_quote_vault: Pubkey,
    /// The default key until `set_damm`, then the DERIVED migration pool and its two vaults.
    pub damm_pool: Pubkey,
    pub damm_vault_a: Pubkey,
    pub damm_vault_b: Pubkey,
    /// The Core collection (the default key until `init_collection`).
    pub collection: Pubkey,
    /// sha256 of the art manifest, published with the metadata. Not checkable on chain.
    pub art_hash: [u8; 32],
    /// The metadata folder URI; the program builds `base_uri + number + ".json"`.
    #[max_len(MAX_URI_BYTES)]
    pub base_uri: String,
    /// 0 = DBC pool, 1 = DAMM v2 pool.
    pub phase: u8,
    pub paused: bool,
    pub cancelled: bool,
    pub team_set: bool,
    pub art_locked: bool,
    /// The 30 team numbers: bit `number - 1`.
    pub team_mask: [u8; MASK_BYTES],
    pub base_lamports: u64,
    pub factor_bps: u32,
    pub min_burn_lamports: u64,
    pub max_burn_lamports: u64,
    /// The burn amount of tiers 0 to 9 (derived from the base and the factor, floor at every step).
    pub burn_by_tier: [u64; TIERS],
    pub round_threshold: u64,
    pub config_bump: u8,
    pub state_bump: u8,
    pub pending_bump: u8,
    pub swapper_bump: u8,
    pub authority_bump: u8,
    pub pot_bump: u8,
}

/// `State`: the singleton `["state"]`. Written by every mint, claim and crank.
#[account]
#[derive(InitSpace)]
pub struct State {
    /// The supply and sequence counter: every Binder ever minted, burned or not (at most 1,000). The number of a burned Binder
    /// stays set in `minted_mask`, so it can never be minted again.
    pub minted: u16,
    /// The Binders that share income: `minted` minus the forfeited (burned) ones. The divisor `n` of the accumulator.
    pub active: u16,
    pub reserve_minted: u8,
    pub public_minted: u16,
    /// The lowest candidate for the next public number.
    pub next_public: u16,
    /// 0 until the first `mint_reserve`: the art is locked and the genesis amount is locked at this moment.
    pub opened_at: i64,
    /// 0 until the 30th team Binder exists: public mints are possible from this moment and the 24 hour genesis window
    /// starts here (review R3: a team batch that takes longer than 24 hours must not let a finalize precede every public mint).
    pub public_open_at: i64,
    /// The numbers that exist: bit `number - 1`.
    pub minted_mask: [u8; MASK_BYTES],
    /// The reward-per-Binder accumulator, lamports x 10^12.
    pub acc_per_binder: u128,
    /// Accumulator shares credited and not yet paid, lamports x 10^12 (exact; the reserve is its ceiling).
    pub owed_scaled: u128,
    /// The amount locked at the opening, apart from the regular pot, until it is shared.
    pub genesis_locked: u64,
    /// The shares created at the sharing and not yet paid.
    pub genesis_owed: u64,
    pub genesis_shared: bool,
    /// Binders eligible for genesis: 30 team plus public mints 1 to 100 minted before the sharing.
    pub genesis_eligible_count: u16,
    pub genesis_per_binder: u64,
    /// 0 = SOL round (credited through the accumulator), 1 = card round (a budget being reserved).
    pub round_kind: u8,
    /// Lamports credited (SOL round) or reserved (card round) so far, always below the threshold.
    pub round_progress: u64,
    pub round_index: u32,
    /// Closed card budgets not yet withdrawn by the card wallet.
    pub card_withdrawable: u64,
    pub income_realized_total: u64,
    pub claimed_total: u64,
    pub bump: u8,
}

impl State {
    /// A fresh state (arrays of 125 bytes have no `Default`).
    pub fn new(bump: u8) -> Self {
        Self {
            minted: 0,
            active: 0,
            reserve_minted: 0,
            public_minted: 0,
            next_public: 1,
            opened_at: 0,
            public_open_at: 0,
            minted_mask: [0u8; MASK_BYTES],
            acc_per_binder: 0,
            owed_scaled: 0,
            genesis_locked: 0,
            genesis_owed: 0,
            genesis_shared: false,
            genesis_eligible_count: 0,
            genesis_per_binder: 0,
            round_kind: 0,
            round_progress: 0,
            round_index: 0,
            card_withdrawable: 0,
            income_realized_total: 0,
            claimed_total: 0,
            bump,
        }
    }
}

/// `Pending`: the singleton `["pending"]`: the queued price change, the queued rule set, a proposed admin.
#[account]
#[derive(InitSpace)]
pub struct Pending {
    /// The default key when no rotation is proposed.
    pub pending_admin: Pubkey,
    pub price_active: bool,
    pub price_base: u64,
    pub price_factor_bps: u32,
    pub price_eta: i64,
    pub rules_active: bool,
    pub rules_eta: i64,
    pub rules_count: u8,
    pub rules: [Pubkey; MAX_RULES],
    pub bump: u8,
}

/// `Binder`: one per Binder, `["binder", asset]`.
#[account]
#[derive(InitSpace)]
pub struct Binder {
    pub asset: Pubkey,
    /// 1 to 1,000: its art.
    pub number: u16,
    /// The mint sequence, 1 to 1,000 in the order minted (team first): the key of the card-round snapshot.
    pub seq: u16,
    pub is_team: bool,
    pub genesis_eligible: bool,
    pub genesis_claimed: bool,
    /// Set by `forfeit_burned`: the Core asset was burned and the Binder left the active set. The keeper raffle skips it.
    pub forfeited: bool,
    /// The accumulator value at the mint, advanced by every claim (the fraction is kept).
    pub acc_snapshot: u128,
    pub sol_claimed: u64,
    pub bump: u8,
}
