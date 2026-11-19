//! `pack_binder`: the Pack Binder (docs/PROTOCOL_SPEC.md section 8, docs/validation/P2-01_BINDER_DESIGN.md).
//!
//! A Metaplex Core collection of 1,000 membership NFTs (30 team Binders minted free by the owner wallet, 970 public
//! mints). A public mint pays 0.30 SOL to the pot and 0.20 SOL to the Treasury and spends a burn amount of SOL, by CPI
//! with a PDA signer, on $PACK's active pool (the DBC pool before the migration, DAMM v2 after it), then burns every $PACK it
//! received (balance difference, P2-00), all in one transaction. The pot is shared equally among the Binders that exist,
//! from the mint onward, through a reward-per-Binder accumulator; the pot before the opening is locked as the genesis
//! amount and shared once. The 5% resale royalty (3% pot / 2% Treasury) is a collection-level Core Royalties plugin.
//!
//! Read `docs/validation/P2-01_BINDER_DESIGN.md` first: every account, instruction, check and invariant is there.

use anchor_lang::prelude::*;

pub mod constants;
pub mod core_cpi;
pub mod cpi_lite;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod loader_state;
pub mod math;
pub mod pack;
pub mod pot;
pub mod state;
pub mod util;
pub mod venue;

use instructions::*;

// The program id of the LOCAL / DEV deployment (its secret key was never kept: the program is loaded at genesis of the test
// validator and of LiteSVM). The devnet and mainnet ids are created at deploy time (gates G-KEYS and G-FUNDS), as
// `pack_bond`: declare_id!, Anchor.toml and the clients are changed together.
declare_id!("Ex8ffvBE3sTMyUtyytKRCrzbfuVs5ytsNLmvW2BZ8Sgi");

#[program]
pub mod pack_binder {
    use super::*;

    /// Upgrade authority: creates `Config`, `State`, `Pending` and funds the pot vault; verifies every pinned account.
    pub fn init_config(ctx: Context<InitConfig>, args: InitConfigArgs) -> Result<()> {
        instructions::init::handle_init_config(ctx, args)
    }

    /// Upgrade authority, once: creates the Core collection with the Royalties plugin (500 bps, pot 60 / Treasury 40).
    pub fn init_collection(ctx: Context<InitCollection>, args: InitCollectionArgs) -> Result<()> {
        instructions::init::handle_init_collection(ctx, args)
    }

    /// Admin, before the opening: the metadata folder and the art manifest hash.
    pub fn set_art(ctx: Context<SetArt>, base_uri: String, art_hash: [u8; 32]) -> Result<()> {
        instructions::admin::handle_set_art(ctx, base_uri, art_hash)
    }

    /// Admin, once, before the opening: the 30 team numbers.
    pub fn set_team_numbers(ctx: Context<SetTeamNumbers>, numbers: [u16; 30]) -> Result<()> {
        instructions::admin::handle_set_team_numbers(ctx, numbers)
    }

    /// Admin, once, after the migration: pins the derived DAMM v2 pool and vaults and flips the phase.
    pub fn set_damm(ctx: Context<SetDamm>) -> Result<()> {
        instructions::admin::handle_set_damm(ctx)
    }

    /// Admin: queues a new base and factor behind the 24 hour timelock.
    pub fn queue_price_change(
        ctx: Context<QueuePriceChange>,
        base_lamports: u64,
        factor_bps: u32,
    ) -> Result<()> {
        instructions::admin::handle_queue_price_change(ctx, base_lamports, factor_bps)
    }

    pub fn cancel_price_change(ctx: Context<CancelPriceChange>) -> Result<()> {
        instructions::admin::handle_cancel_price_change(ctx)
    }

    /// Anyone, after the eta: applies the queued price change to the tiers above the one being minted.
    pub fn apply_price_change(ctx: Context<ApplyPriceChange>) -> Result<()> {
        instructions::admin::handle_apply_price_change(ctx)
    }

    /// Admin: queues a new royalty allow list (the System Program required) behind the timelock.
    pub fn queue_rule_set(ctx: Context<QueueRuleSet>, rules: Vec<Pubkey>) -> Result<()> {
        instructions::admin::handle_queue_rule_set(ctx, rules)
    }

    pub fn cancel_rule_set(ctx: Context<CancelRuleSet>) -> Result<()> {
        instructions::admin::handle_cancel_rule_set(ctx)
    }

    /// Anyone, after the eta: writes the queued allow list to the collection's Royalties plugin.
    pub fn apply_rule_set(ctx: Context<ApplyRuleSet>) -> Result<()> {
        instructions::admin::handle_apply_rule_set(ctx)
    }

    /// Admin: stops or resumes PUBLIC mints (claims and cranks keep working).
    pub fn set_paused(ctx: Context<SetPaused>, paused: bool) -> Result<()> {
        instructions::admin::handle_set_paused(ctx, paused)
    }

    pub fn propose_admin(ctx: Context<ProposeAdmin>, new_admin: Pubkey) -> Result<()> {
        instructions::admin::handle_propose_admin(ctx, new_admin)
    }

    pub fn accept_admin(ctx: Context<AcceptAdmin>) -> Result<()> {
        instructions::admin::handle_accept_admin(ctx)
    }

    /// Admin, only before the opening: cancels the Binder and sends the free pot to the fixed buyback wallet.
    pub fn cancel_binder(ctx: Context<CancelBinder>) -> Result<()> {
        instructions::admin::handle_cancel_binder(ctx)
    }

    /// Anyone, after a cancel: sends what the pot holds above its rent minimum to the buyback wallet.
    pub fn sweep_to_buyback(ctx: Context<SweepToBuyback>) -> Result<()> {
        instructions::admin::handle_sweep_to_buyback(ctx)
    }

    /// The owner wallet: mints 1 to 5 team Binders to the Treasury, free. The first call opens the mint.
    pub fn mint_reserve<'info>(
        ctx: Context<'info, MintReserve<'info>>,
        numbers: Vec<u16>,
    ) -> Result<()> {
        instructions::reserve::handle_mint_reserve(ctx, numbers)
    }

    /// Anyone: the public mint on the DBC pool (before the migration).
    pub fn mint_binder_dbc(ctx: Context<MintBinderDbc>, min_out: u64) -> Result<()> {
        instructions::mint::handle_mint_binder_dbc(ctx, min_out)
    }

    /// Anyone: the public mint on the DAMM v2 pool (after the migration and `set_damm`).
    pub fn mint_binder_damm(ctx: Context<MintBinderDamm>, min_out: u64) -> Result<()> {
        instructions::mint::handle_mint_binder_damm(ctx, min_out)
    }

    /// Anyone: realizes the pot's free income for the Binders that exist.
    pub fn sync_pot(ctx: Context<SyncPot>) -> Result<()> {
        instructions::crank::handle_sync_pot(ctx)
    }

    /// Anyone, 24 hours after the opening: shares genesis among the eligible Binders minted so far.
    pub fn finalize_genesis(ctx: Context<FinalizeGenesis>) -> Result<()> {
        instructions::crank::handle_finalize_genesis(ctx)
    }

    /// The current owner of a Binder: claims its accumulator share and, once, its genesis share.
    pub fn claim_vault(ctx: Context<ClaimVault>) -> Result<()> {
        instructions::vault::handle_claim_vault(ctx)
    }

    /// Anyone, for a Binder whose Core asset was burned: returns its unclaimed shares to the pot as income and removes it from
    /// the active set (owner decision 2026-11-09).
    pub fn forfeit_burned(ctx: Context<ForfeitBurned>) -> Result<()> {
        instructions::forfeit::handle_forfeit_burned(ctx)
    }

    /// The current owner: moves tokens out of the Binder's vault.
    pub fn vault_withdraw_token(ctx: Context<VaultWithdrawToken>, amount: u64) -> Result<()> {
        instructions::vault::handle_vault_withdraw_token(ctx, amount)
    }

    /// The current owner: moves a Core prize out of the Binder's vault.
    pub fn vault_withdraw_core(ctx: Context<VaultWithdrawCore>) -> Result<()> {
        instructions::vault::handle_vault_withdraw_core(ctx)
    }

    /// The card wallet: takes a closed card budget out of the pot.
    pub fn withdraw_card_budget(ctx: Context<WithdrawCardBudget>, amount: u64) -> Result<()> {
        instructions::crank::handle_withdraw_card_budget(ctx, amount)
    }
}
