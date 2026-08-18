//! Property-based / fuzz testing for `compute_finalization` and `median_i64`.
//!
//! This file implements the requirements from the issue:
//!   - At least 20 randomized test cases (deterministic seeded RNG)
//!   - Asserts 5 invariants:
//!     1. total >= 0
//!     2. total <= gross (when gross >=0)
//!     3. No panic for valid ranges
//!     4. If median == baseline, removal ==0
//!     5. penalty in [0,8000]
//!   - At least 10 boundary-value tests
//!   - Covers median_i64 with arbitrary-length arrays
//!   - Deterministic & CI-friendly
//!
//! ## Bug found & fixed during fuzzing
//! `median_i64` previously did `(a + b) /2` in i64, which overflows for
//! `[i64::MAX, i64::MAX]` and `[i64::MIN, i64::MIN]`.
//! Fix: promote to i128 before addition. See `verification_oracle/src/lib.rs`.

use soroban_sdk::{testutils::Address as _, Address, Env, Vec};
use verification_oracle::{
    compute_finalization, median_i64, OracleConfig, MAX_WINDOW_SECS, MIN_WINDOW_SECS,
};

// ---------------------------------------------------------------------------
// Deterministic RNG (LCG + SplitMix for better distribution, no external crate)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    // xorshift64* style
    fn next_u64(&mut self) -> u64 {
        // LCG with good constants, deterministic across platforms
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }

    fn next_i64(&mut self) -> i64 {
        self.next_u64() as i64
    }

    /// Generate i64 in [min, max] inclusive, deterministic.
    fn gen_range_i64(&mut self, min: i64, max: i64) -> i64 {
        assert!(min <= max);
        let range = (max as i128 - min as i128 + 1) as u128;
        let val = (self.next_u64() as u128) % range;
        (min as i128 + val as i128) as i64
    }

    fn gen_range_u32(&mut self, min: u32, max: u32) -> u32 {
        assert!(min <= max);
        let range = (max - min + 1) as u64;
        ((self.next_u64() % range) as u32) + min
    }

    fn gen_bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_config(e: &Env) -> OracleConfig {
    OracleConfig {
        min_oracles: 3,
        max_oracles: 10,
        quality_threshold_ph: 600,
        quality_threshold_ph_max: 700,
        quality_threshold_turbidity: 50,
        quality_threshold_do: 50,
        quality_threshold_temp: 300,
        credit_per_kg_n: 10,
        credit_per_kg_p: 20,
        staking_token: Address::generate(e),
        treasury: Address::generate(e),
        min_stake: 0,
        unstake_cooldown_secs: 86400,
        commit_phase_secs: 300,
        min_reveal_ledgers: 0,
        max_reveal_ledgers: 60,
        slash_pct_bps: 1000,
        min_slash_amount: 0,
        max_slash_amount: i128::MAX,
        window_secs: 3600,
    }
}

fn default_config_with_rng(e: &Env, rng: &mut DeterministicRng) -> OracleConfig {
    let mut cfg = default_config(e);
    // Generate valid but random thresholds
    cfg.quality_threshold_ph = rng.gen_range_i64(0, 1000);
    cfg.quality_threshold_ph_max = rng.gen_range_i64(cfg.quality_threshold_ph, 1400);
    cfg.quality_threshold_turbidity = rng.gen_range_i64(0, 1000);
    cfg.quality_threshold_do = rng.gen_range_i64(0, 1000);
    cfg.quality_threshold_temp = rng.gen_range_i64(-500, 1000);
    cfg.credit_per_kg_n = rng.gen_range_i64(0, 1000) as i128;
    cfg.credit_per_kg_p = rng.gen_range_i64(0, 1000) as i128;
    cfg
}

#[derive(Debug, Clone)]
struct FuzzCase {
    med_ph: i64,
    med_turb: i64,
    med_do: i64,
    med_temp: i64,
    med_flow: i64,
    med_n: i64,
    med_p: i64,
    baseline_n: i128,
    baseline_p: i128,
    temp_threshold: i128,
    /// Monitoring window length in seconds (Issue #93). Drawn from the range
    /// `update_config` accepts, so the fuzzer exercises the full multiplier
    /// span the contract can actually be configured with — not just 3600.
    window_secs: u64,
    config: OracleConfig,
}

fn generate_valid_case(e: &Env, rng: &mut DeterministicRng) -> FuzzCase {
    // Valid ranges per MATH.md entry validation + sensible upper bounds to avoid overflow
    let med_ph = rng.gen_range_i64(0, 1400);
    let med_turb = rng.gen_range_i64(0, 10000);
    let med_do = rng.gen_range_i64(0, 5000);
    let med_temp = rng.gen_range_i64(-500, 1000);
    // Capped so that the widest configurable window (MAX_WINDOW_SECS) still
    // cannot overflow the `(baseline - med) * flow * window_secs` product:
    // 100 * 1e6 * 86_400 ≈ 8.6e12, far below i128::MAX.
    let med_flow = rng.gen_range_i64(0, 1_000_000);
    let med_n = rng.gen_range_i64(0, 1000);
    let med_p = rng.gen_range_i64(0, 1000);
    let baseline_n = rng.gen_range_i64(0, 100) as i128;
    let baseline_p = rng.gen_range_i64(0, 100) as i128;
    let temp_threshold = rng.gen_range_i64(-500, 1000) as i128;
    let window_secs = rng.gen_range_i64(MIN_WINDOW_SECS as i64, MAX_WINDOW_SECS as i64) as u64;
    let mut config = default_config_with_rng(e, rng);
    config.window_secs = window_secs;

    FuzzCase {
        med_ph,
        med_turb,
        med_do,
        med_temp,
        med_flow,
        med_n,
        med_p,
        baseline_n,
        baseline_p,
        temp_threshold,
        window_secs,
        config,
    }
}

fn assert_invariants(case: &FuzzCase, result: &verification_oracle::FinalizationResult) {
    // 1. total >=0
    assert!(
        result.total >= 0,
        "Invariant failed: total >=0, got {} for case {:?}",
        result.total,
        case
    );

    // Compute gross ourselves for invariant 2
    let n_credit = result.n_removed * case.config.credit_per_kg_n;
    let p_credit = result.p_removed * case.config.credit_per_kg_p;
    let gross = n_credit + p_credit + result.volumetric_credit;

    // 2. total <= gross when gross >=0 (penalty never increases credits)
    if gross >= 0 {
        assert!(
            result.total <= gross,
            "Invariant failed: total <= gross, total={} gross={} case={:?}",
            result.total,
            gross,
            case
        );
    } else {
        // When gross negative due to misconfiguration, total floors at 0, so total >= gross
        // Expected behavior: total ==0 and gross negative.
        assert_eq!(
            result.total, 0,
            "When gross negative, total should floor at 0"
        );
    }

    // 5. penalty bounded [0,8000]
    assert!(
        (0..=8000).contains(&result.penalty),
        "Invariant failed: penalty in [0,8000], got {} case {:?}",
        result.penalty,
        case
    );

    // Additional: volumetric_credit >=0 when flow >=0
    assert!(
        result.volumetric_credit >= 0,
        "volumetric_credit should be >=0 for non-negative flow"
    );

    // n_removed, p_removed >=0
    assert!(result.n_removed >= 0);
    assert!(result.p_removed >= 0);

    // 4. If median == baseline, removal ==0 (tested separately but also check here if applicable)
    if case.med_n as i128 == case.baseline_n {
        assert_eq!(
            result.n_removed, 0,
            "n_removed should be 0 when med_n == baseline_n, case {:?}",
            case
        );
    }
    if case.med_p as i128 == case.baseline_p {
        assert_eq!(
            result.p_removed, 0,
            "p_removed should be 0 when med_p == baseline_p, case {:?}",
            case
        );
    }
}

// ---------------------------------------------------------------------------
// Randomized fuzz tests (deterministic)
// ---------------------------------------------------------------------------

#[test]
fn test_fuzz_random_valid_inputs_deterministic_100_cases() {
    // Fixed seed for determinism across CI runs
    let seed = 0xCAFEBABE_DEADBEEF_u64;
    let mut rng = DeterministicRng::new(seed);
    let e = Env::default();

    for i in 0..100 {
        let case = generate_valid_case(&e, &mut rng);
        // This should NOT panic for valid ranges
        let result = compute_finalization(
            &case.config,
            case.med_ph,
            case.med_turb,
            case.med_do,
            case.med_temp,
            case.med_flow,
            case.med_n,
            case.med_p,
            case.baseline_n,
            case.baseline_p,
            case.temp_threshold,
            case.window_secs,
        );
        assert_invariants(&case, &result);

        // Additional check: if med_flow ==0 then all removal and volumetric should be 0
        if case.med_flow == 0 {
            assert_eq!(result.volumetric_credit, 0, "iteration {}", i);
            assert_eq!(result.n_removed, 0, "iteration {}", i);
            assert_eq!(result.p_removed, 0, "iteration {}", i);
            assert_eq!(result.total, 0, "iteration {}", i);
        }
    }
}

#[test]
fn test_fuzz_random_valid_inputs_second_seed_50_cases() {
    let seed = 0x12345678_9ABCDEF0_u64;
    let mut rng = DeterministicRng::new(seed);
    let e = Env::default();

    for _ in 0..50 {
        let case = generate_valid_case(&e, &mut rng);
        let result = compute_finalization(
            &case.config,
            case.med_ph,
            case.med_turb,
            case.med_do,
            case.med_temp,
            case.med_flow,
            case.med_n,
            case.med_p,
            case.baseline_n,
            case.baseline_p,
            case.temp_threshold,
            case.window_secs,
        );
        assert_invariants(&case, &result);
    }
}

#[test]
fn test_fuzz_all_valid_fields_zero() {
    // Edge: all zeros (valid per entry validation)
    let e = Env::default();
    let config = default_config(&e);
    let result = compute_finalization(&config, 0, 0, 0, 0, 0, 0, 0, 10, 2, 300, 3600);
    assert_eq!(result.total, 0);
    // ph 0 <600 =>2000, turb 0 <=50 =>0, DO 0 <50 =>2000, temp 0 <=300 =>0 => 4000
    assert_eq!(result.penalty, 4000);
    assert!(result.penalty >= 0 && result.penalty <= 8000);
}

#[test]
fn test_fuzz_median_i64_random_arrays_100_cases() {
    let seed = 0xDEADBEEF_C0FFEE_u64;
    let mut rng = DeterministicRng::new(seed);
    let e = Env::default();

    for i in 0..100 {
        let len = rng.gen_range_u32(1, 10) as usize; // valid lengths 1..10
        let mut values: Vec<i64> = Vec::new(&e);
        let mut rust_vals: std::vec::Vec<i64> = std::vec::Vec::new();
        for _ in 0..len {
            // generate values including extremes occasionally
            let val = if i % 10 == 0 {
                // every 10th iteration, force extreme
                if rng.gen_bool() {
                    i64::MAX
                } else {
                    i64::MIN
                }
            } else {
                rng.gen_range_i64(-10000, 10000)
            };
            values.push_back(val);
            rust_vals.push(val);
        }

        // Should not panic
        let med = median_i64(&values);

        // Compute expected median using std sort + i128 averaging to avoid overflow
        rust_vals.sort();
        let expected = if len % 2 == 0 {
            let a = rust_vals[len / 2 - 1] as i128;
            let b = rust_vals[len / 2] as i128;
            ((a + b) / 2) as i64
        } else {
            rust_vals[len / 2]
        };

        assert_eq!(
            med, expected,
            "median mismatch iteration {} len {} values {:?}",
            i, len, rust_vals
        );

        // Invariant: median is within [min, max]
        let min = *rust_vals.iter().min().unwrap();
        let max = *rust_vals.iter().max().unwrap();
        assert!(
            med >= min && med <= max,
            "median {} not in [{}, {}] for {:?}",
            med,
            min,
            max,
            rust_vals
        );
    }
}

#[test]
fn test_fuzz_median_i64_extreme_values_no_overflow() {
    // This is the bug previously present: [MAX, MAX] would overflow i64 addition.
    // After fix, it should return MAX without panic.
    let e = Env::default();

    // Case 1: both MAX
    let mut v = Vec::new(&e);
    v.push_back(i64::MAX);
    v.push_back(i64::MAX);
    let med = median_i64(&v);
    assert_eq!(med, i64::MAX);

    // Case 2: both MIN
    let mut v2 = Vec::new(&e);
    v2.push_back(i64::MIN);
    v2.push_back(i64::MIN);
    let med2 = median_i64(&v2);
    assert_eq!(med2, i64::MIN);

    // Case 3: MAX and MIN -> average 0? (MAX + MIN)/2 = (MAX-1)/2? Let's compute: MAX=9223372036854775807, MIN=-9223372036854775808 => sum=-1, /2 =0 (trunc toward zero)
    let mut v3 = Vec::new(&e);
    v3.push_back(i64::MAX);
    v3.push_back(i64::MIN);
    let med3 = median_i64(&v3);
    // (MAX as i128 + MIN as i128) = -1, /2 =0
    assert_eq!(med3, 0);

    // Case 4: 10 elements all MAX
    let mut v4 = Vec::new(&e);
    for _ in 0..10 {
        v4.push_back(i64::MAX);
    }
    let med4 = median_i64(&v4);
    assert_eq!(med4, i64::MAX);

    // Case 5: mixed extreme odd
    let mut v5 = Vec::new(&e);
    v5.push_back(i64::MIN);
    v5.push_back(0);
    v5.push_back(i64::MAX);
    let med5 = median_i64(&v5);
    assert_eq!(med5, 0);
}

#[test]
fn test_fuzz_median_i64_single_element() {
    let e = Env::default();
    let mut rng = DeterministicRng::new(0xABCD);
    for _ in 0..20 {
        let val = rng.next_i64();
        let mut v = Vec::new(&e);
        v.push_back(val);
        assert_eq!(median_i64(&v), val);
    }
}

// ---------------------------------------------------------------------------
// Boundary-value tests (at least 10)
// ---------------------------------------------------------------------------

#[test]
fn test_boundary_flow_rate_zero() {
    let e = Env::default();
    let config = default_config(&e);
    let result = compute_finalization(&config, 700, 10, 80, 250, 0, 8, 1, 10, 2, 300, 3600);
    assert_eq!(result.volumetric_credit, 0);
    assert_eq!(result.n_removed, 0);
    assert_eq!(result.p_removed, 0);
    assert_eq!(result.total, 0);
    assert!(result.penalty >= 0 && result.penalty <= 8000);
}

#[test]
fn test_boundary_baseline_n_equals_med_n() {
    let e = Env::default();
    let config = default_config(&e);
    // med_n == baseline_n => n_removed should be 0
    let result = compute_finalization(&config, 700, 10, 80, 250, 500, 10, 1, 10, 2, 300, 3600);
    assert_eq!(result.n_removed, 0);
    assert!(result.p_removed > 0); // p still below baseline
    assert!(result.total >= 0);
}

#[test]
fn test_boundary_baseline_p_equals_med_p() {
    let e = Env::default();
    let config = default_config(&e);
    // med_p == baseline_p => p_removed 0
    let result = compute_finalization(&config, 700, 10, 80, 250, 500, 8, 2, 10, 2, 300, 3600);
    assert_eq!(result.p_removed, 0);
    assert!(result.n_removed > 0);
}

#[test]
fn test_boundary_both_baselines_equal_medians() {
    let e = Env::default();
    let config = default_config(&e);
    let result = compute_finalization(&config, 700, 10, 80, 250, 500, 10, 2, 10, 2, 300, 3600);
    assert_eq!(result.n_removed, 0);
    assert_eq!(result.p_removed, 0);
    // Only volumetric credit remains
    assert_eq!(result.volumetric_credit, 50);
    // No penalty
    assert_eq!(result.penalty, 0);
    assert_eq!(result.total, 50);
}

#[test]
fn test_boundary_all_penalties_maximum() {
    let e = Env::default();
    let config = default_config(&e);
    // All 4 conditions breached: ph out, turb high, do low, temp high => 2000+2000+2000+1000=7000
    let result = compute_finalization(&config, 300, 200, 10, 350, 500, 8, 1, 10, 2, 300, 3600);
    assert_eq!(result.penalty, 7000);
    assert!(result.penalty <= 8000);
    assert!(result.total >= 0);
    assert!(result.total <= 100); // gross is 100, penalty reduces to 30
    assert_eq!(result.total, 30);
}

#[test]
fn test_boundary_penalty_capped_at_8000() {
    // Even if we artificially create config that would exceed 8000 if weights changed,
    // the cap ensures <=8000. Currently max is 7000, but we test capping logic.
    let e = Env::default();
    let config = default_config(&e);
    // All penalties + verify cap
    let result = compute_finalization(&config, 0, 10000, 0, 10000, 1000, 0, 0, 10, 2, 0, 3600);
    assert!(result.penalty <= 8000);
    assert!(result.penalty >= 0);
    // With our weights, should be 7000 even for extreme
    assert_eq!(result.penalty, 7000);
}

#[test]
fn test_boundary_medians_at_i64_max_safe_range() {
    // Flow at 1_000_000 is safe, but ph at 1400 boundary, nitrogen at 0
    let e = Env::default();
    let config = default_config(&e);
    let result = compute_finalization(
        &config, 1400, 0, 1000, 0, 1_000_000, 0, 0, 10, 2, 1000, 3600,
    );
    assert!(result.total >= 0);
    assert!(result.penalty <= 8000);
    // ph 1400 >700 => penalty 2000, but other conditions ok
    assert_eq!(result.penalty, 2000);
}

#[test]
fn test_boundary_median_at_i64_min_and_max_for_median_fn() {
    // Test median function itself at boundaries (not compute_finalization, which would overflow with MAX flow)
    let e = Env::default();
    // median of [MIN, MAX] should not panic and be 0 (fixed bug)
    let mut v = Vec::new(&e);
    v.push_back(i64::MIN);
    v.push_back(i64::MAX);
    let med = median_i64(&v);
    assert_eq!(med, 0);
}

#[test]
fn test_boundary_ph_at_threshold_boundaries() {
    let e = Env::default();
    let config = default_config(&e);
    // ph exactly at lower bound 600 -> no penalty
    let r1 = compute_finalization(&config, 600, 10, 80, 250, 500, 8, 1, 10, 2, 300, 3600);
    assert_eq!(r1.penalty, 0);
    // ph exactly at upper bound 700 -> no penalty
    let r2 = compute_finalization(&config, 700, 10, 80, 250, 500, 8, 1, 10, 2, 300, 3600);
    assert_eq!(r2.penalty, 0);
    // ph just outside: 599 and 701
    let r3 = compute_finalization(&config, 599, 10, 80, 250, 500, 8, 1, 10, 2, 300, 3600);
    assert_eq!(r3.penalty, 2000);
    let r4 = compute_finalization(&config, 701, 10, 80, 250, 500, 8, 1, 10, 2, 300, 3600);
    assert_eq!(r4.penalty, 2000);
}

#[test]
fn test_boundary_credit_per_kg_zero() {
    let e = Env::default();
    let mut config = default_config(&e);
    config.credit_per_kg_n = 0;
    config.credit_per_kg_p = 0;
    let result = compute_finalization(&config, 700, 10, 80, 250, 500, 8, 1, 10, 2, 300, 3600);
    // Only volumetric credit remains
    assert_eq!(result.total, result.volumetric_credit);
    assert_eq!(result.total, 50);
}

#[test]
fn test_boundary_baseline_zero() {
    let e = Env::default();
    let config = default_config(&e);
    // baseline 0, med_n 0 => equal => 0 removal
    let r1 = compute_finalization(&config, 700, 10, 80, 250, 500, 0, 0, 0, 0, 300, 3600);
    assert_eq!(r1.n_removed, 0);
    assert_eq!(r1.p_removed, 0);
    // baseline 0, med_n 0 for N but P below baseline? Actually P median 0 < baseline 2? Wait baseline 0, so not.
    // Let's test baseline 10, med_n 0 => max removal
    let r2 = compute_finalization(&config, 700, 10, 80, 250, 500, 0, 0, 10, 2, 300, 3600);
    assert_eq!(r2.n_removed, 10 * 500 * 3600 / 1_000_000);
    assert_eq!(r2.p_removed, 2 * 500 * 3600 / 1_000_000);
}

#[test]
fn test_boundary_temperature_at_threshold() {
    let e = Env::default();
    let config = default_config(&e);
    // temp exactly at 300 => no penalty
    let r1 = compute_finalization(&config, 700, 10, 80, 300, 500, 8, 1, 10, 2, 300, 3600);
    assert_eq!(r1.penalty, 0);
    // temp 301 => penalty +1000
    let r2 = compute_finalization(&config, 700, 10, 80, 301, 500, 8, 1, 10, 2, 300, 3600);
    assert_eq!(r2.penalty, 1000);
}

#[test]
fn test_boundary_turbidity_and_do_at_thresholds() {
    let e = Env::default();
    let config = default_config(&e);
    // turb exactly 50 => no penalty, 51 => penalty
    let r1 = compute_finalization(&config, 700, 50, 80, 250, 500, 8, 1, 10, 2, 300, 3600);
    assert_eq!(r1.penalty, 0);
    let r2 = compute_finalization(&config, 700, 51, 80, 250, 500, 8, 1, 10, 2, 300, 3600);
    assert_eq!(r2.penalty, 2000);

    // DO exactly 50 => no penalty, 49 => penalty
    let r3 = compute_finalization(&config, 700, 10, 50, 250, 500, 8, 1, 10, 2, 300, 3600);
    assert_eq!(r3.penalty, 0);
    let r4 = compute_finalization(&config, 700, 10, 49, 250, 500, 8, 1, 10, 2, 300, 3600);
    assert_eq!(r4.penalty, 2000);
}

#[test]
fn test_boundary_flow_rate_max_no_overflow() {
    // Use flow = 1_000_000 (our valid max) which should NOT overflow
    let e = Env::default();
    let config = default_config(&e);
    let result = compute_finalization(&config, 700, 10, 80, 250, 1_000_000, 8, 1, 10, 2, 300, 3600);
    assert!(result.total >= 0);
    // n_removed = (10-8)*1e6*3600/1e6 = 2*3600=7200 kg
    assert_eq!(result.n_removed, 7200);
    assert_eq!(result.p_removed, 3600);
    // volumetric = 1e6/10=100_000
    assert_eq!(result.volumetric_credit, 100_000);
}

#[test]
#[should_panic(expected = "n removal: time-window multiplication overflow")]
fn test_boundary_overflow_expected_panic_extreme_flow_and_baseline() {
    // This tests that extreme values DO panic as expected (checked_mul)
    // This is an expected panic, not a bug.
    let e = Env::default();
    let config = default_config(&e);
    compute_finalization(
        &config,
        700,
        10,
        80,
        250,
        i64::MAX,
        0,
        1,
        i64::MAX as i128,
        2,
        300,
        MAX_WINDOW_SECS,
    );
}

#[test]
fn test_boundary_median_empty_panics_expected() {
    // Empty vec should panic (expected)
    let e = Env::default();
    let v: Vec<i64> = Vec::new(&e);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        median_i64(&v);
    }));
    assert!(result.is_err(), "median of empty vec should panic");
}

// ---------------------------------------------------------------------------
// Additional fuzz: credit_per_kg randomization but keep gross >=0
// ---------------------------------------------------------------------------

#[test]
fn test_fuzz_penalty_never_increases_credits_for_nonnegative_gross() {
    let seed = 0xBADDCAFE_F00DBABE_u64;
    let mut rng = DeterministicRng::new(seed);
    let e = Env::default();

    for _ in 0..100 {
        let case = generate_valid_case(&e, &mut rng);
        // Force credit_per_kg to be non-negative to ensure gross >=0
        let result = compute_finalization(
            &case.config,
            case.med_ph,
            case.med_turb,
            case.med_do,
            case.med_temp,
            case.med_flow,
            case.med_n,
            case.med_p,
            case.baseline_n,
            case.baseline_p,
            case.temp_threshold,
            case.window_secs,
        );

        let gross = result.n_removed * case.config.credit_per_kg_n
            + result.p_removed * case.config.credit_per_kg_p
            + result.volumetric_credit;

        assert!(
            gross >= 0,
            "gross should be >=0 for non-negative credit rates"
        );
        assert!(
            result.total <= gross,
            "penalty should never increase credits: total {} <= gross {} for {:?}",
            result.total,
            gross,
            case
        );
    }
}

#[test]
fn test_fuzz_baseline_equality_removal_zero_50_cases() {
    let mut rng = DeterministicRng::new(0xFEEDFACE_C0DE1234_u64);
    let e = Env::default();

    for _ in 0..50 {
        let mut case = generate_valid_case(&e, &mut rng);
        // Force med_n == baseline_n
        let baseline = rng.gen_range_i64(0, 100);
        case.med_n = baseline;
        case.baseline_n = baseline as i128;

        let result = compute_finalization(
            &case.config,
            case.med_ph,
            case.med_turb,
            case.med_do,
            case.med_temp,
            case.med_flow,
            case.med_n,
            case.med_p,
            case.baseline_n,
            case.baseline_p,
            case.temp_threshold,
            case.window_secs,
        );
        assert_eq!(result.n_removed, 0);

        // Also test p
        case.med_p = baseline;
        case.baseline_p = baseline as i128;
        let result2 = compute_finalization(
            &case.config,
            case.med_ph,
            case.med_turb,
            case.med_do,
            case.med_temp,
            case.med_flow,
            case.med_n,
            case.med_p,
            case.baseline_n,
            case.baseline_p,
            case.temp_threshold,
            case.window_secs,
        );
        assert_eq!(result2.p_removed, 0);
    }
}

// ---------------------------------------------------------------------------
// Ensure fuzz tests are deterministic (same seed => same sequence)
// ---------------------------------------------------------------------------

#[test]
fn test_fuzz_deterministic_seed_same_sequence() {
    let seed = 0xABCDEF12_34567890_u64;
    let mut rng1 = DeterministicRng::new(seed);
    let mut rng2 = DeterministicRng::new(seed);

    for _ in 0..20 {
        assert_eq!(rng1.next_u64(), rng2.next_u64());
    }

    // Also test that different seeds produce different sequences
    let mut rng3 = DeterministicRng::new(seed + 1);
    let mut rng1_again = DeterministicRng::new(seed);
    assert_ne!(rng1_again.next_u64(), rng3.next_u64());
}
