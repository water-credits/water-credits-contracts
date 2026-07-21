//! Integration tests for Issue #26: per-project nutrient/temperature baselines.
//!
//! Before #26, `submit_reading_impl` hardcoded `baseline_n = 10` and
//! `baseline_p = 2` (and compared temperature against the global
//! `quality_threshold_temp` max) for every project. This made every project use
//! the same thresholds regardless of its actual water quality, producing
//! systematically wrong credit totals.
//!
//! These tests prove the fix: two projects with identical sensor readings but
//! different `ProjectConfig` baselines produce different credit totals, and the
//! per-project temperature baseline drives the temperature quality penalty.

use credit_token::{CreditToken, CreditTokenClient};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String, Vec};
use verification_oracle::{VerificationOracle, VerificationOracleClient};

/// Identical sensor readings for both projects. Encodings per doc/MATH.md §1:
/// ph=700 (7.00), turb=10, do=80, flow=500 L/s, temp=250 (25.0°C),
/// n=8 mg/L (raw), p=1 mg/L (raw).
const READING: (i64, i64, i64, i64, i64, i64, i64) = (700, 10, 80, 500, 250, 8, 1);

struct Fixture {
    _e: Env,
    admin: Address,
    oracle_client: VerificationOracleClient<'static>,
    token_client: CreditTokenClient<'static>,
    beneficiary: Address,
    oracles: Vec<Address>,
}

fn setup() -> Fixture {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let beneficiary = Address::generate(&e);

    let oracle_id = e.register_contract(None, VerificationOracle);
    let oracle_client = VerificationOracleClient::new(&e, &oracle_id);
    let staking_token = Address::generate(&e);
    let treasury = Address::generate(&e);
    oracle_client.initialize(&admin, &staking_token, &treasury);

    let token_id = e.register_contract(None, CreditToken);
    let token_client = CreditTokenClient::new(&e, &token_id);
    let token_project_id = BytesN::from_array(&e, &[1u8; 32]);
    token_client.initialize(
        &admin,
        &String::from_str(&e, "Test Credits"),
        &String::from_str(&e, "TST"),
        &token_project_id,
        &String::from_str(&e, "Wetland_Restoration_v1.0"),
    );
    token_client.set_minter(&admin, &oracle_id);

    // Disable the min-stake requirement so oracles can be added without funding.
    let mut config = oracle_client.get_config();
    config.min_stake = 0;
    oracle_client.update_config(&admin, &config);

    let mut oracles = Vec::new(&e);
    for _ in 0..3u32 {
        let o = Address::generate(&e);
        oracle_client.add_oracle(&admin, &o);
        oracles.push_back(o);
    }

    Fixture {
        _e: e,
        admin,
        oracle_client,
        token_client,
        beneficiary,
        oracles,
    }
}

/// Configure a project with the given per-project baselines and return its id.
fn configure_project(
    f: &Fixture,
    baseline_n: i64,
    baseline_p: i64,
    baseline_temp: i64,
) -> BytesN<32> {
    // Derive a distinct project id from the baselines so each test project is unique.
    let mut seed = [0u8; 32];
    seed[0] = (baseline_n & 0xff) as u8;
    seed[1] = (baseline_p & 0xff) as u8;
    seed[2] = (baseline_temp & 0xff) as u8;
    let project_id = BytesN::from_array(&f._e, &seed);
    f.oracle_client.set_project_config(
        &f.admin,
        &project_id,
        &f.token_client.address,
        &f.beneficiary,
        &baseline_n,
        &baseline_p,
        &baseline_temp,
    );
    project_id
}

fn submit_three(f: &Fixture, project_id: &BytesN<32>) {
    let (ph, turb, do_, flow, temp, n, p) = READING;
    for i in 0..3u32 {
        f.oracle_client.submit_reading(
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

/// Acceptance criterion: two projects with different baselines produce different
/// credit totals for identical sensor readings.
#[test]
fn test_different_baselines_produce_different_credits() {
    let f = setup();

    // Project A: low baselines (agricultural runoff zone, naturally high load).
    let proj_a = configure_project(&f, 25, 5, 300);
    // Project B: high baselines (urban stormwater, naturally low load).
    let proj_b = configure_project(&f, 3, 1, 300);

    submit_three(&f, &proj_a);
    submit_three(&f, &proj_b);

    let res_a = f.oracle_client.get_last_result(&proj_a).unwrap();
    let res_b = f.oracle_client.get_last_result(&proj_b).unwrap();

    // Identical medians, so flow/volumetric are equal; only N/P removal differs.
    // Project A (baseline_n=25, baseline_p=5): n_removed=(25-8)=17, p_removed=(5-1)=4.
    // Project B (baseline_n=3,  baseline_p=1): n_removed=0 (med_n 8 > 3), p_removed=0.
    // Therefore A's total credits must exceed B's.
    assert!(
        res_a.total_credits > res_b.total_credits,
        "project A (low baselines) should yield more credits than project B (high baselines): A={} B={}",
        res_a.total_credits,
        res_b.total_credits
    );
    assert_eq!(
        res_a.n_removal_kg,
        (25 - 8) as i128 * 500 * 3600 / 1_000_000
    );
    assert_eq!(res_b.n_removal_kg, 0);
    assert_eq!(res_a.p_removal_kg, (5 - 1) as i128 * 500 * 3600 / 1_000_000);
    assert_eq!(res_b.p_removal_kg, 0);
}

/// Acceptance criterion: a project that does not set its own baselines falls back
/// to the global OracleConfig defaults (baseline_n=10, baseline_p=2, temp=300),
/// preserving the previous hardcoded behavior.
#[test]
fn test_unset_baseline_falls_back_to_default() {
    let f = setup();

    // Call set_project_config with zero baselines → must behave like defaults.
    let proj = configure_project(&f, 0, 0, 0);
    submit_three(&f, &proj);

    let res = f.oracle_client.get_last_result(&proj).unwrap();
    // med_n=8 < default 10 → n_removed=(10-8)=2; med_p=1 < default 2 → p_removed=(2-1)=1.
    assert_eq!(res.n_removal_kg, (10 - 8) as i128 * 500 * 3600 / 1_000_000);
    assert_eq!(res.p_removal_kg, (2 - 1) as i128 * 500 * 3600 / 1_000_000);
}

/// Acceptance criterion: the per-project temperature baseline drives the
/// temperature quality penalty, independent of the global max threshold.
#[test]
fn test_per_project_temp_baseline_drives_penalty() {
    let f = setup();

    // Both projects get the SAME identical readings (temp=250 → 25.0°C encoded).
    // Project A: temp baseline 300 (30°C) → 250 < 300 → NO temp penalty.
    // Project B: temp baseline 200 (20°C) → 250 > 200 → temp penalty (+1000 bps).
    let proj_a = configure_project(&f, 10, 2, 300);
    let proj_b = configure_project(&f, 10, 2, 200);

    submit_three(&f, &proj_a);
    submit_three(&f, &proj_b);

    let res_a = f.oracle_client.get_last_result(&proj_a).unwrap();
    let res_b = f.oracle_client.get_last_result(&proj_b).unwrap();

    // All other medians identical → only the temperature penalty differs.
    assert_eq!(res_a.quality_penalty + 1000, res_b.quality_penalty);
    assert!(
        res_b.total_credits < res_a.total_credits,
        "project B (lower temp baseline) should be penalized more: A_pen={} B_pen={}",
        res_a.quality_penalty,
        res_b.quality_penalty
    );
}

/// Edge case: very high baselines produce large removal credits without overflow.
/// Verifies the arithmetic path (baseline - med) * flow * 3600 / 1_000_000
/// does not panic when baseline is much larger than med_n.
#[test]
fn test_high_baseline_no_overflow() {
    let f = setup();

    // baseline_n = 1000 mg/L, med_n = 8 → diff = 992, flow = 500
    // n_removed = 992 * 500 * 3600 / 1_000_000 = 1785 (well within i128)
    let proj = configure_project(&f, 1000, 100, 300);
    submit_three(&f, &proj);

    let res = f.oracle_client.get_last_result(&proj).unwrap();
    assert_eq!(
        res.n_removal_kg,
        (1000 - 8) as i128 * 500 * 3600 / 1_000_000
    );
    assert_eq!(res.p_removal_kg, (100 - 1) as i128 * 500 * 3600 / 1_000_000);
}

/// Edge case: med_n exactly equals baseline_n → zero removal (boundary test).
#[test]
fn test_reading_equals_baseline_zero_removal() {
    let f = setup();

    // med_n = 10 = baseline_n → n_removed = 0
    // med_p = 2 = baseline_p → p_removed = 0
    // Use a custom reading where n=10, p=2.
    let proj = configure_project(&f, 10, 2, 300);

    let (ph, turb, do_, flow, temp, _, _) = READING;
    for i in 0..3u32 {
        f.oracle_client.submit_reading(
            &f.oracles.get(i).unwrap(),
            &proj,
            &1,
            &ph,
            &turb,
            &do_,
            &flow,
            &temp,
            &10, // n = baseline_n exactly
            &2,  // p = baseline_p exactly
        );
    }

    let res = f.oracle_client.get_last_result(&proj).unwrap();
    assert_eq!(
        res.n_removal_kg, 0,
        "n_removed should be 0 when med_n == baseline_n"
    );
    assert_eq!(
        res.p_removal_kg, 0,
        "p_removed should be 0 when med_p == baseline_p"
    );
}

/// Edge case: reading above baseline → zero removal (no negative credits).
#[test]
fn test_reading_above_baseline_zero_removal() {
    let f = setup();

    // baseline_n = 5, but med_n = 8 > 5 → n_removed = 0
    // baseline_p = 1, but med_p = 1 == 1 → p_removed = 0
    let proj = configure_project(&f, 5, 1, 300);

    let (ph, turb, do_, flow, temp, _, _) = READING;
    for i in 0..3u32 {
        f.oracle_client.submit_reading(
            &f.oracles.get(i).unwrap(),
            &proj,
            &1,
            &ph,
            &turb,
            &do_,
            &flow,
            &temp,
            &8, // n > baseline_n
            &1, // p == baseline_p
        );
    }

    let res = f.oracle_client.get_last_result(&proj).unwrap();
    assert_eq!(res.n_removal_kg, 0);
    assert_eq!(res.p_removal_kg, 0);
}
