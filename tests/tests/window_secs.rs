//! Configurable monitoring window (issue #93).
//!
//! `compute_finalization` hardcoded `3600` as the `Δt` factor in the nutrient
//! removal formula, so credits were only correct for a deployment submitting
//! exactly once per hour. A 30-minute interval double-counted; a 6-hour
//! interval under-counted 6×. There was no field anywhere in `OracleConfig`,
//! `SensorReading` or `WindowState` for an operator to declare the real
//! interval.
//!
//! `OracleConfig::window_secs` now carries it, defaulting to `3600` so existing
//! deployments are unaffected, and `update_config` constrains it to
//! `[MIN_WINDOW_SECS, MAX_WINDOW_SECS]`.
//!
//! These tests drive the **real contract entry points** — `submit_reading`
//! (direct path) and the `commit_reading`/`reveal_reading` commit-reveal path,
//! both ending in an on-chain `VerificationResult` read back through
//! `get_last_result` — plus the overflow envelope at the new upper bound.

use credit_token::{CreditToken, CreditTokenClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env, IntoVal, String, Symbol, Val, Vec,
};
use verification_oracle::{
    compute_finalization, sha256_commitment, OracleConfig, RevealParams, VerificationOracle,
    VerificationOracleClient, MAX_WINDOW_SECS, MIN_WINDOW_SECS,
};

const HALF_HOUR: u64 = 1800;
const ONE_HOUR: u64 = 3600;

struct Fixture {
    e: Env,
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
    token_client.initialize(
        &admin,
        &String::from_str(&e, "Test Credits"),
        &String::from_str(&e, "TST"),
        &BytesN::from_array(&e, &[1u8; 32]),
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
        e,
        admin,
        oracle_client,
        token_client,
        beneficiary,
        oracles,
    }
}

fn set_window_secs(f: &Fixture, window_secs: u64) {
    let mut config = f.oracle_client.get_config();
    config.window_secs = window_secs;
    f.oracle_client.update_config(&f.admin, &config);
}

// ══════════════════════════════════════════════════════════════════════════
// Real path 1 — direct `submit_reading`, per-project baselines
// ══════════════════════════════════════════════════════════════════════════

/// Sensor readings chosen so the arithmetic is exact at both window lengths
/// and the window-independent volumetric term is zero, which lets
/// `total_credits` itself be compared (see the constants below).
///
/// `flow_rate = 5` L/s → `volumetric_credit = 5 * 100 / 1000 = 0`, so every
/// credit in the total comes from nutrient removal — the part `window_secs`
/// scales. `(baseline − med) * flow = 1000 * 5 = 5000`, and `5000 * 1800` and
/// `5000 * 3600` are both exact multiples of 1 000 000, so neither result is
/// truncated by the integer division.
const DIRECT_READING: (i64, i64, i64, i64, i64, i64, i64) = (700, 10, 80, 5, 250, 8, 1);
const DIRECT_BASELINE_N: i64 = 1008; // med_n = 8   → Δ = 1000
const DIRECT_BASELINE_P: i64 = 1001; // med_p = 1   → Δ = 1000
const DIRECT_BASELINE_TEMP: i64 = 300; // med_temp = 250 → no penalty

/// Register a project and drive three real `submit_reading` calls, returning
/// the on-chain result the third submission finalizes.
fn finalize_via_submit_reading(
    f: &Fixture,
    project_seed: u8,
) -> verification_oracle::VerificationResult {
    let project_id = BytesN::from_array(&f.e, &[project_seed; 32]);
    f.oracle_client.set_project_config(
        &f.admin,
        &project_id,
        &f.token_client.address,
        &f.beneficiary,
        &DIRECT_BASELINE_N,
        &DIRECT_BASELINE_P,
        &DIRECT_BASELINE_TEMP,
    );

    let (ph, turb, do_, flow, temp, n, p) = DIRECT_READING;
    for i in 0..3u32 {
        f.oracle_client.submit_reading(
            &f.oracles.get(i).unwrap(),
            &project_id,
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

    f.oracle_client
        .get_last_result(&project_id)
        .expect("window must have finalized after three submissions")
}

/// The acceptance criterion, through the real `submit_reading` entry point:
/// `window_secs = 1800` yields exactly half the credits of `window_secs = 3600`
/// for identical readings.
#[test]
fn test_half_hour_window_yields_exactly_half_the_credits_via_submit_reading() {
    let f = setup();

    // Δ=1000, flow=5 → 1000 * 5 * 3600 / 1e6 = 18 kg of each nutrient.
    set_window_secs(&f, ONE_HOUR);
    let hourly = finalize_via_submit_reading(&f, 0x11);

    set_window_secs(&f, HALF_HOUR);
    let half_hourly = finalize_via_submit_reading(&f, 0x22);

    assert_eq!(hourly.n_removal_kg, 18);
    assert_eq!(hourly.p_removal_kg, 18);
    assert_eq!(half_hourly.n_removal_kg, 9);
    assert_eq!(half_hourly.p_removal_kg, 9);

    // flow = 5 → volumetric_credit = 5 * 100 / 1000 = 0, so the total is pure
    // nutrient-removal credit and halves exactly along with the window.
    assert_eq!(hourly.volumetric_credit, 0);
    assert_eq!(half_hourly.volumetric_credit, 0);
    assert_eq!(hourly.quality_penalty, 0);
    assert_eq!(half_hourly.quality_penalty, 0);

    // 18 * credit_per_kg_n(10) + 18 * credit_per_kg_p(20) = 540
    assert_eq!(hourly.total_credits, 540);
    assert_eq!(half_hourly.total_credits, 270);
    assert_eq!(
        half_hourly.total_credits * 2,
        hourly.total_credits,
        "a 30-minute window must credit exactly half of a 60-minute window"
    );
}

/// A 6-hour deployment under-counted 6× before this change. It must now credit
/// exactly 6× a 1-hour window for the same readings.
#[test]
fn test_six_hour_window_yields_six_times_the_credits_via_submit_reading() {
    let f = setup();

    set_window_secs(&f, ONE_HOUR);
    let hourly = finalize_via_submit_reading(&f, 0x31);

    set_window_secs(&f, 6 * ONE_HOUR);
    let six_hourly = finalize_via_submit_reading(&f, 0x32);

    assert_eq!(six_hourly.n_removal_kg, hourly.n_removal_kg * 6);
    assert_eq!(six_hourly.p_removal_kg, hourly.p_removal_kg * 6);
    assert_eq!(six_hourly.total_credits, hourly.total_credits * 6);
}

/// Backward compatibility, end to end: a freshly initialized contract that is
/// never reconfigured must produce exactly the credits the old hardcoded
/// `3600` produced.
#[test]
fn test_default_window_reproduces_the_previous_hardcoded_behaviour() {
    let f = setup();
    assert_eq!(f.oracle_client.get_config().window_secs, ONE_HOUR);

    let result = finalize_via_submit_reading(&f, 0x41);

    let (_, _, _, flow, _, med_n, med_p) = DIRECT_READING;
    assert_eq!(
        result.n_removal_kg,
        (DIRECT_BASELINE_N - med_n) as i128 * flow as i128 * 3600 / 1_000_000
    );
    assert_eq!(
        result.p_removal_kg,
        (DIRECT_BASELINE_P - med_p) as i128 * flow as i128 * 3600 / 1_000_000
    );
}

// ══════════════════════════════════════════════════════════════════════════
// Real path 2 — commit-reveal, global baselines
// ══════════════════════════════════════════════════════════════════════════

/// `finalize_reveals` uses the global baselines (`baseline_n = 10`,
/// `baseline_p = 2`), so the reading is chosen against those instead:
/// `flow = 2500` makes both `Δn * flow = 25 000` and `Δp * flow = 5 000`
/// scale exactly at 1800 s and 3600 s.
const REVEAL_READING: (i64, i64, i64, i64, i64, i64, i64) = (700, 10, 80, 2500, 250, 0, 0);

/// Drive a full commit-reveal round through the real entry points and return
/// the `VerificationResult` the third reveal finalizes.
fn finalize_via_commit_reveal(
    f: &Fixture,
    project_seed: u8,
) -> verification_oracle::VerificationResult {
    let project_id = BytesN::from_array(&f.e, &[project_seed; 32]);
    f.oracle_client.set_project_config(
        &f.admin,
        &project_id,
        &f.token_client.address,
        &f.beneficiary,
        &0,
        &0,
        &0,
    );

    let (ph, turb, do_, flow, temp, n, p) = REVEAL_READING;
    let salt = BytesN::from_array(&f.e, &[project_seed ^ 0xFF; 32]);
    let nonce = 1u64;
    let commitment = sha256_commitment(&f.e, nonce, ph, turb, do_, flow, temp, n, p, &salt);
    let reveal_params = RevealParams {
        nonce,
        ph,
        turbidity: turb,
        dissolved_oxygen: do_,
        flow_rate: flow,
        temperature: temp,
        total_nitrogen: n,
        total_phosphorus: p,
        salt,
    };

    f.oracle_client.open_window(&f.admin, &project_id);
    for i in 0..3u32 {
        f.oracle_client.commit_reading(
            &f.oracles.get(i).unwrap(),
            &project_id,
            &nonce,
            &commitment,
        );
    }

    // Advance past the commit phase in both wall clock and ledger sequence.
    f.e.ledger().with_mut(|l| {
        l.timestamp += 301;
        l.sequence_number += 61;
    });
    f.oracle_client.begin_reveal_phase(&project_id);

    let mut finalized = None;
    for i in 0..3u32 {
        finalized =
            f.oracle_client
                .reveal_reading(&f.oracles.get(i).unwrap(), &project_id, &reveal_params);
    }
    finalized.expect("third reveal must finalize the window")
}

/// The same proportionality through the commit-reveal path, which reaches
/// `compute_finalization` via `finalize_reveals` rather than
/// `submit_reading_impl` — both call sites must read the configured window.
#[test]
fn test_half_hour_window_halves_nutrient_removal_via_commit_reveal() {
    let f = setup();

    // Δn = 10 - 0 = 10, Δp = 2 - 0 = 2, flow = 2500.
    //   n: 10 * 2500 * 3600 / 1e6 = 90 kg ; at 1800 s → 45 kg
    //   p:  2 * 2500 * 3600 / 1e6 = 18 kg ; at 1800 s →  9 kg
    set_window_secs(&f, ONE_HOUR);
    let hourly = finalize_via_commit_reveal(&f, 0x51);

    set_window_secs(&f, HALF_HOUR);
    let half_hourly = finalize_via_commit_reveal(&f, 0x52);

    assert_eq!(hourly.n_removal_kg, 90);
    assert_eq!(hourly.p_removal_kg, 18);
    assert_eq!(half_hourly.n_removal_kg, 45);
    assert_eq!(half_hourly.p_removal_kg, 9);

    // The volumetric term is a flow-rate credit with no Δt factor, so it is
    // identical in both windows and the *total* is not simply halved. Pin the
    // exact expected totals so the split stays explicit:
    //   volumetric = 2500 * 100 / 1000 = 250
    //   hourly     = 90 * 10 + 18 * 20 + 250 = 1510
    //   half-hourly= 45 * 10 +  9 * 20 + 250 =  880
    assert_eq!(hourly.volumetric_credit, 250);
    assert_eq!(half_hourly.volumetric_credit, 250);
    assert_eq!(hourly.total_credits, 1510);
    assert_eq!(half_hourly.total_credits, 880);
}

// ══════════════════════════════════════════════════════════════════════════
// `update_config` validation, through the real client
// ══════════════════════════════════════════════════════════════════════════

fn try_update_window_secs(f: &Fixture, window_secs: u64) -> bool {
    let mut config: OracleConfig = f.oracle_client.get_config();
    config.window_secs = window_secs;
    f.e.try_invoke_contract::<Val, soroban_sdk::InvokeError>(
        &f.oracle_client.address,
        &Symbol::new(&f.e, "update_config"),
        soroban_sdk::vec![&f.e, f.admin.to_val(), config.into_val(&f.e)],
    )
    .is_ok()
}

/// The guard blocks a real route: out-of-range windows are rejected by the
/// live `update_config` entry point, so the formula can never see them.
#[test]
fn test_update_config_enforces_window_secs_bounds() {
    let f = setup();

    assert!(
        !try_update_window_secs(&f, 0),
        "zero-second window must be rejected"
    );
    assert!(
        !try_update_window_secs(&f, MIN_WINDOW_SECS - 1),
        "59-second window must be rejected"
    );
    assert!(
        !try_update_window_secs(&f, MAX_WINDOW_SECS + 1),
        "86401-second window must be rejected"
    );

    // Rejections must not have mutated the stored config.
    assert_eq!(f.oracle_client.get_config().window_secs, ONE_HOUR);

    assert!(try_update_window_secs(&f, MIN_WINDOW_SECS));
    assert_eq!(f.oracle_client.get_config().window_secs, MIN_WINDOW_SECS);
    assert!(try_update_window_secs(&f, MAX_WINDOW_SECS));
    assert_eq!(f.oracle_client.get_config().window_secs, MAX_WINDOW_SECS);
}

/// A rejected `window_secs` must leave credit issuance on the previously
/// configured window, not fall through to some other value.
#[test]
fn test_rejected_window_secs_does_not_change_credit_issuance() {
    let f = setup();

    set_window_secs(&f, HALF_HOUR);
    let before = finalize_via_submit_reading(&f, 0x61);

    assert!(!try_update_window_secs(&f, MAX_WINDOW_SECS + 1));
    let after = finalize_via_submit_reading(&f, 0x62);

    assert_eq!(before.total_credits, after.total_credits);
    assert_eq!(f.oracle_client.get_config().window_secs, HALF_HOUR);
}

// ══════════════════════════════════════════════════════════════════════════
// Overflow envelope at the widened upper bound
// ══════════════════════════════════════════════════════════════════════════

fn bounds_config(e: &Env) -> OracleConfig {
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

/// The direct acceptance criterion at the formula level: identical inputs,
/// `window_secs = 1800` versus `3600`, exactly half the removal.
#[test]
fn test_compute_finalization_halves_removal_at_half_the_window() {
    let e = Env::default();
    let config = bounds_config(&e);

    // Δn = 1000, Δp = 1000, flow = 5 → volumetric 0, so `total` halves too.
    let hourly = compute_finalization(&config, 700, 10, 80, 250, 5, 8, 1, 1008, 1001, 300, 3600);
    let half = compute_finalization(&config, 700, 10, 80, 250, 5, 8, 1, 1008, 1001, 300, 1800);

    assert_eq!(hourly.n_removed, 18);
    assert_eq!(hourly.p_removed, 18);
    assert_eq!(half.n_removed, 9);
    assert_eq!(half.p_removed, 9);
    assert_eq!(half.total * 2, hourly.total);
}

/// `window_secs = 0` is unreachable through `update_config`, but the formula
/// must still behave sanely (zero elapsed time, zero nutrient removal) rather
/// than dividing by zero or panicking, for any direct caller.
#[test]
fn test_zero_window_yields_zero_nutrient_removal_without_panicking() {
    let e = Env::default();
    let config = bounds_config(&e);

    let fin = compute_finalization(&config, 700, 10, 80, 250, 500, 8, 1, 1008, 1001, 300, 0);
    assert_eq!(fin.n_removed, 0);
    assert_eq!(fin.p_removed, 0);
    // Volumetric credit has no Δt factor, so it survives a zero window.
    assert_eq!(fin.volumetric_credit, 50);
}

/// Widening the ceiling from 3 600 to 86 400 makes the intermediate product 24×
/// larger, so the overflow surface of the `× window_secs` step genuinely moves.
/// A product that fits comfortably at 3 600 must still be computed exactly at
/// `MAX_WINDOW_SECS` — the guard must not have become over-eager.
#[test]
fn test_max_window_does_not_overflow_for_plausible_magnitudes() {
    let e = Env::default();
    let config = bounds_config(&e);

    // Δn = 1e9, flow = 1e9 → product 1e18, × 86 400 = 8.64e22, far below i128::MAX.
    let fin = compute_finalization(
        &config,
        700,
        10,
        80,
        250,
        1_000_000_000,
        0,
        0,
        1_000_000_000,
        1_000_000_000,
        300,
        MAX_WINDOW_SECS,
    );
    assert_eq!(
        fin.n_removed,
        1_000_000_000i128 * 1_000_000_000 * MAX_WINDOW_SECS as i128 / 1_000_000
    );
}

/// The other side of the widened envelope: a product that is safe at 3 600 but
/// overflows `i128` once multiplied by `MAX_WINDOW_SECS` must panic and revert,
/// not silently wrap into a corrupted credit amount. `1e17 * 1e17 = 1e34`;
/// `× 3 600 = 3.6e37` fits in `i128` (max ≈ 1.7e38) while `× 86 400 = 8.64e38`
/// does not.
#[test]
#[should_panic(expected = "n removal: time-window multiplication overflow")]
fn test_max_window_overflow_panics_instead_of_wrapping() {
    let e = Env::default();
    let config = bounds_config(&e);

    compute_finalization(
        &config,
        700,
        10,
        80,
        250,
        100_000_000_000_000_000, // 1e17 L/s
        0,
        0,
        100_000_000_000_000_000, // baseline_n = 1e17
        2,
        300,
        MAX_WINDOW_SECS,
    );
}

/// Sanity companion to the test above: the very same inputs are computed
/// without panicking at the previous hardcoded 3 600, which is what makes it a
/// test of the *widened* bound rather than of a pre-existing overflow.
#[test]
fn test_same_inputs_do_not_overflow_at_the_old_hardcoded_window() {
    let e = Env::default();
    let config = bounds_config(&e);

    let fin = compute_finalization(
        &config,
        700,
        10,
        80,
        250,
        100_000_000_000_000_000,
        0,
        0,
        100_000_000_000_000_000,
        2,
        300,
        3600,
    );
    assert_eq!(
        fin.n_removed,
        100_000_000_000_000_000i128 * 100_000_000_000_000_000 * 3600 / 1_000_000
    );
}

/// Guards against a future `MAX_WINDOW_SECS` bump silently reopening the
/// wraparound risk the `checked_mul` exists to prevent: the constants must stay
/// in the range the validation and the overflow analysis above assume.
#[test]
fn test_window_bounds_constants_are_the_documented_ones() {
    assert_eq!(MIN_WINDOW_SECS, 60);
    assert_eq!(MAX_WINDOW_SECS, 86_400);
}
