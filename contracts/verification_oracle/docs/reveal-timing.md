# Reveal Phase Timing Model

## Hybrid Time/Ledger Design

The verification oracle uses a hybrid timing model that combines wall-clock time and ledger-based counters:

| Parameter | Unit | Purpose |
|-----------|------|---------|
| `commit_phase_secs` | Seconds (wall clock) | Human-readable, operator-friendly time window for commit phase |
| `min_reveal_ledgers` | Ledger count | Precise, immune to clock manipulation; enforces MEV protection gap |
| `max_reveal_ledgers` | Ledger count | Precise, immune to clock manipulation; defines reveal window boundary |

## MEV Protection via min_reveal_ledgers

`min_reveal_ledgers` enforces a mandatory gap between commit and reveal phases. This is the primary defense against frontrunning attacks.

### The Problem (min_reveal_ledgers = 0)

Without a ledger-based gap (the old default of 0), an attacker can:

1. Watch the mempool for the `begin_reveal_phase` transaction
2. Observe it lands in ledger N, transitioning the window to Reveal phase
3. Submit a competing reveal transaction in the **same ledger N**
4. React to legitimate reveals before they are confirmed

This "same-ledger reaction" undermines the commit-reveal scheme entirely because the attacker gains risk-free information about honest submissions.

### The Solution (min_reveal_ledgers ≥ 1)

With `min_reveal_ledgers = 5` (the new default):

- The reveal phase opens at ledger N
- No reveals are accepted until ledger N + 5 (after 5 ledgers have passed)
- Attackers cannot react and submit in the same ledger or within the gap
- Honest reveals succeed after the mandatory delay

## Ledger Cadence on Stellar Mainnet

Stellar mainnet produces approximately **1 ledger every 5 seconds**.

### Recommended Values

| Security Level | min_reveal_ledgers | Approximate Delay | Use Case |
|---|---|---|---|
| **Minimum (default)** | 5 | ~25 seconds | Most deployments; good MEV protection with low latency overhead |
| **Standard** | 12 | ~60 seconds | Higher security; 1 minute gap between commit and reveal |
| **High** | 24 | ~2 minutes | Very sensitive deployments; maximum protection |
| **Maximum** | 60 | ~5 minutes | Extreme caution; rarely needed |

Operators may increase `min_reveal_ledgers` for additional security. Decrease only with careful justification after security review.

## Setting Values Coherently

When configuring the oracle, ensure the timing parameters are compatible:

### Example 1: 1-Hour Commit Window

```
commit_phase_secs = 3600 (60 minutes)
min_reveal_ledgers = 12 (60 seconds at 5s/ledger)
max_reveal_ledgers = 720 (60 minutes at 5s/ledger)
```

**Interpretation:**
- Operators have 60 minutes to call `begin_reveal_phase` after opening the window
- Reveals are blocked for the first 60 seconds after the phase opens (MEV protection)
- Honest participants have a 60-minute window to reveal after that

### Example 2: 10-Minute Commit Window

```
commit_phase_secs = 600 (10 minutes)
min_reveal_ledgers = 5 (25 seconds at 5s/ledger)
max_reveal_ledgers = 120 (10 minutes at 5s/ledger)
```

**Interpretation:**
- Operators have 10 minutes to transition to reveal phase
- Reveals are blocked for the first 25 seconds (MEV protection)
- Honest participants have approximately a 10-minute window to reveal

### Example 3: Defensive Configuration

```
commit_phase_secs = 1800 (30 minutes)
min_reveal_ledgers = 24 (2 minutes at 5s/ledger)
max_reveal_ledgers = 360 (30 minutes at 5s/ledger)
```

**Interpretation:**
- Conservative configuration with 2-minute reveal delay
- Suitable for high-value data feeds or sensitive deployments

## Time vs Ledger: Why Both?

**Time-based (`commit_phase_secs`):**
- Human-readable and operator-friendly
- Easy to reason about and configure
- Subject to clock skew, network delays, and adversarial timestamps

**Ledger-based (`min_reveal_ledgers`, `max_reveal_ledgers`):**
- Immune to clock manipulation
- Precise and deterministic
- Directly tied to network consensus finality
- Perfect for cryptographic guarantees

By using both, the oracle achieves:
- Operator convenience (time-based threshold for when to transition phases)
- Cryptographic soundness (ledger-based protection against MEV attacks)

## Implementation Details

### reveal_reading Ledger Check

The `reveal_reading` function enforces the minimum ledger boundary:

```rust
let current_ledger = e.ledger().sequence();
if current_ledger < window.reveal_opened_ledger + config.min_reveal_ledgers {
    panic!("reveal submitted before the reveal window opened");
}
```

This ensures reveals cannot land in the same ledger or within the mandatory gap.

### begin_reveal_phase Recording

When `begin_reveal_phase` is called, it records the current ledger sequence:

```rust
window.reveal_opened_ledger = e.ledger().sequence();
```

This anchor point is used for all subsequent reveal checks.

## Validation Rules

The oracle enforces these invariants:

1. **min_reveal_ledgers ≥ 1**: MEV protection is mandatory; zero is rejected
2. **min_reveal_ledgers ≤ max_reveal_ledgers**: The minimum gap cannot exceed the reveal window
3. **max_reveal_ledgers > 0**: The reveal window must have positive width

These constraints are enforced in both `initialize()` and `update_config()`.

## Migration Notes for Operators

If upgrading from an older deployment with `min_reveal_ledgers = 0`:

1. The new default is `min_reveal_ledgers = 5`
2. Existing deployments must update via `update_config()` to adopt MEV protection
3. No breaking changes to finalization logic; only the timing constraints are tightened
4. Historical windows are unaffected; the change applies only to new windows opened after the update

## References

- **Issue #172**: Default min_reveal_ledgers = 0 provides no MEV protection
- **Commit-Reveal Schemes**: https://en.wikipedia.org/wiki/Commitment_scheme
- **MEV Protection**: https://ethereum.org/en/developers/docs/mev/
- **Stellar Ledger Timing**: https://developers.stellar.org/docs/glossary/ledger
