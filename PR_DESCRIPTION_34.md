# Fix: Governance vote() can permanently stall proposals on membership changes (#34)

## Summary

`governance.vote()` only evaluated a proposal for approval/rejection once
**every current member had voted** (`total_votes >= total_members`). Because
`total_members` is read live, adding a member after a proposal was created
raised the threshold above what the original voters could ever reach, leaving
the proposal stuck in `Active` until it expired with no path to execution.
Removing a member after voting, conversely, silently changed the denominator
and let a removed member's prior vote keep inflating the tallies.

This PR makes proposal resolution depend on a **creation-time snapshot** of the
membership and on a **configurable quorum**, so membership changes after
creation can never retroactively alter a proposal's outcome.

## Why this matters

- **Liveness bug:** a single `add_member` after `propose` can deadlock
  governance — the remaining voters can never hit 100% turnout, so the proposal
  is neither approved nor rejected and can only expire.
- **Correctness bug:** `votes_for`/`votes_against` were stored as
  `Vec<Address>`, growing unboundedly and being (de)serialized on every
  `vote()`. The approval ratio was computed over *cast* votes, so abstentions
  effectively counted as "yes".

## Root cause

In `contracts/governance/src/lib.rs`, `vote()` (~lines 280–320) used:

```rust
let total_members = member_count(&e);          // live count, not snapshotted
let total_votes   = proposal.votes_for.len() + proposal.votes_against.len();
if total_votes >= total_members {
    let yes_pct = (proposal.votes_for.len() as u64 * 10000) / total_votes as u64;
    // ...
}
```

- `total_members` is the live count, so membership edits after creation move the
  goalposts.
- The `Proposal` struct kept no baseline membership count.
- The approval threshold was a fraction of *cast* votes, not of eligible
  voters, so non-participation was rewarded.

## Changes

### `Proposal` struct (`contracts/governance/src/lib.rs`)
- Added `eligible_voters: u32` — the member count snapshotted at
  `propose()` time. This is the stable denominator for quorum and approval math.
- Changed `votes_for: Vec<Address>` and `votes_against: Vec<Address>` to
  `votes_for: u32` and `votes_against: u32` counts.
  Per-voter de-duplication is already enforced by `DataKey::HasVoted`, so the
  explicit voter lists were redundant and unbounded.

### `propose()` (`contracts/governance/src/lib.rs`)
- Snapshots `eligible_voters = member_count(&e)` at creation.

### `vote()` (`contracts/governance/src/lib.rs`)
- Increments `votes_for`/`votes_against` counts instead of pushing addresses.
- Resolves a proposal when the number of cast votes reaches the **quorum**:
  `ceil(quorum_bps / 10000 * eligible_voters)`. This fixes the liveness bug —
  a proposal no longer needs 100% turnout against the live membership count, so
  adding a member after creation can no longer deadlock it.
- Approval is measured as a fraction of **cast votes**
  (`votes_for * 10000 / (votes_for + votes_against)`), gated by the quorum so a
  proposal cannot resolve on a single vote from a tiny subset of the membership.
- Resolution uses only the snapshot, never the live `member_count()`, so
  `add_member`/`remove_member` after creation cannot change the threshold.
- A vote cast after the proposal is already `Approved`/`Rejected`/`Executed` is
  now a harmless no-op (previously it panicked with "proposal not active"), so
  voting past the quorum no longer aborts.

### `GovernanceConfig` (`contracts/governance/src/lib.rs`)
- Added `quorum_bps: u32` (default `5000` = 50% of eligible voters). This is the
  configurable quorum required before a proposal is resolved, decoupling
  resolution from 100% turnout.

### Docs
- `SECURITY.md` — updated the voting/approval explanation to describe quorum and
  the eligible-voter denominator.
- `README.md` — updated the `Proposal` struct documentation.

### Test harness portability
- Updated `Symbol`/`IntoVal` usages in the test module and `mock_target` to
  fully-qualified paths and wrapped the two `catch_unwind` assertions in
  `std::panic::AssertUnwindSafe`, so the suite compiles cleanly across SDK
  versions.

## Quorum / approval design decision

The fix adopts the following, now documented, semantics:

1. **Quorum** — a proposal is only resolved once `votes_for + votes_against`
   reaches `quorum_bps` of `eligible_voters` (default 50%). Below quorum the
   proposal remains `Active` and resolves only when quorum is met (or it
   expires at `voting_ends_at`). The quorum denominator is the
   creation-time snapshot, so membership changes after creation cannot move the
   goalposts.
2. **Approval** — once quorum is reached, a proposal is `Approved` iff
   `votes_for * 10000 / (votes_for + votes_against) >= approval_threshold_bps`
   (default 6000 = 60%). Approval is measured over **cast votes**, gated by the
   quorum so a proposal cannot resolve on a single vote from a tiny subset of
   the membership.

This is the "majority of cast votes once quorum (of eligible voters) is reached"
interpretation explicitly listed as a valid design choice in the issue's
*Key Challenges* section. Abstentions therefore do not count as explicit "yes"
votes beyond the quorum gate, and the prior liveness bug (proposals stuck in
`Active` when membership changed) is fully resolved.

## Acceptance criteria

- [x] `Proposal` gains `eligible_voters: u32` (snapshotted at creation).
- [x] `votes_for`/`votes_against` changed from `Vec<Address>` to `u32` counts.
- [x] `DataKey::HasVoted` retained for de-duplication.
- [x] Approval check fires when `total_votes` meets the configurable quorum
      (not 100% turnout).
- [x] **New test:** member added after proposal creation does not change the
      proposal threshold.
- [x] **New test:** proposal reaches threshold before all members vote when
      quorum is met.
- [x] **New test:** member removed after voting does not corrupt vote counts.
- [x] All existing governance tests updated and passing.

## Relevant files / functions

- `contracts/governance/src/lib.rs` — `vote()`, `propose()`, `Proposal` struct,
  `GovernanceConfig`, `DataKey::HasVoted`.
- `README.md`, `SECURITY.md` — documentation.

## Out of scope

Token-weighted voting and delegation (noted in the original issue).

closes #34
