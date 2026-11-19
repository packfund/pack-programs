//! The accounting of the Binder pot (docs/validation/P2-01_BINDER_DESIGN.md section 5): the pot identity, `sync_pot`,
//! claims, genesis and the registration of a mint. Everything here works on `State` and plain numbers (no account, no
//! syscall), so the money model is unit-tested on the host and the same functions run in the instructions.
//!
//! The pot identity (design 5.1), true after every instruction, with `L` the lamports of the pot vault and `R` its
//! rent minimum:
//!
//! ```text
//! L - R >= ceil(owed_scaled / SCALE) + genesis_locked + genesis_owed + card_reserved + card_withdrawable
//! card_reserved = round_progress while a card round is being filled, else 0
//! ```
//!
//! `income` (what `sync_pot` may realize) is the left side minus the right side. `owed_scaled` is the EXACT sum of
//! `n x delta` minus the payouts, so the reserve is at most a fraction of a lamport too high and never too low (design 5.2).

use anchor_lang::prelude::*;

use crate::constants::{
    GENESIS_PUBLIC_MINTS, GENESIS_WINDOW_SECS, MAX_SUPPLY, PUBLIC_MAX, ROUND_CARD, ROUND_SOL,
    SCALE, SYNC_STEPS, TEAM_COUNT,
};
use crate::errors::BinderError;
use crate::math::{bit_get, bit_set};
use crate::state::State;

/// A card round that filled during a sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClosedRound {
    pub index: u32,
    pub budget: u64,
    /// The mint-sequence snapshot of the raffle: `minted` at the moment the round closed.
    pub snapshot_seq: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    /// Income taken out of the free balance in this call.
    pub realized: u64,
    /// Of which credited through the accumulator (SOL rounds).
    pub credited: u64,
    /// Of which reserved for a card budget (card rounds).
    pub to_cards: u64,
    pub closed: Vec<ClosedRound>,
}

fn ceil_div(a: u128, b: u128) -> Result<u128> {
    a.checked_add(b - 1)
        .map(|x| x / b)
        .ok_or_else(|| error!(BinderError::MathOverflow))
}

/// The lamports of the pot that are promised to someone (design 5.1, right-hand side).
pub fn reserved_lamports(s: &State) -> Result<u128> {
    let card_reserved = if s.round_kind == ROUND_CARD {
        s.round_progress
    } else {
        0
    };
    ceil_div(s.owed_scaled, SCALE)?
        .checked_add(u128::from(s.genesis_locked))
        .and_then(|v| v.checked_add(u128::from(s.genesis_owed)))
        .and_then(|v| v.checked_add(u128::from(card_reserved)))
        .and_then(|v| v.checked_add(u128::from(s.card_withdrawable)))
        .ok_or_else(|| error!(BinderError::MathOverflow))
}

/// The free income: the pot's lamports above its rent minimum and above everything promised. A pot that holds less than
/// it has promised is a broken invariant, never a rounding to zero.
pub fn free_income(pot_lamports: u64, rent_min: u64, s: &State) -> Result<u64> {
    let balance = pot_lamports
        .checked_sub(rent_min)
        .ok_or_else(|| error!(BinderError::InvariantBroken))?;
    let reserved = reserved_lamports(s)?;
    let income = u128::from(balance)
        .checked_sub(reserved)
        .ok_or_else(|| error!(BinderError::InvariantBroken))?;
    u64::try_from(income).map_err(|_| error!(BinderError::MathOverflow))
}

/// The pot identity: the pot can pay everything it has promised and still keeps its rent minimum.
pub fn check_identity(pot_lamports: u64, rent_min: u64, s: &State) -> Result<()> {
    free_income(pot_lamports, rent_min, s).map(|_| ())
}

/// A Binder may be counted only when the pot holds no free income (review R1). A sync does at most `SYNC_STEPS` round steps,
/// so a backlog of more steps would be realized later with the new Binder among the `n` and the new Binder would share income
/// that arrived before its mint. The mint and `mint_reserve` call this after their own sync; the client puts as many
/// `sync_pot` instructions in front as the backlog needs. A complete sync leaves 0 in practice: every realized lamport is reserved
/// (`n x delta` is within `n / SCALE` of `take x SCALE`, and the reserve is rounded up). In a rare state (the fractional part of
/// `owed_scaled` within `n` of a whole lamport) ONE lamport of free income can remain; the next sync clears it, so such a mint
/// fails `SyncIncomplete` once and succeeds with one more `sync_pot` in front (the client re-simulates and retries once). It
/// can never get stuck: any lamport left is realizable.
pub fn require_synced(pot_lamports: u64, rent_min: u64, s: &State) -> Result<()> {
    // with no active Binder there is nobody an earlier income could be taken from: it waits for the next Binder, and the mint
    // that adds it must not be blocked by it (every Binder burned and forfeited; no deadlock)
    if s.active == 0 {
        return Ok(());
    }
    require!(
        free_income(pot_lamports, rent_min, s)? == 0,
        BinderError::SyncIncomplete
    );
    Ok(())
}

/// Realizes `income` for the `s.active` Binders that exist (design 5.3; the active ones are the minted ones minus the
/// forfeited, i.e. burned, ones). A SOL round credits exactly `threshold` through
/// the accumulator and a card round reserves exactly `threshold`; the split happens at the boundary. At most
/// `SYNC_STEPS` steps per call: whatever is left stays income for the next call. No Binder, no income: a no-op.
pub fn sync(s: &mut State, income: u64, threshold: u64) -> Result<SyncOutcome> {
    let mut out = SyncOutcome::default();
    if s.active == 0 || income == 0 {
        return Ok(out);
    }
    let n = u128::from(s.active);
    let mut left = income;
    for _ in 0..SYNC_STEPS {
        if left == 0 {
            break;
        }
        let room = threshold
            .checked_sub(s.round_progress)
            .filter(|r| *r > 0)
            .ok_or_else(|| error!(BinderError::InvariantBroken))?;
        let take = left.min(room);
        if s.round_kind == ROUND_SOL {
            // delta >= 10^12 / 1,000 for take >= 1, so a step always moves the accumulator. `n x delta` is at most
            // `take x SCALE`: the part of `take` lost to the division (under one lamport) is not reserved, so it shows as
            // free income again and is realized with the next income (the round counts `take`, the reserve counts `n x delta`).
            let delta = u128::from(take)
                .checked_mul(SCALE)
                .map(|v| v / n)
                .ok_or_else(|| error!(BinderError::MathOverflow))?;
            s.acc_per_binder = s
                .acc_per_binder
                .checked_add(delta)
                .ok_or_else(|| error!(BinderError::MathOverflow))?;
            s.owed_scaled = n
                .checked_mul(delta)
                .and_then(|v| s.owed_scaled.checked_add(v))
                .ok_or_else(|| error!(BinderError::MathOverflow))?;
            out.credited = out
                .credited
                .checked_add(take)
                .ok_or_else(|| error!(BinderError::MathOverflow))?;
        } else {
            out.to_cards = out
                .to_cards
                .checked_add(take)
                .ok_or_else(|| error!(BinderError::MathOverflow))?;
        }
        s.round_progress = s
            .round_progress
            .checked_add(take)
            .ok_or_else(|| error!(BinderError::MathOverflow))?;
        left -= take;
        if s.round_progress == threshold {
            if s.round_kind == ROUND_SOL {
                s.round_kind = ROUND_CARD;
            } else {
                s.card_withdrawable = s
                    .card_withdrawable
                    .checked_add(threshold)
                    .ok_or_else(|| error!(BinderError::MathOverflow))?;
                out.closed.push(ClosedRound {
                    index: s.round_index,
                    budget: threshold,
                    snapshot_seq: s.minted,
                });
                s.round_index = s
                    .round_index
                    .checked_add(1)
                    .ok_or_else(|| error!(BinderError::MathOverflow))?;
                s.round_kind = ROUND_SOL;
            }
            s.round_progress = 0;
        }
    }
    out.realized = income - left;
    s.income_realized_total = s
        .income_realized_total
        .checked_add(out.realized)
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------------------
// claims
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimAmounts {
    pub acc_share: u64,
    pub genesis_share: u64,
    /// The new `acc_snapshot`: advanced by exactly the paid share, so the fraction is kept.
    pub new_snapshot: u128,
}

impl ClaimAmounts {
    pub fn total(&self) -> Result<u64> {
        self.acc_share
            .checked_add(self.genesis_share)
            .ok_or_else(|| error!(BinderError::MathOverflow))
    }
}

/// Whether a Binder still has a genesis share to take.
pub fn genesis_pending(s: &State, eligible: bool, claimed: bool) -> bool {
    s.genesis_shared && eligible && !claimed
}

/// What a Binder can claim now: `floor((acc - snapshot) / SCALE)` plus the genesis share once.
pub fn claim_amounts(s: &State, snapshot: u128, genesis_pending: bool) -> Result<ClaimAmounts> {
    let diff = s
        .acc_per_binder
        .checked_sub(snapshot)
        .ok_or_else(|| error!(BinderError::InvariantBroken))?;
    let acc_share = u64::try_from(diff / SCALE).map_err(|_| error!(BinderError::MathOverflow))?;
    let new_snapshot = u128::from(acc_share)
        .checked_mul(SCALE)
        .and_then(|v| snapshot.checked_add(v))
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    Ok(ClaimAmounts {
        acc_share,
        genesis_share: if genesis_pending {
            s.genesis_per_binder
        } else {
            0
        },
        new_snapshot,
    })
}

/// Takes a claim out of the promised amounts (the caller pays the lamports).
pub fn apply_claim(s: &mut State, c: &ClaimAmounts) -> Result<()> {
    s.owed_scaled = u128::from(c.acc_share)
        .checked_mul(SCALE)
        .and_then(|v| s.owed_scaled.checked_sub(v))
        .ok_or_else(|| error!(BinderError::InvariantBroken))?;
    s.genesis_owed = s
        .genesis_owed
        .checked_sub(c.genesis_share)
        .ok_or_else(|| error!(BinderError::InvariantBroken))?;
    s.claimed_total = s
        .claimed_total
        .checked_add(c.total()?)
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// genesis
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenesisOutcome {
    pub eligible: u16,
    pub per_binder: u64,
    /// What goes back to the pot as ordinary income (it simply stops being reserved).
    pub remainder: u64,
}

/// Locks the pot's free balance as the genesis amount (the opening: nothing is owed yet, so everything above the rent
/// minimum is the genesis amount).
pub fn lock_genesis(s: &mut State, pot_lamports: u64, rent_min: u64) -> Result<u64> {
    let free = free_income(pot_lamports, rent_min, s)?;
    s.genesis_locked = free;
    Ok(free)
}

/// Shares the genesis amount equally among the eligible Binders minted so far; the remainder becomes income.
pub fn share_genesis(s: &mut State) -> Result<GenesisOutcome> {
    require!(!s.genesis_shared, BinderError::GenesisAlreadyShared);
    let eligible = u64::from(s.genesis_eligible_count);
    // every eligible Binder can have been burned and forfeited before the sharing: nobody to pay, the amount goes back to income
    let per = s.genesis_locked.checked_div(eligible).unwrap_or(0);
    let distributed = per
        .checked_mul(eligible)
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    let remainder = s.genesis_locked - distributed;
    s.genesis_owed = s
        .genesis_owed
        .checked_add(distributed)
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    s.genesis_per_binder = per;
    s.genesis_locked = 0;
    s.genesis_shared = true;
    Ok(GenesisOutcome {
        eligible: s.genesis_eligible_count,
        per_binder: per,
        remainder,
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// burned Binders (owner decision 2026-11-09, review R6)
// ---------------------------------------------------------------------------------------------------------------------

/// What a forfeit released from the reserve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Forfeited {
    /// The unclaimed accumulator share, lamports x SCALE (exact, with its fraction).
    pub released_acc_scaled: u128,
    /// The unclaimed genesis share in lamports (0 when genesis is not shared yet, not eligible, or already claimed).
    pub released_genesis: u64,
}

/// Takes a burned Binder out of the active set and releases what it had not claimed. The caller has synced first, so the
/// accumulator is current. Nothing is paid and no lamport moves: the reserve shrinks, so the same pot lamports become free
/// income, which the NEXT `sync` credits to the remaining active Binders only (the burned one has left the divisor).
///
/// * accumulator share: `owed_scaled -= acc - snapshot`, exactly. The identity `owed_scaled = sum over active Binders of
///   (acc - snapshot)` still holds, so the pot identity (design 5.1) is kept and nothing is lost: the reserve is the rounded-up
///   total, so the free income gained is the released amount to within the rounding of that total (below one lamport).
/// * genesis share: if genesis is shared, the Binder is eligible and has not claimed, `genesis_owed -= genesis_per_binder`;
///   if genesis is NOT shared yet there is no share to release, so the Binder simply leaves the eligible set
///   (`genesis_eligible_count -= 1`) and the amount is shared equally among the rest (DECISIONS_LOG 2026-11-09).
/// * `active -= 1`; `minted` and the bitmap are untouched (supply, sequence and the number stay).
pub fn forfeit(
    s: &mut State,
    snapshot: u128,
    genesis_eligible: bool,
    genesis_claimed: bool,
) -> Result<Forfeited> {
    require!(s.active > 0, BinderError::InvariantBroken);
    let released_acc_scaled = s
        .acc_per_binder
        .checked_sub(snapshot)
        .ok_or_else(|| error!(BinderError::InvariantBroken))?;
    s.owed_scaled = s
        .owed_scaled
        .checked_sub(released_acc_scaled)
        .ok_or_else(|| error!(BinderError::InvariantBroken))?;
    let mut released_genesis = 0u64;
    if genesis_eligible {
        if s.genesis_shared {
            if !genesis_claimed {
                released_genesis = s.genesis_per_binder;
                s.genesis_owed = s
                    .genesis_owed
                    .checked_sub(released_genesis)
                    .ok_or_else(|| error!(BinderError::InvariantBroken))?;
            }
        } else {
            s.genesis_eligible_count = s
                .genesis_eligible_count
                .checked_sub(1)
                .ok_or_else(|| error!(BinderError::InvariantBroken))?;
        }
    }
    s.active = s
        .active
        .checked_sub(1)
        .ok_or_else(|| error!(BinderError::InvariantBroken))?;
    Ok(Forfeited {
        released_acc_scaled,
        released_genesis,
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// registering a mint
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registered {
    pub seq: u16,
    pub genesis_eligible: bool,
}

/// The end of the 24 hour genesis window: it starts when public mints become possible, that is when the 30th team Binder
/// exists (review R3), and `NotOpen` before that.
pub fn genesis_window_end(s: &State) -> Result<i64> {
    require!(s.public_open_at > 0, BinderError::NotOpen);
    s.public_open_at
        .checked_add(GENESIS_WINDOW_SECS)
        .ok_or_else(|| error!(BinderError::MathOverflow))
}

/// Whether a Binder minted now takes part in genesis: team, or public mint 1 to 100, and only while genesis is not yet
/// shared (a Binder minted after the sharing gets none, whatever its number).
pub fn mint_eligibility(s: &State, is_team: bool, public_index: u16) -> bool {
    !s.genesis_shared && (is_team || public_index <= GENESIS_PUBLIC_MINTS)
}

/// Counts a new Binder: the counters, the bitmap, the cursor and the genesis eligibility. `number` must be free.
pub fn register_mint(s: &mut State, number: u16, is_team: bool) -> Result<Registered> {
    require!(s.minted < MAX_SUPPLY, BinderError::SoldOut);
    require!(!bit_get(&s.minted_mask, number), BinderError::NumberTaken);
    let genesis_eligible;
    if is_team {
        require!(s.reserve_minted < TEAM_COUNT, BinderError::ReserveComplete);
        genesis_eligible = mint_eligibility(s, true, 0);
        s.reserve_minted = s
            .reserve_minted
            .checked_add(1)
            .ok_or_else(|| error!(BinderError::MathOverflow))?;
    } else {
        require!(s.public_minted < PUBLIC_MAX, BinderError::SoldOut);
        let public_index = s
            .public_minted
            .checked_add(1)
            .ok_or_else(|| error!(BinderError::MathOverflow))?;
        genesis_eligible = mint_eligibility(s, false, public_index);
        s.public_minted = public_index;
        s.next_public = number
            .checked_add(1)
            .ok_or_else(|| error!(BinderError::MathOverflow))?;
    }
    require!(
        bit_set(&mut s.minted_mask, number),
        BinderError::NumberTaken
    );
    s.minted = s
        .minted
        .checked_add(1)
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    s.active = s
        .active
        .checked_add(1)
        .ok_or_else(|| error!(BinderError::MathOverflow))?;
    if genesis_eligible {
        s.genesis_eligible_count = s
            .genesis_eligible_count
            .checked_add(1)
            .ok_or_else(|| error!(BinderError::MathOverflow))?;
    }
    Ok(Registered {
        seq: s.minted,
        genesis_eligible,
    })
}

/// What a sync tells the indexer: one `PotSynced` (when something was realized) and one `RoundClosed` per filled card round.
/// The handler emits them with `emit_cpi!` (a macro that needs the handler's own `ctx`).
pub struct SyncEvents {
    pub synced: Option<crate::events::PotSynced>,
    pub closed: Vec<crate::events::RoundClosed>,
}

/// Realizes the free income of the pot for the Binders that exist and returns the events to emit.
pub fn sync_pot_state(
    s: &mut State,
    pot_lamports: u64,
    rent_min: u64,
    threshold: u64,
) -> Result<SyncEvents> {
    let income = free_income(pot_lamports, rent_min, s)?;
    let out = sync(s, income, threshold)?;
    let synced = (out.realized > 0).then_some(crate::events::PotSynced {
        realized: out.realized,
        credited: out.credited,
        to_cards: out.to_cards,
        minted: s.minted,
        active: s.active,
        acc_per_binder: s.acc_per_binder,
        round_kind: s.round_kind,
        round_index: s.round_index,
        round_progress: s.round_progress,
    });
    let closed = out
        .closed
        .iter()
        .map(|c| crate::events::RoundClosed {
            round_index: c.index,
            budget: c.budget,
            snapshot_seq: c.snapshot_seq,
        })
        .collect();
    Ok(SyncEvents { synced, closed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::team_mask_from;

    /// The real team shape: #100, #15, #67, #86 and the 26 lowest others from #2.
    fn real_team() -> Vec<u16> {
        let mut v = vec![100u16, 15, 67, 86];
        let mut n = 2u16;
        while v.len() < 30 {
            if !v.contains(&n) {
                v.push(n);
            }
            n += 1;
        }
        v
    }

    const THRESHOLD: u64 = 5_000_000_000;
    const RENT: u64 = 890_880;

    fn state_with(n: u16) -> State {
        let mut s = State::new(255);
        s.minted = n;
        s.active = n;
        s
    }

    /// A tiny model of the pot: the lamports of the vault, the Binders' snapshots and the totals paid.
    struct Model {
        s: State,
        lamports: u64,
        snapshots: Vec<u128>,
        eligible: Vec<bool>,
        claimed: Vec<bool>,
        forfeited: Vec<bool>,
        inflow: u128,
        paid: u128,
    }

    impl Model {
        fn new() -> Self {
            Model {
                s: State::new(255),
                lamports: RENT,
                snapshots: vec![],
                eligible: vec![],
                claimed: vec![],
                forfeited: vec![],
                inflow: 0,
                paid: 0,
            }
        }

        fn income_in(&mut self, x: u64) {
            self.lamports += x;
            self.inflow += x as u128;
        }

        fn sync(&mut self) -> SyncOutcome {
            let income = free_income(self.lamports, RENT, &self.s).unwrap();
            sync(&mut self.s, income, THRESHOLD).unwrap()
        }

        /// A team or public mint in the program's order: sync until nothing is left (the client puts as many sync_pot in
        /// front as the backlog needs, review R1), then count, snapshot = accumulator now.
        fn mint(&mut self, is_team: bool, number: u16) -> usize {
            self.sync();
            while self.s.active > 0 && free_income(self.lamports, RENT, &self.s).unwrap() > 0 {
                self.sync();
            }
            require_synced(self.lamports, RENT, &self.s).unwrap();
            let r = register_mint(&mut self.s, number, is_team).unwrap();
            self.snapshots.push(self.s.acc_per_binder);
            self.eligible.push(r.genesis_eligible);
            self.claimed.push(false);
            self.forfeited.push(false);
            self.snapshots.len() - 1
        }

        /// A burned Binder is forfeited: a sync first, then the release (as `forfeit_burned` does).
        fn forfeit(&mut self, i: usize) -> Forfeited {
            self.sync();
            let f = forfeit(
                &mut self.s,
                self.snapshots[i],
                self.eligible[i],
                self.claimed[i],
            )
            .unwrap();
            self.forfeited[i] = true;
            f
        }

        fn claim(&mut self, i: usize) -> u64 {
            if self.forfeited[i] {
                return 0; // a burned Binder has no asset to claim with
            }
            self.sync();
            let pend = genesis_pending(&self.s, self.eligible[i], self.claimed[i]);
            let c = claim_amounts(&self.s, self.snapshots[i], pend).unwrap();
            let total = c.total().unwrap();
            apply_claim(&mut self.s, &c).unwrap();
            self.snapshots[i] = c.new_snapshot;
            if pend {
                self.claimed[i] = true;
            }
            self.lamports -= total;
            self.paid += total as u128;
            total
        }

        fn check(&self) {
            check_identity(self.lamports, RENT, &self.s).unwrap();
            // the accumulator identity over the ACTIVE Binders: what is owed is exactly the sum of what each can still claim
            let mut owed = 0u128;
            let mut active = 0u16;
            for (i, snapshot) in self.snapshots.iter().enumerate() {
                if !self.forfeited[i] {
                    owed += self.s.acc_per_binder - snapshot;
                    active += 1;
                }
            }
            assert_eq!(
                owed, self.s.owed_scaled,
                "owed_scaled is the sum over the active Binders"
            );
            assert_eq!(active, self.s.active);
            assert!(
                self.paid <= self.inflow,
                "paid {} > inflow {}",
                self.paid,
                self.inflow
            );
        }
    }

    #[test]
    fn rounding_dust_never_over_credits() {
        // design 5.2: three Binders, four syncs of 1 lamport. A reserve counted in whole lamports would say 0 while every
        // Binder can claim 1 lamport after the fourth sync. The exact reserve (ceil of owed_scaled) covers it.
        let mut m = Model::new();
        for n in 0..3 {
            m.mint(true, n + 1);
        }
        for _ in 0..4 {
            m.income_in(1);
            m.sync();
            m.check();
        }
        // 4 lamports came in, at most 3 can be claimed (one each) and the identity still holds
        let claimed: u64 = (0..3).map(|i| m.claim(i)).sum();
        assert!(claimed <= 4);
        m.check();
    }

    #[test]
    fn nothing_realized_without_binders() {
        let mut s = state_with(0);
        let out = sync(&mut s, 1_000_000_000, THRESHOLD).unwrap();
        assert_eq!(out, SyncOutcome::default());
        assert_eq!(s.acc_per_binder, 0);
        assert_eq!(s.income_realized_total, 0);
    }

    #[test]
    fn rounds_split_exactly_at_the_threshold() {
        // 12 SOL of income with a 5 SOL threshold and 10 Binders: a SOL round of exactly 5 SOL, a card budget of exactly
        // 5 SOL (the round closes), then 2 SOL start the next SOL round
        let mut s = state_with(10);
        let out = sync(&mut s, 12_000_000_000, THRESHOLD).unwrap();
        assert_eq!(out.realized, 12_000_000_000);
        assert_eq!(out.credited, 7_000_000_000);
        assert_eq!(out.to_cards, 5_000_000_000);
        assert_eq!(
            out.closed,
            vec![ClosedRound {
                index: 0,
                budget: THRESHOLD,
                snapshot_seq: 10
            }]
        );
        assert_eq!(s.round_kind, ROUND_SOL);
        assert_eq!(s.round_progress, 2_000_000_000);
        assert_eq!(s.round_index, 1);
        assert_eq!(s.card_withdrawable, THRESHOLD);
        // the accumulator moved by 7 SOL / 10 Binders
        assert_eq!(s.acc_per_binder, 700_000_000 * SCALE);
        // the identity: 7 SOL owed + 5 SOL withdrawable
        assert_eq!(reserved_lamports(&s).unwrap(), 12_000_000_000);
    }

    #[test]
    fn a_step_limit_leaves_the_rest_for_the_next_call() {
        // 100 SOL in one go: four steps of 5 SOL (SOL round, card round, SOL round, card round: two card rounds close),
        // the remaining 80 SOL stay income
        let mut s = state_with(10);
        let out = sync(&mut s, 100_000_000_000, THRESHOLD).unwrap();
        assert_eq!(out.realized, 4 * THRESHOLD);
        assert_eq!(out.closed.len(), 2);
        let again = sync(&mut s, 80_000_000_000, THRESHOLD).unwrap();
        assert_eq!(again.realized, 4 * THRESHOLD);
    }

    #[test]
    fn a_mint_needs_a_synced_pot_and_a_complete_sync_leaves_nothing() {
        // review R1: 60 SOL of income is 12 round steps; one sync does four, so the pot is not synced after one or two
        let mut s = state_with(30);
        let income = 60_000_000_000u64;
        let pot = RENT + income;
        let mut left = income;
        let mut syncs = 0;
        loop {
            let free = free_income(pot, RENT, &s).unwrap();
            assert_eq!(free, left);
            if free == 0 {
                assert!(require_synced(pot, RENT, &s).is_ok());
                break;
            }
            assert!(require_synced(pot, RENT, &s).is_err());
            let out = sync(&mut s, free, THRESHOLD).unwrap();
            left -= out.realized;
            syncs += 1;
        }
        assert_eq!(syncs, 3, "12 steps at four a call");
        // the remainder that is not a whole step is realized too: nothing is left over for a later Binder
        let mut s = state_with(7);
        let pot = RENT + 123_456_789;
        let out = sync(&mut s, 123_456_789, THRESHOLD).unwrap();
        assert_eq!(out.realized, 123_456_789);
        assert!(require_synced(pot, RENT, &s).is_ok());
        // a single lamport is realizable as well (the accumulator always moves)
        let mut s = state_with(1000);
        let pot = RENT + 1;
        assert!(require_synced(pot, RENT, &s).is_err());
        sync(&mut s, 1, THRESHOLD).unwrap();
        assert!(require_synced(pot, RENT, &s).is_ok());
    }

    #[test]
    fn the_complete_sync_rule_holds_for_one_active_binder_and_is_skipped_only_for_none() {
        // review R1 with exactly one active Binder: a backlog is refused (the skip is for zero only)
        for active in [1u16, 2, 30] {
            let s = state_with(active);
            assert!(
                require_synced(RENT + 1, RENT, &s).is_err(),
                "active {active}"
            );
            assert!(require_synced(RENT, RENT, &s).is_ok(), "active {active}");
        }
        let none = state_with(0);
        assert!(require_synced(RENT + 1_000, RENT, &none).is_ok());
    }

    #[test]
    fn the_mints_own_payment_goes_to_earlier_binders_not_to_the_new_one() {
        // review note 2: 0.30 SOL arrives, sync runs for the 30 team Binders, then the new Binder is counted
        let mut m = Model::new();
        for i in 0..30u16 {
            m.mint(true, i + 2);
        }
        m.income_in(300_000_000); // the mint's own 0.30 SOL
        let new = m.mint(false, 1); // syncs first, then counts the new Binder
        assert_eq!(
            m.claim(new),
            0,
            "the new Binder must get nothing from its own mint"
        );
        let earlier: u64 = (0..30).map(|i| m.claim(i)).sum();
        assert!(
            (300_000_000 - 30..=300_000_000).contains(&earlier),
            "earlier Binders get the 0.30 SOL (minus rounding dust): {earlier}"
        );
        m.check();
    }

    #[test]
    fn a_binder_minted_later_gets_nothing_from_earlier_income() {
        let mut m = Model::new();
        m.mint(true, 2);
        m.income_in(1_000_000_000);
        m.sync();
        let late = m.mint(true, 3);
        assert_eq!(m.claim(late), 0);
        assert!(m.claim(0) >= 999_999_999);
        // income after the second mint is shared by both
        m.income_in(2_000_000_000);
        let a = m.claim(0);
        let b = m.claim(late);
        assert!(a.abs_diff(b) <= 1 && a + b >= 1_999_999_998);
        m.check();
    }

    #[test]
    fn genesis_is_shared_equally_once_and_the_remainder_returns_to_income() {
        let mut m = Model::new();
        m.income_in(10_000_000_001); // 10 SOL and 1 lamport gathered before the opening
                                     // the opening: lock the free balance, then mint the 30 team Binders
        lock_genesis(&mut m.s, m.lamports, RENT).unwrap();
        assert_eq!(m.s.genesis_locked, 10_000_000_001);
        check_identity(m.lamports, RENT, &m.s).unwrap();
        for i in 0..30u16 {
            m.mint(true, i + 2);
        }
        assert_eq!(m.s.genesis_eligible_count, 30);
        let g = share_genesis(&mut m.s).unwrap();
        assert_eq!(g.eligible, 30);
        assert_eq!(g.per_binder, 333_333_333);
        assert_eq!(g.remainder, 10_000_000_001 - 333_333_333 * 30);
        assert_eq!(m.s.genesis_locked, 0);
        // the remainder is ordinary income now
        assert_eq!(free_income(m.lamports, RENT, &m.s).unwrap(), g.remainder);
        // each eligible Binder is paid once
        let first = m.claim(0);
        assert_eq!(first, 333_333_333); // the 11 lamports of remainder are less than one lamport each
        let second = m.claim(0);
        assert_eq!(second, 0);
        m.check();
        // sharing twice is refused
        assert!(share_genesis(&mut m.s).is_err());
    }

    #[test]
    fn binders_minted_after_the_sharing_are_not_eligible() {
        // review note 3: finalize at 24 h with fewer than 100 public mints; later public mints are not eligible
        let mut m = Model::new();
        m.income_in(3_000_000_000);
        lock_genesis(&mut m.s, m.lamports, RENT).unwrap();
        for i in 0..30u16 {
            m.mint(true, i + 2);
        }
        let early = m.mint(false, 1); // public mint 1, before the sharing: eligible
        assert!(m.eligible[early]);
        assert_eq!(m.s.genesis_eligible_count, 31);
        share_genesis(&mut m.s).unwrap();
        let late = m.mint(false, 32); // public mint 2 (still under 100), after the sharing
        assert!(
            !m.eligible[late],
            "a public mint after finalize_genesis must not be eligible"
        );
        assert_eq!(m.s.genesis_eligible_count, 31);
        assert_eq!(m.claim(late), 0);
        assert!(m.claim(early) >= 96_774_193);
        m.check();
    }

    #[test]
    fn the_100th_public_mint_is_eligible_the_101st_is_not() {
        let team = team_mask_from(&real_team()).unwrap();
        let mut s = State::new(255);
        s.minted_mask = team;
        s.minted = 30;
        s.reserve_minted = 30;
        s.genesis_eligible_count = 30;
        let mut number = 1u16;
        for index in 1..=101u16 {
            while bit_get(&s.minted_mask, number) {
                number += 1;
            }
            let r = register_mint(&mut s, number, false).unwrap();
            assert_eq!(r.genesis_eligible, index <= 100, "public mint {index}");
        }
        assert_eq!(s.genesis_eligible_count, 130);
    }

    #[test]
    fn register_mint_enforces_the_caps_and_never_repeats_a_number() {
        let mut s = State::new(255);
        for i in 0..30u16 {
            register_mint(&mut s, i + 2, true).unwrap();
        }
        assert!(
            register_mint(&mut s, 40, true).is_err(),
            "the 31st team Binder"
        );
        assert!(
            register_mint(&mut s, 2, false).is_err(),
            "a number that exists"
        );
        assert_eq!(s.minted, 30);
        assert_eq!(s.reserve_minted, 30);
    }

    #[test]
    fn supply_never_exceeds_1000() {
        let team = real_team();
        let team_mask = team_mask_from(&team).unwrap();
        let mut s = State::new(255);
        for n in &team {
            register_mint(&mut s, *n, true).unwrap();
        }
        let mut cursor = 1;
        for _ in 0..PUBLIC_MAX {
            let n = crate::math::next_public_number(&team_mask, &s.minted_mask, cursor).unwrap();
            register_mint(&mut s, n, false).unwrap();
            cursor = n + 1;
        }
        assert_eq!(s.minted, MAX_SUPPLY);
        assert_eq!(s.public_minted, PUBLIC_MAX);
        assert!(register_mint(&mut s, 1, false).is_err());
    }

    #[test]
    fn claim_keeps_the_fraction() {
        let mut s = state_with(3);
        s.acc_per_binder = 10 * SCALE + SCALE / 3; // 10.333.. lamports per Binder
        s.owed_scaled = 3 * s.acc_per_binder;
        let c = claim_amounts(&s, 0, false).unwrap();
        assert_eq!(c.acc_share, 10);
        assert_eq!(c.new_snapshot, 10 * SCALE);
        // the 0.333 lamport stays claimable later
        s.acc_per_binder += SCALE; // one more lamport arrives
        let c2 = claim_amounts(&s, c.new_snapshot, false).unwrap();
        assert_eq!(c2.acc_share, 1, "0.333 + 1 = 1.333 rounds down to 1");
    }

    // ---- a deterministic random model: identity and solvency after every step ----------------------------------

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[test]
    fn random_sequences_keep_the_pot_identity_and_solvency() {
        for seed in 1..=40u64 {
            let mut rng = Lcg(seed * 7919);
            let mut m = Model::new();
            m.income_in(rng.below(20_000_000_000));
            lock_genesis(&mut m.s, m.lamports, RENT).unwrap();
            let mut next_team = 2u16;
            let mut next_public = 1u16;
            let mut shared = false;
            for step in 0..400u32 {
                match rng.below(9) {
                    8 => {
                        // a burn and its forfeit: a random Binder that is still active
                        if !m.snapshots.is_empty() {
                            let i = rng.below(m.snapshots.len() as u64) as usize;
                            if !m.forfeited[i] && m.s.active > 0 {
                                m.forfeit(i);
                            }
                        }
                    }
                    0 | 1 => m.income_in(rng.below(7_000_000_000)),
                    2 => {
                        m.sync();
                    }
                    3 | 4 => {
                        if m.s.reserve_minted < 30 {
                            m.mint(true, next_team);
                            next_team += 1;
                        } else if m.s.public_minted < 970 {
                            m.mint(false, next_public);
                            next_public += 1;
                            // the team here is 2..=31: public numbers skip it
                            while (2..=31).contains(&next_public) {
                                next_public += 1;
                            }
                        }
                    }
                    5 => {
                        if m.s.reserve_minted == 30 && !shared {
                            share_genesis(&mut m.s).unwrap();
                            shared = true;
                        }
                    }
                    _ => {
                        if !m.snapshots.is_empty() {
                            let i = rng.below(m.snapshots.len() as u64) as usize;
                            m.claim(i);
                        }
                    }
                }
                m.check();
                assert!(m.s.round_progress < THRESHOLD, "seed {seed} step {step}");
            }
            // everything every Binder can still claim is covered by the pot: claim them all, twice
            for _ in 0..2 {
                for i in 0..m.snapshots.len() {
                    m.claim(i);
                    m.check();
                }
            }
            // dust stays reserved: less than one lamport per Binder plus the last sync's rounding
            let dust = ceil_div(m.s.owed_scaled, SCALE).unwrap();
            assert!(
                dust <= 2 * (m.snapshots.len() as u128 + 1),
                "seed {seed}: {dust} lamports of dust"
            );
            if shared {
                assert_eq!(
                    m.s.genesis_owed, 0,
                    "seed {seed}: every genesis share was paid"
                );
            }
        }
    }

    #[test]
    fn a_forfeit_releases_the_share_and_only_the_remaining_binders_get_it() {
        let mut m = Model::new();
        for n in 0..4 {
            m.mint(true, n + 1);
        }
        m.income_in(1_000_000_000); // inside the first SOL round
        m.sync();
        let r = m.s.acc_per_binder - m.snapshots[0]; // 250,000,000 x SCALE
        assert_eq!(r, 250_000_000 * SCALE);
        let owed = m.s.owed_scaled;
        let f = m.forfeit(0);
        assert_eq!(f.released_acc_scaled, r);
        assert_eq!(m.s.owed_scaled, owed - r);
        assert_eq!((m.s.minted, m.s.active), (4, 3)); // the supply counter stays, the divisor drops
        m.check();
        // the released lamports are free income now, to the lamport
        assert_eq!(
            free_income(m.lamports, RENT, &m.s).unwrap(),
            250_000_000,
            "the share is income again"
        );
        // the next sync credits it to the three that remain: the forfeited Binder is out of the divisor
        m.sync();
        let each = (m.s.acc_per_binder - m.snapshots[1]) / SCALE;
        assert_eq!(
            each,
            250_000_000 + 83_333_333,
            "its own share and a third of the released one"
        );
        assert_eq!(m.claim(0), 0, "a forfeited Binder is paid nothing");
        let paid: u64 = (1..4).map(|i| m.claim(i)).sum();
        assert!(
            (1_000_000_000 - 3..=1_000_000_000).contains(&paid),
            "{paid}"
        );
        m.check();
    }

    #[test]
    fn a_forfeit_before_genesis_is_shared_leaves_the_eligible_set_and_after_it_releases_the_share()
    {
        // before: no share exists, the Binder only leaves the eligible set
        let mut m = Model::new();
        m.income_in(700_000_000);
        lock_genesis(&mut m.s, m.lamports, RENT).unwrap();
        for n in 0..7 {
            m.mint(true, n + 1);
        }
        assert_eq!(m.s.genesis_eligible_count, 7);
        let f = m.forfeit(2);
        assert_eq!(f.released_genesis, 0);
        assert_eq!(m.s.genesis_eligible_count, 6);
        let g = share_genesis(&mut m.s).unwrap();
        assert_eq!((g.eligible, g.per_binder, g.remainder), (6, 116_666_666, 4));
        m.check();
        // after: an unpaid eligible Binder releases its share, one that claimed it releases none
        assert_eq!(m.claim(0), 116_666_666);
        let owed = m.s.genesis_owed;
        assert_eq!(m.forfeit(0).released_genesis, 0);
        assert_eq!(m.s.genesis_owed, owed);
        assert_eq!(m.forfeit(1).released_genesis, 116_666_666);
        assert_eq!(m.s.genesis_owed, owed - 116_666_666);
        assert_eq!(m.s.genesis_eligible_count, 6, "fixed at the sharing");
        m.check();
    }

    #[test]
    fn with_no_active_binder_income_waits_and_the_first_binder_after_it_gets_it() {
        let mut m = Model::new();
        m.mint(true, 1);
        m.mint(true, 2);
        m.forfeit(0);
        m.forfeit(1);
        assert_eq!((m.s.minted, m.s.active), (2, 0));
        m.income_in(3_000_000_000);
        let out = m.sync();
        assert_eq!(out, SyncOutcome::default(), "nobody to credit: a no-op");
        assert!(
            require_synced(m.lamports, RENT, &m.s).is_ok(),
            "a mint is not blocked"
        );
        m.check();
        let i = m.mint(false, 3);
        assert_eq!(m.s.active, 1);
        m.sync();
        assert_eq!(
            m.claim(i),
            3_000_000_000,
            "the waiting income is the first Binder's"
        );
        // the last eligible Binder gone before the sharing: zero eligible, nothing to divide by
        let mut s = State::new(255);
        s.genesis_locked = 700;
        let g = share_genesis(&mut s).unwrap();
        assert_eq!((g.eligible, g.per_binder, g.remainder), (0, 0, 700));
    }

    #[test]
    fn the_identity_fails_loudly_when_the_pot_is_short() {
        let mut s = state_with(10);
        s.owed_scaled = 5 * SCALE;
        assert!(check_identity(RENT + 4, RENT, &s).is_err());
        assert!(check_identity(RENT + 5, RENT, &s).is_ok());
        // below the rent minimum is broken too
        assert!(check_identity(RENT - 1, RENT, &state_with(0)).is_err());
    }

    #[test]
    fn lock_genesis_takes_only_the_free_balance() {
        let mut s = state_with(0);
        let locked = lock_genesis(&mut s, RENT + 777, RENT).unwrap();
        assert_eq!(locked, 777);
        assert_eq!(free_income(RENT + 777, RENT, &s).unwrap(), 0);
    }
}
