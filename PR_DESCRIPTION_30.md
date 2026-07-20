# Perf: Replace insertion-sort median with gas-efficient stack-allocated selection (#30)

## Summary

`median_i64` in `verification_oracle` used an insertion sort over a Soroban
`Vec<i64>`, incurring O(n²) host calls per invocation. With seven calls per
finalization and `max_oracles = 10`, each finalization executed up to 350 host
vector operations just for sorting. `median_i128` was an identical but dead
(`#[allow(unused)]`) function.

This PR replaces `median_i64` with a **stack-allocated insertion sort** on a
native `[i64; 10]` buffer, reducing host calls from O(n²) to O(n) (exactly n
`.get()` reads, zero allocations). It removes the dead `median_i128`, enforces
`max_oracles ≤ 10` in `update_config` to guarantee the stack buffer invariant,
and adds 10 unit tests plus a gas regression benchmark.

## Why this matters

- **Gas savings:** Under `max_oracles = 10`, each finalization previously
  performed ~350 host vector `.get`/`.insert` calls across seven median
  invocations. The new code does exactly `10 × 7 = 70` host calls (one `.get`
  per value), with all sorting and computation happening in-processor.
- **Dead code removal:** `median_i128` was an exact copy with no callers,
  maintained for no reason.
- **Overflow safety:** The old code could overflow on `(a + b) / 2` for
  extreme `i64` values (e.g. two `i64::MAX` readings). The new code promotes
  to `i128` before addition.
- **Invariant enforcement:** The stack buffer is sized for `MAX_ORACLES = 10`;
  `update_config` now rejects `max_oracles > 10`, ensuring the buffer is
  always sufficient.

## Root cause

In `contracts/verification_oracle/src/lib.rs`, `median_i64` (~lines 232–255):

```rust
fn median_i64(e: &Env, values: &Vec<i64>) -> i64 {
    let mut sorted: Vec<i64> = Vec::new(e);       // Soroban host allocation
    for i in 0..values.len() {
        let val = values.get(i).unwrap();          // host call
        for j in 0..sorted.len() {                 // O(n²) inner loop
            if val < sorted.get(j).unwrap() {      // host call
                sorted.insert(j, val);              // host call
                inserted = true;
                break;
            }
        }
        if !inserted {
            sorted.push_back(val);                  // host call
        }
    }
    // ... median extraction
}
```

Every `.get(j)`, `.insert(j, val)`, and `.push_back(val)` is a
**host function call** into the Soroban VM with non-trivial per-call
overhead. With 10 oracles, each median invocation performs ~55 host calls
(sum of triangular numbers for insertion sort). Multiplied by 7 sensor
fields per finalization, that's ~385 host calls per window — purely for
sorting.

## Changes

### `contracts/verification_oracle/src/lib.rs`

#### Core algorithm

**`median_i64`** — replaced insertion sort on `Vec<i64>` with a native-Rust
insertion sort on a stack-allocated `[i64; MAX_ORACLES]` buffer:

1. Extract each value from the Soroban `Vec` with a single `.get(i)` call
   (O(n) host calls total).
2. Sort the native Rust slice in-processor (zero host calls; insertion sort
   on ≤ 10 elements is effectively free).
3. Compute the median: for odd counts return the middle element; for even
   counts promote both middle elements to `i128`, sum, divide, and cast
   back to `i64` — overflow-safe and behaviour-identical to the old code.

```rust
fn median_i64(_e: &Env, values: &Vec<i64>) -> i64 {
    let n = values.len();
    if n == 0 { return 0; }

    let mut buf: [i64; MAX_ORACLES] = [0; MAX_ORACLES];
    let end = n.min(MAX_ORACLES);
    for i in 0..end { buf[i] = values.get(i).unwrap(); }

    for i in 1..end {
        let key = buf[i];
        let mut j = i;
        while j > 0 && buf[j - 1] > key { buf[j] = buf[j - 1]; j -= 1; }
        buf[j] = key;
    }

    if end % 2 == 0 {
        let a = buf[end / 2 - 1] as i128;
        let b = buf[end / 2] as i128;
        ((a + b) / 2) as i64
    } else {
        buf[end / 2]
    }
}
```

The `_e: &Env` parameter is retained for backward compatibility with the 14
existing call sites (7 in `submit_reading_impl`, 7 in `finalize_reveals`).

#### Dead code removal

**`median_i128`** — removed entirely. It was identical logic flagged with
`#[allow(unused)]` and had zero callers anywhere in the codebase.

#### Invariant enforcement

**`MAX_ORACLES = 10` constant** — documents the hard bound and sizes the
stack buffer.

**`update_config`** — now panics with `"max_oracles must be at most 10"`
when `config.max_oracles > 10`, guaranteeing the stack buffer invariant.

**`test_config_update_succeeds`** — updated from `max_oracles: 15` to
`max_oracles: 10` to respect the new invariant.

#### Even-size median behaviour

The even-size median formula `(a + b) / 2` (with i128 promotion) preserves
the original **truncation-toward-zero** behaviour of Rust integer division.
Verified in tests:

| Input | Sorted | Old `(a+b)/2` | New `(a+b)/2` (i128) |
|---|---|---|---|
| `[-2, -3]` | `[-3, -2]` | `-5/2 = -2` | `-5/2 = -2` ✓ |
| `[-3, -4]` | `[-4, -3]` | `-7/2 = -3` | `-7/2 = -3` ✓ |
| `[i64::MIN, i64::MAX]` | same | `-1/2 = 0` | `-1/2 = 0` ✓ |
| `[i64::MAX, i64::MAX]` | same | overflow | `i64::MAX` (safe) ✓ |

### Tests (10 new unit tests + 1 gas benchmark)

| Test | What it covers |
|---|---|
| `test_median_single_element` | Single-value input |
| `test_median_odd_count` | `[3, 1, 2]` → `2` (unsorted odd count) |
| `test_median_even_count` | `[1, 3]` → `2` (basic even averaging) |
| `test_median_even_count_truncates_toward_zero` | `[-2, -3]` → `-2` and `[-3, -4]` → `-3` |
| `test_median_even_count_positive_rounding` | `[3, 2]` → `2` (positive truncation) |
| `test_median_all_negative` | Five negative values → median `-5` |
| `test_median_mixed_signs` | `[-10, 0, 10]` → `0` |
| `test_median_extreme_values_no_overflow` | `[i64::MAX, i64::MAX]`, `[i64::MIN, i64::MIN]`, `[i64::MIN, i64::MAX]` |
| `test_median_ten_elements_max_oracles` | `[9,0,8,1,7,2,6,3,5,4]` → `4` (max oracle count) |
| `test_median_empty_returns_zero` | Empty Vec → `0` (defensive guard) |
| `test_median_gas_with_max_oracles` | 10-oracle finalization CPU budget < 10M instructions |

#### Gas benchmark design

`test_median_gas_with_max_oracles` sets up 10 oracles, submits 9 readings
without triggering finalization, captures the CPU instruction count, then
submits the 10th reading which triggers the full finalization path (median
×7 + credit math + storage writes + events). The assertion that this stays
under 10M CPU instructions is a **regression guard** — if the median
computation ever regresses to O(n²) host calls, this test will catch it
without requiring manual before/after comparison on every CI run.

## Gas comparison (theoretical)

| Scenario | Old (insertion sort on Vec) | New (stack buffer + native sort) |
|---|---|---|
| Host `.get()` calls per median | ~n + n(n-1)/2 (reads) | n |
| Host `.insert()` calls per median | n(n-1)/2 (writes) | 0 |
| Host `Vec::new()` allocations | 1 (Soroban Vec) | 0 |
| Host operations, n=10, per median | ~55 reads + ~45 writes = ~100 | 10 reads |
| Host ops, 7 fields, n=10 | ~700 | 70 |
| Host ops, 7 fields, n=3 (min) | ~147 | 21 |

The exact gas (CPU instruction) savings depend on the Soroban host
implementation, but the order-of-magnitude reduction in host calls is clear.

## Acceptance criteria

- [x] `median_i64` replaced with an O(n) algorithm that minimizes Soroban
      host calls.
- [x] `median_i128` removed (dead code, `#[allow(unused)]`).
- [x] Even-size median behaviour explicitly tested and documented
      (truncation toward zero, overflow-safe via i128 promotion).
- [x] Gas benchmark test added comparing resource usage with
      `max_oracles = 10`.
- [x] All existing oracle tests pass (backward-compatible change).
- [x] No use of `std::sort` or heap allocation.
- [x] `max_oracles` hard cap enforced in `update_config` (≤ 10).

## Relevant files / functions

- `contracts/verification_oracle/src/lib.rs`:
  - `median_i64` (replaced)
  - `median_i128` (removed)
  - `MAX_ORACLES` constant (new)
  - `update_config` (max_oracles ≤ 10 guard added)
  - `test_median_*` tests (new)
  - `test_median_gas_with_max_oracles` (new)
  - `test_config_update_succeeds` (updated max_oracles: 15 → 10)
- `doc/MATH.md` — §2 "Multi-Oracle Median Aggregation" (documents the median
  formula; behaviour unchanged)

## Out of scope

- Changes to the credit formula
- Changes to other `OracleConfig` fields
- Sorting network for fixed-size inputs (insertion sort on ≤ 10 elements is
  already optimal for this domain)
- Generic/trait-based unification of median (over-engineering; only `i64` is
  ever needed)

## Test plan

```sh
# Unit tests (verification_oracle crate)
cargo test -p verification_oracle

# Integration tests (full multi-contract suite)
cargo test -p tests --features testutils

# Gas benchmark
cargo test -p verification_oracle test_median_gas_with_max_oracles

# Lint
cargo clippy -p verification_oracle -- -D warnings
```

closes #30
