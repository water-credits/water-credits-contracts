# Fix: Oracle honors token `max_supply` cap instead of panicking on finalization (Issue #36)

## Summary

`credit_token` enforces a `max_supply` cap inside `mint_to`. The verification
oracle's `submit_reading()` (and the commit-reveal `finalize_reveals()` path)
called `mint_to` with the full `res.total_credits` **without first checking how
close the project was to its supply ceiling**.

When `total_supply + total_credits > max_supply`, `mint_to` panics, which rolled
back the *entire* oracle finalization transaction. The window was left in a
broken state and the project was permanently unable to finalize another window
until an admin called `reset_window`. A project approaching its credit ceiling
would experience repeated finalization failures with no actionable error.

This PR fixes the oracle to clamp the mint amount to the token's remaining
allowance *before* calling `mint_to`, so finalization can never be rolled back
by a cap breach, and records exactly how many credits were actually minted.

## Root Cause

In `contracts/verification_oracle/src/lib.rs`, the mint logic was:

```rust
if res.total_credits > 0 {
    if let Some(config) = e.storage().persistent().get::<_, ProjectConfig>(&cfg_key) {
        let mint_args = vec![/* ... res.total_credits ... */];
        e.invoke_contract::<()>(&config.token_contract, &Symbol::new(&e, "mint_to"), mint_args);
        // ← no pre-check of token.max_supply() / token.total_supply()
    }
}
```

If `token.total_supply() + res.total_credits > token.max_supply()`, the
cross-contract `mint_to` panicked and the whole finalization reverted.

## Changes

### `contracts/verification_oracle/src/lib.rs`

1. **New helper `mint_credits_respecting_cap`** — performs two cross-contract
   read calls (`total_supply()`, `max_supply()`) on the token, then computes the
   mintable amount:

   ```text
   mintable = if max_supply > 0 {
       remaining = max_supply - total_supply
       if remaining <= 0 { 0 } else { remaining.min(total_credits) }
   } else {
       total_credits   // uncapped token
   }
   ```

   It mints exactly `mintable` (or skips the mint entirely when `mintable <= 0`)
   and returns the amount actually minted. This guarantees `mint_to` can never
   exceed the cap, so it can never panic on a cap breach.

2. **`VerificationResult.credits_minted: i128`** — new field that distinguishes
   *credits earned* (`total_credits`) from *credits actually minted*
   (`credits_minted`). This is an ABI change for `get_last_result()` and
   `get_result_history()` callers, who must now read the extra field.

3. **`submit_reading_impl` and `finalize_reveals`** — the mint now runs **before**
   the result is persisted, so `credits_minted` is recorded accurately, and so a
   cap breach can never roll back finalization (Issue #36). Both paths read the
   project's `ProjectConfig` and call `mint_credits_respecting_cap`.

4. **Public `submit_reading`** — the ad-hoc, unbounded `mint_to` block was removed;
   minting is now handled uniformly inside the impl (which also covers the
   commit-reveal finalization path, fixing the same latent bug there).

### `tests/tests/verification_oracle_supply_cap.rs` (new)

End-to-end integration tests using the **real** `credit_token` contract:

- `test_mint_respects_max_supply_cap_partial` — `max_supply = 100`,
  `total_supply = 90`, oracle computes **50** credits → only **10** are minted,
  `credits_minted == 10`, window finalizes, token `total_supply == 100`.
- `test_mint_at_exact_max_supply_no_panic` — `max_supply = 100`,
  `total_supply = 100` → oracle finalizes with `credits_minted == 0`, **no
  panic**, window is not left broken.
- `test_mint_uncapped_when_max_supply_zero` — `max_supply == 0` (uncapped) →
  full **50** credits minted, `credits_minted == 50`.

## Acceptance Criteria Coverage

- [x] Oracle reads `total_supply()` and `max_supply()` from the token before calling `mint_to`.
- [x] When `max_supply > 0` and `total_supply + total_credits > max_supply`, oracle mints only `max_supply - total_supply`.
- [x] When the remaining mintable amount is `0`, no `mint_to` call is made and the window finalizes cleanly.
- [x] `VerificationResult` gains `credits_minted: i128` field.
- [x] Test: `max_supply = 100`, `total_supply = 90`, 50 credits computed → only 10 minted, `credits_minted == 10`.
- [x] Test: project at exactly `max_supply` → finalizes with `credits_minted == 0`, no panic.

## Design Notes / Decisions

- **Un-minted amount is effectively "lost"** for that window (no pending-mint
  queue). This is the simplest, safest semantic: the beneficiary receives every
  credit the project is still entitled to under its cap, and the shortfall simply
  reflects that the project has exhausted its approved supply. A pending-mint
  queue (explicitly **out of scope**) could be added later in governance.
- The partial-mint semantics are: *credits earned* (`total_credits`) is always
  the full environmental result; *credits minted* (`credits_minted`) is what was
  actually issued, bounded by the cap.
- Cross-contract read calls add two extra invocations per finalization
  (`total_supply` + `max_supply`), increasing gas/instruction cost marginally,
  which is an accepted trade-off for correctness.

## Labels

`type: bug`, `type: correctness`, `difficulty: advanced`, `area: verification-oracle`, `area: credit-token`, `priority: high`

## Test Plan

```sh
cargo test -p tests --features testutils max_supply
```

All three new tests pass, and the full `tests` suite passes with no regressions.
WASM builds for `verification_oracle` and `credit_token` succeed.

closes #36
