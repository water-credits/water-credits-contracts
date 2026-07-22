//! Boundary and overflow tests for the `submit_reading` / `reveal_reading` credit
//! formula (verification_oracle finalization arithmetic).
//!
//! Prior to this suite, the formula in `verification_oracle/src/lib.rs` was only
//! exercised with hardcoded mid-range sensor values (ph=700, flow=500, n=8, p=1).
//! These tests cover the boundary conditions called out for audit-readiness:
//!   - entry validation rejects structurally-valid-but-physically-impossible readings
//!     (negative nitrogen/phosphorus/flow/dissolved-oxygen, out-of-range pH)
//!   - `i128` intermediates that would silently wrap now panic via `checked_mul`
//!     instead of minting corrupted credit amounts
//!   - a legitimate extreme value (e.g. `flow_rate` near `i64::MAX`) is still
//!     handled correctly when it doesn't actually overflow
//!   - `total_credits` is floored at 0 rather than going negative
//!   - the pH penalty band upper bound is independently configurable, not
//!     hardcoded to `quality_threshold_ph + 100`
//!
//! See doc/MATH.md for the formula this suite exercises.
//!
//! ## Why the panic-path tests call `validate_sensor_reading` / `compute_finalization`
//! directly instead of going through `submit_reading`
//!
//! Soroban's native test-contract dispatch (`Env::register_contract(None, T)` +
//! client calls, used by every test in this workspace) cannot catch a contract
//! panic in this SDK/toolchain combination — the panic aborts the whole test
//! process (`thread caused non-unwinding panic. aborting.`) instead of
//! unwinding into a `Result` or a `#[should_panic]`-catchable panic. This is a
//! pre-existing, environment-level limitation, already called out in
//! `credit_lifecycle.rs` (`test_unauthorized_oracle_rejected`: "This panic is
//! non-catchable in the test host, so we can only verify preconditions") and
//! reproducible even through the codebase's own `e.try_invoke_contract(...)`
//! workaround. It reproduces identically on the exact toolchain CI pins
//! (`dtolnay/rust-toolchain@1.85.0`) with the `Cargo.lock`-pinned
//! `soroban-sdk 20.5.0` / `soroban-env-host 20.3.0`.
//!
//! `validate_sensor_reading` and `compute_finalization` are therefore exported
//! as plain `pub fn` (not part of the `#[contractimpl]` on-chain surface — see
//! their doc comments in `verification_oracle::lib`) so the panic conditions
//! required by this issue's acceptance criteria can be unit-tested as ordinary
//! Rust function calls, with no contract dispatch involved and normal
//! `#[should_panic]` semantics.

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, Vec};
use verification_oracle::{
    compute_finalization, validate_sensor_reading, VerificationOracle, VerificationOracleClient,
};

// ══════════════════════════════════════════════════════════════════════════
// Direct unit tests: validate_sensor_reading / compute_finalization
// (no contract dispatch — see module doc comment above)
// ══════════════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "ph out of valid range")]
fn test_negative_ph_rejected() {
    validate_sensor_reading(-1, 500, 8, 1, 80);
}

#[test]
#[should_panic(expected = "ph out of valid range")]
fn test_ph_above_1400_rejected() {
    validate_sensor_reading(1401, 500, 8, 1, 80);
}

#[test]
#[should_panic(expected = "flow_rate must be non-negative")]
fn test_negative_flow_rate_rejected() {
    validate_sensor_reading(700, -1, 8, 1, 80);
}

#[test]
#[should_panic(expected = "total_nitrogen must be non-negative")]
fn test_negative_total_nitrogen_rejected() {
    validate_sensor_reading(700, 500, -1, 1, 80);
}

#[test]
#[should_panic(expected = "total_phosphorus must be non-negative")]
fn test_negative_total_phosphorus_rejected() {
    validate_sensor_reading(700, 500, 8, -1, 80);
}

#[test]
#[should_panic(expected = "dissolved_oxygen must be non-negative")]
fn test_negative_dissolved_oxygen_rejected() {
    validate_sensor_reading(700, 500, 8, 1, -1);
}

/// pH 0 and 1400 (the inclusive boundary values) must be accepted, not rejected.
#[test]
fn test_ph_boundary_values_accepted() {
    validate_sensor_reading(0, 500, 8, 1, 80);
    validate_sensor_reading(1400, 500, 8, 1, 80);
}

/// Zero is a valid (non-negative) boundary for every other field too.
#[test]
fn test_zero_boundary_values_accepted() {
    validate_sensor_reading(700, 0, 0, 0, 0);
}

fn default_config(e: &Env) -> verification_oracle::OracleConfig {
    verification_oracle::OracleConfig {
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
    }
}

/// Reproduces the exact overflow scenario from the issue: a baseline near
/// `i64::MAX` combined with `flow_rate = i64::MAX` overflows i128 in the
/// `* 3600` step. This must panic cleanly (transaction revert, via
/// `checked_mul`) rather than silently wrap and mint a corrupted credit
/// amount.
#[test]
#[should_panic(expected = "n removal: time-window multiplication overflow")]
fn test_extreme_baseline_and_max_flow_overflow_panics() {
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
    );
}

/// A large but physically-plausible baseline combined with `flow_rate` at the
/// very top of its valid range (structurally valid i64, ≥ 0) must NOT panic —
/// the checked arithmetic should only reject genuine overflow, not merely
/// large values.
#[test]
fn test_high_baseline_with_max_flow_does_not_overflow() {
    let e = Env::default();
    let config = default_config(&e);
    let fin = compute_finalization(&config, 700, 10, 80, 250, i64::MAX, 8, 1, 1000, 100, 300);
    assert_eq!(
        fin.n_removed,
        (1000i128 - 8) * i64::MAX as i128 * 3600 / 1_000_000
    );
}

/// An admin-configured `credit_per_kg_n` large enough to overflow the
/// `n_removed * credit_per_kg_n` multiplication must panic instead of
/// wrapping.
#[test]
#[should_panic(expected = "n credit multiplication overflow")]
fn test_credit_rate_multiplication_overflow_panics() {
    let e = Env::default();
    let mut config = default_config(&e);
    config.credit_per_kg_n = i128::MAX;
    // med_n=8 < baseline_n=10 -> n_removed=3 (nonzero), so 3 * i128::MAX overflows.
    compute_finalization(&config, 700, 10, 80, 250, 500, 8, 1, 10, 2, 300);
}

/// All sensor fields at 0 (valid per entry validation) must produce zero
/// credits without panicking.
#[test]
fn test_all_zero_sensor_values_no_panic_zero_credits() {
    let e = Env::default();
    let config = default_config(&e);
    let fin = compute_finalization(&config, 0, 0, 0, 0, 0, 0, 0, 10, 2, 300);
    assert_eq!(fin.total, 0);
    assert_eq!(fin.n_removed, 0);
    assert_eq!(fin.p_removed, 0);
    assert_eq!(fin.volumetric_credit, 0);
}

/// All four quality-penalty conditions triggered simultaneously (doc/MATH.md
/// Example B): penalty sums to 7000 bps, gross=100, total=30.
#[test]
fn test_all_penalty_conditions_triggered_simultaneously() {
    let e = Env::default();
    let config = default_config(&e);
    let fin = compute_finalization(&config, 300, 200, 10, 350, 500, 8, 1, 10, 2, 300);
    assert_eq!(fin.penalty, 7000);
    assert_eq!(fin.total, 30);
}

/// A misconfigured (admin-set) negative `credit_per_kg_n` can legitimately
/// drive `gross` negative; `total` must floor at 0, never go negative.
#[test]
fn test_negative_gross_from_misconfigured_credit_rate_floors_at_zero() {
    let e = Env::default();
    let mut config = default_config(&e);
    config.credit_per_kg_n = -1000;
    // n_removed=3, p_removed=1, volumetric=50, penalty=0 ->
    // gross = 3*(-1000) + 1*20 + 50 = -2930 -> would be negative pre-floor.
    let fin = compute_finalization(&config, 700, 10, 80, 250, 500, 8, 1, 10, 2, 300);
    assert_eq!(fin.total, 0);
}

/// The maximum penalty achievable under the fixed formula weights (2000 + 2000
/// + 2000 + 1000 = 7000 bps; the `penalty > 8000` cap exists as a ceiling for
///   future weight changes but is unreachable today — see doc/MATH.md §5.5)
///   applied to a large nonzero gross credit must still yield the exact scaled,
///   non-negative total: `gross * (10000 - 7000) / 10000`.
#[test]
fn test_max_penalty_with_nonzero_gross_credit() {
    let e = Env::default();
    let config = default_config(&e);
    let fin = compute_finalization(&config, 300, 200, 10, 350, 500, 8, 1, 1000, 100, 300);

    let n_removed = (1000i128 - 8) * 500 * 3600 / 1_000_000;
    let p_removed = (100i128 - 1) * 500 * 3600 / 1_000_000;
    let volumetric = 500i128 * 100 / 1000;
    let gross = n_removed * 10 + p_removed * 20 + volumetric;
    let expected_total = gross * 3000 / 10000;

    assert_eq!(fin.penalty, 7000);
    assert_eq!(fin.total, expected_total);
    assert!(fin.total >= 0);
}

/// The pH upper bound was previously hardcoded to `quality_threshold_ph + 100`.
/// It's now the independent `quality_threshold_ph_max` field: a pH reading
/// that would have been penalized under the old fixed offset is accepted once
/// the config widens the band.
#[test]
fn test_configurable_ph_upper_bound() {
    let e = Env::default();
    let mut config = default_config(&e);

    // Default band is [600, 700]; ph=750 breaches the (old, hardcoded) +100 max.
    let fin_default = compute_finalization(&config, 750, 10, 80, 250, 500, 8, 1, 10, 2, 300);
    assert_eq!(
        fin_default.penalty, 2000,
        "ph=750 should breach the default [600,700] band"
    );

    // Widen the band to [600, 900]; the same ph=750 reading is now in-range.
    config.quality_threshold_ph_max = 900;
    let fin_widened = compute_finalization(&config, 750, 10, 80, 250, 500, 8, 1, 10, 2, 300);
    assert_eq!(
        fin_widened.penalty, 0,
        "ph=750 should be in-range once the band is widened to 900"
    );
}

// ══════════════════════════════════════════════════════════════════════════
// Contract-level integration tests (through submit_reading), no-panic paths
// only — see module doc comment above for why panic paths aren't tested here.
// ══════════════════════════════════════════════════════════════════════════

struct Fixture {
    e: Env,
    admin: Address,
    client: VerificationOracleClient<'static>,
    oracles: Vec<Address>,
}

fn setup() -> Fixture {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let oracle_id = e.register_contract(None, VerificationOracle);
    let client = VerificationOracleClient::new(&e, &oracle_id);
    let staking_token = Address::generate(&e);
    let treasury = Address::generate(&e);
    client.initialize(&admin, &staking_token, &treasury);

    let mut config = client.get_config();
    config.min_stake = 0;
    client.update_config(&admin, &config);

    let mut oracles = Vec::new(&e);
    for _ in 0..3u32 {
        let o = Address::generate(&e);
        client.add_oracle(&admin, &o);
        oracles.push_back(o);
    }

    Fixture {
        e,
        admin,
        client,
        oracles,
    }
}

#[allow(clippy::too_many_arguments)]
fn submit_three(
    f: &Fixture,
    project_id: &BytesN<32>,
    ph: i64,
    turb: i64,
    do_: i64,
    flow: i64,
    temp: i64,
    n: i64,
    p: i64,
) {
    for i in 0..3u32 {
        f.client.submit_reading(
            &f.oracles.get(i).unwrap(),
            project_id,
            &1,
            &ph,
            &turb,
            &do_,
            &flow,
            &temp,
            &n,
            &p,
        );
    }
}

/// End-to-end (through the real contract, not the direct formula call): pH 0
/// and 1400 are accepted and finalize normally.
#[test]
fn test_submit_reading_accepts_ph_boundary_values() {
    let f = setup();

    let proj_low = BytesN::from_array(&f.e, &[7u8; 32]);
    submit_three(&f, &proj_low, 0, 10, 80, 500, 250, 8, 1);
    assert!(f.client.get_last_result(&proj_low).is_some());

    let proj_high = BytesN::from_array(&f.e, &[8u8; 32]);
    submit_three(&f, &proj_high, 1400, 10, 80, 500, 250, 8, 1);
    assert!(f.client.get_last_result(&proj_high).is_some());
}

/// End-to-end: all sensor fields at 0 finalize with zero credits, no panic.
#[test]
fn test_submit_reading_all_zero_sensor_values_no_panic() {
    let f = setup();
    let project_id = BytesN::from_array(&f.e, &[13u8; 32]);
    submit_three(&f, &project_id, 0, 0, 0, 0, 0, 0, 0);

    let res = f.client.get_last_result(&project_id).unwrap();
    assert_eq!(res.total_credits, 0);
}

// A contract-level (submit_reading) equivalent of
// test_high_baseline_with_max_flow_does_not_overflow above was intentionally
// omitted: exercising it requires set_project_config, which also configures a
// token contract for auto-minting — and finalizing with i64::MAX flow_rate
// yields a nonzero total_credits, so the mint step would invoke_contract
// against a fake `Address::generate` with no deployed contract, panicking for
// an unrelated reason (not the formula). The direct-unit-test version above
// already fully covers this scenario without that setup mismatch.

/// End-to-end: the configurable pH band, exercised through the real contract
/// update_config -> submit_reading path.
#[test]
fn test_submit_reading_configurable_ph_upper_bound() {
    let f = setup();

    let proj_default = BytesN::from_array(&f.e, &[17u8; 32]);
    submit_three(&f, &proj_default, 750, 10, 80, 500, 250, 8, 1);
    let res_default = f.client.get_last_result(&proj_default).unwrap();
    assert_eq!(res_default.quality_penalty, 2000);

    let mut config = f.client.get_config();
    config.quality_threshold_ph_max = 900;
    f.client.update_config(&f.admin, &config);

    let proj_widened = BytesN::from_array(&f.e, &[18u8; 32]);
    submit_three(&f, &proj_widened, 750, 10, 80, 500, 250, 8, 1);
    let res_widened = f.client.get_last_result(&proj_widened).unwrap();
    assert_eq!(res_widened.quality_penalty, 0);
}

/// End-to-end: max achievable penalty (7000 bps) applied to a nonzero gross
/// credit through the real contract path.
#[test]
fn test_submit_reading_all_penalty_conditions_simultaneously() {
    let f = setup();
    let project_id = BytesN::from_array(&f.e, &[14u8; 32]);
    submit_three(&f, &project_id, 300, 200, 10, 500, 350, 8, 1);

    let res = f.client.get_last_result(&project_id).unwrap();
    assert_eq!(res.quality_penalty, 7000);
    assert_eq!(res.total_credits, 30);
}

/// End-to-end: a misconfigured negative `credit_per_kg_n` floors total_credits
/// at 0 through the real contract path.
#[test]
fn test_submit_reading_negative_gross_floors_at_zero() {
    let f = setup();
    let project_id = BytesN::from_array(&f.e, &[15u8; 32]);

    let mut config = f.client.get_config();
    config.credit_per_kg_n = -1000;
    f.client.update_config(&f.admin, &config);

    submit_three(&f, &project_id, 700, 10, 80, 500, 250, 8, 1);

    let res = f.client.get_last_result(&project_id).unwrap();
    assert_eq!(res.total_credits, 0);
}
