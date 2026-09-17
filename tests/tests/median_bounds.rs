//! Bounds tests for the fixed-size stack buffer `median_i64` sorts in.
//!
//! `median_i64` copies a window's readings into a `[i64; MEDIAN_BUFFER_LEN]`
//! stack array. `MEDIAN_BUFFER_LEN` is derived from `MAX_ORACLES_HARD_LIMIT` —
//! the ceiling `update_config` enforces on `OracleConfig::max_oracles` — so the
//! buffer always covers a fully subscribed window (a window holds at most one
//! submission per active oracle). This suite pins that coupling so it cannot be
//! broken silently: raising the oracle cap past the buffer fails here, and the
//! median is exercised at exactly buffer capacity, both directly and through a
//! full ten-oracle finalization.
//!
//! The panic-path test calls `median_i64` directly instead of going through
//! `submit_reading`: a contract panic aborts the test process in this
//! SDK/toolchain combination rather than unwinding, so `#[should_panic]` only
//! works on the exported `pub fn`. See the module doc comment in
//! `credit_formula_bounds.rs` for the full explanation of that limitation.

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, Vec};
use verification_oracle::{
    median_i64, OracleConfig, VerificationOracle, VerificationOracleClient, MAX_ORACLES_HARD_LIMIT,
    MEDIAN_BUFFER_LEN,
};

fn make_i64_vec(e: &Env, values: &[i64]) -> Vec<i64> {
    let mut v = Vec::new(e);
    for value in values {
        v.push_back(*value);
    }
    v
}

/// The buffer must hold a reading from every oracle a fully subscribed window
/// can have. If `MAX_ORACLES_HARD_LIMIT` is ever raised past the buffer size,
/// this fails (and so does the `const` assertion in the contract).
///
/// The literal `10` guards `update_config`'s rejection message, which has to
/// spell the limit out because contract panics carry string literals only.
#[test]
fn test_median_buffer_covers_max_oracles_hard_limit() {
    assert!(MEDIAN_BUFFER_LEN >= MAX_ORACLES_HARD_LIMIT as usize);
    assert_eq!(MAX_ORACLES_HARD_LIMIT, 10);
}

/// Buffer capacity is the largest input `median_i64` accepts, and it must still
/// sort correctly there.
#[test]
fn test_median_at_buffer_capacity() {
    let e = Env::default();
    // Reverse order so the insertion sort has work to do; sorted this is the
    // run `0..MEDIAN_BUFFER_LEN`, whose median is `(n - 1) / 2` under
    // truncating division for both parities of `n`.
    let values: std::vec::Vec<i64> = (0..MEDIAN_BUFFER_LEN as i64).rev().collect();
    let v = make_i64_vec(&e, &values);

    assert_eq!(v.len() as usize, MEDIAN_BUFFER_LEN);
    assert_eq!(median_i64(&v), (MEDIAN_BUFFER_LEN as i64 - 1) / 2);
}

/// One value past capacity reverts with the named bounds error instead of
/// writing past the buffer. No contract call site can reach this today —
/// `update_config` caps `max_oracles` at the buffer size — but the guard keeps
/// the failure legible if that cap is ever relaxed without resizing the buffer.
#[test]
#[should_panic(expected = "median input exceeds oracle buffer capacity")]
fn test_median_above_buffer_capacity_panics() {
    let e = Env::default();
    let values: std::vec::Vec<i64> = (0..=MEDIAN_BUFFER_LEN as i64).collect();
    let v = make_i64_vec(&e, &values);

    assert_eq!(v.len() as usize, MEDIAN_BUFFER_LEN + 1);
    median_i64(&v);
}

/// A window at the configured maximum oracle count must finalize on the median
/// of all its readings — the end-to-end check that the buffer is big enough for
/// a fully subscribed window.
///
/// Every oracle reports the healthy-system values from doc/MATH.md Example A
/// except `flow_rate`, which is spread so the median lands back on Example A's
/// `500`. The expected `total_credits` of `100` therefore matches the
/// identical-readings case: a median taken over fewer than all ten values
/// (or over an unsorted buffer) would not produce it.
#[test]
fn test_full_capacity_window_finalizes_on_median() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let staking_token = Address::generate(&e);
    let treasury = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[1u8; 32]);

    let contract_id = e.register_contract(None, VerificationOracle);
    let client = VerificationOracleClient::new(&e, &contract_id);
    client.initialize(&admin, &staking_token, &treasury);

    let oracle_count = MAX_ORACLES_HARD_LIMIT;
    let mut oracles = Vec::new(&e);
    for _ in 0..oracle_count {
        let oracle = Address::generate(&e);
        client.add_oracle(&admin, &oracle);
        oracles.push_back(oracle);
    }

    // Require every oracle to report before finalizing, so the median runs on a
    // full buffer. `min_oracles <= oracle_count` is enforced by `update_config`,
    // hence the oracles are registered first.
    client.update_config(
        &admin,
        &OracleConfig {
            min_oracles: oracle_count,
            max_oracles: oracle_count,
            quality_threshold_ph: 600,
            quality_threshold_ph_max: 700,
            quality_threshold_turbidity: 50,
            quality_threshold_do: 50,
            quality_threshold_temp: 300,
            credit_per_kg_n: 10,
            credit_per_kg_p: 20,
            staking_token,
            treasury,
            min_stake: 0,
            unstake_cooldown_secs: 86400,
            commit_phase_secs: 300,
            min_reveal_ledgers: 0,
            max_reveal_ledgers: 60,
            slash_pct_bps: 1000,
            min_slash_amount: 0,
            max_slash_amount: i128::MAX,
            window_secs: 3600,
            max_open_windows: 20,
            fee_bps: 0,
        },
    );

    // Sorted: [100, 200, 300, 400, 500, 500, 600, 700, 800, 900]
    // → median = (500 + 500) / 2 = 500, Example A's flow rate.
    let flow_rates: [i64; 10] = [900, 100, 500, 800, 200, 700, 500, 300, 600, 400];
    assert_eq!(flow_rates.len(), oracle_count as usize);

    let (ph, turbidity, dissolved_oxygen, temperature, nitrogen, phosphorus) =
        (700i64, 10i64, 80i64, 250i64, 8i64, 1i64);

    let mut result = None;
    for i in 0..oracle_count {
        result = client.submit_reading(
            &oracles.get(i).unwrap(),
            &project_id,
            &1,
            &ph,
            &turbidity,
            &dissolved_oxygen,
            &flow_rates[i as usize],
            &temperature,
            &nitrogen,
            &phosphorus,
        );
        // Only the last submission reaches the quorum.
        assert_eq!(result.is_some(), i == oracle_count - 1);
    }

    let result = result.expect("full window should finalize");
    assert_eq!(result.oracle_count, oracle_count);
    assert_eq!(result.total_credits, 100);
}
