# pack_binder

The Pack Binder (`docs/PROTOCOL_SPEC.md` section 8): a Metaplex Core collection of 1,000 membership NFTs. 30 team Binders are
minted free by the owner wallet; 970 are public mints that each pay 0.30 SOL to the Binder pot and 0.20 SOL to the Treasury,
spend a tiered burn amount of SOL on $PACK by CPI (the DBC pool before the migration, DAMM v2 after it) and burn every $PACK
they received, in one transaction. The pot is shared equally among the Binders that exist, from their mint onward, through a
reward-per-Binder accumulator; the pot before the opening is locked as the genesis amount and shared once. The 5% resale
royalty (3% pot, 2% Treasury) is a collection-level Core Royalties plugin.

Read `docs/validation/P2-01_BINDER_DESIGN.md` first: every account, instruction, check and invariant is there. The swap flow
is proven in `docs/validation/P2-00_BINDER_CPI.md`. Security argument, item by item:
`docs/validation/PACK_BINDER_SECURITY_CHECKLIST.md`.

## Build and test

```bash
bash ops/with-env.sh cargo test -p pack_binder                    # unit tests: price tiers, bitmaps, the pot model, venue layouts
bash ops/with-env.sh cargo build-sbf --arch v3 --manifest-path programs/pack_binder/Cargo.toml   # target/deploy/pack_binder.so (SBPF v3, as shipped; without --arch it is v0, 47 KB larger)
bash ops/with-env.sh pnpm --filter @pack/tests test:binder        # LiteSVM: the instructions, the clock, the real cloned Core and DBC
bash ops/with-env.sh bash ops/validator/binder-e2e.sh             # the offline validator: curve completion, migration, DAMM v2
```

The program is built for SBPF v3 and calls Metaplex Core by hand (`src/core_cpi.rs`; the `mpl-core` crate is a dev-dependency whose builders the unit tests compare against byte for byte) and reads the upgradeable loader state by hand (`src/loader_state.rs`): `docs/validation/P2-10_BINDER_SIZE.md` section 10.

The 24 hour timelock and the 24 hour genesis window cannot be waited out on the validator (its clock cannot move), so the
instruction tests run on LiteSVM with a set clock (design D16: no short-timelock build exists). `solana-test-validator` has
no DAMM v2 migration without the real program, so that part runs on the validator.

## Program id

`declare_id!` in `src/lib.rs` is a LOCAL id whose secret key was never kept: LiteSVM and the test validator load the program at
that address at genesis. The devnet and mainnet ids are created at deploy time (gates G-KEYS and G-FUNDS); change
`declare_id!`, the clients and `tests/binder/lib/ix.ts` together.

## Instructions

| Instruction | Signer | When | What it does |
|---|---|---|---|
| `init_config`, `init_collection` | upgrade authority | once | Create `Config`, `State`, `Pending`, the pot vault and the Core collection with the Royalties plugin; verify every pinned account on chain |
| `set_art`, `set_team_numbers` | admin | before the opening | The metadata folder and art hash; the 30 team numbers |
| `mint_reserve` | `reserve_signer` | team numbers only, 5 per call | Mints team Binders to the Treasury; the first call opens the mint and locks the genesis amount |
| `mint_binder_dbc`, `mint_binder_damm` | buyer | all 30 team Binders exist; not paused | The public mint on the active venue |
| `set_damm` | admin | once, after the migration | Pins the DERIVED DAMM v2 pool and vaults, flips the phase |
| `sync_pot`, `finalize_genesis` | anyone | income exists; 24 h after the public mints opened (the 30th team Binder) | Realizes pot income for the active Binders (at most four round steps per call; a mint refuses while income is left, so the client puts `sync_pot` in front); shares genesis |
| `claim_vault` | the Binder's current owner | anything to claim | Pays the accumulator share and the genesis share |
| `vault_withdraw_token`, `vault_withdraw_core` | the Binder's current owner | prizes in the vault | Moves tokens or Core prizes out of the Binder's vault |
| `withdraw_card_budget` | `card_wallet` | a card round closed | Sends a closed card budget to the fixed card wallet |
| `forfeit_burned` | anyone (the keeper) | the Binder's Core asset was burned | Returns the Binder's unclaimed shares to the pot as income for the active Binders and removes it from the active set (`State.active`); the number stays taken |
| `queue_price_change`, `cancel_price_change`, `apply_price_change` | admin; anyone to apply | 24 h timelock | The burn schedule, from the next tier only |
| `queue_rule_set`, `cancel_rule_set`, `apply_rule_set` | admin; anyone to apply | 24 h timelock | The royalty allow list (the only part of the plugin that can change) |
| `set_paused` | admin | any time | Stops or resumes PUBLIC mints only |
| `propose_admin`, `accept_admin` | admin, then the new admin | any time | Two-step admin rotation |
| `cancel_binder`, `sweep_to_buyback` | admin; anyone to sweep | only before the opening | Cancels the Binder; the free pot goes to the fixed buyback wallet |
