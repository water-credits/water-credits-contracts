//!
//! This test exercises the **full finalization path** with 10 oracles, which
//! calls `median_i64` 7 times (once per sensor field) on 10-element `Vec`s.
//! The resource meter validates that the new stack-based insertion sort stays
//! well within the default Soroban CPU budget.
//!
//! Before the fix (`median_i64` allocates a second `Vec` and does O(n²) host
//! calls), this scenario would consume significantly more instructions. After
//! the fix (copy to local `[i64; 10]` array, sort on the stack, zero host
//! allocations inside `median_i64`), the cost is dominated by the existing
//! host calls for window state reads/writes, not by the median computation.
//!
//! All submissions go through the commit-reveal path (Issue #155: the plaintext
//! `submit_reading` entry point was removed as a bypass of the anti-frontrunning
//! scheme).

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env, Vec,
};
use verification_oracle::{
    sha256_commitment, OracleConfig, RevealParams, VerificationOracle, VerificationOracleClient,
};

fn setup_10_oracles(
    e: &Env,
) -> (
    Address,
    VerificationOracleClient<'static>,
    BytesN<32>,
    Vec<Address>,
) {
    let admin = Address::generate(e);
    let project_id = BytesN::from_array(e, &[1u8; 32]);

    let contract_id = e.register_contract(None, VerificationOracle);
    let client = VerificationOracleClient::new(e, &contract_id);
    let staking_token = Address::generate(e);
    let treasury = Address::generate(e);
    client.initialize(&admin, &staking_token, &treasury);

    // Add all 10 oracles first (the default max_oracles = 10 allows this),
    // then raise min_oracles to 10 — the quorum invariant requires
    // `min_oracles <= oracle_count`.
    let mut oracles = Vec::new(e);
    for _ in 0..10u32 {
        let o = Address::generate(e);
        client.add_oracle(&admin, &o);
        oracles.push_back(o);
    }

    // Disable staking and set min_oracles = 10, max_oracles = 10.
    client.update_config(
        &admin,
        &OracleConfig {
            min_oracles: 10,
            max_oracles: 10,
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

    (admin, client, project_id, oracles)
}

/// Drive a full commit-reveal round on `project_id` for all `oracles`,
/// all submitting the same `reading`. Returns the `VerificationResult`
/// produced by the final reveal (when `oracles.len() >= min_oracles`).
fn finalize_via_commit_reveal(
    e: &Env,
    admin: &Address,
    client: &VerificationOracleClient<'static>,
    project_id: &BytesN<32>,
    oracles: &Vec<Address>,
    reading: (i64, i64, i64, i64, i64, i64, i64),
    salt: &BytesN<32>,
    nonce: u64,
) -> Option<verification_oracle::VerificationResult> {
    let (ph, turb, do_, flow, temp, n, p) = reading;
    let commitment = sha256_commitment(e, nonce, ph, turb, do_, flow, temp, n, p, salt);
    let reveal_params = RevealParams {
        nonce,
        ph,
        turbidity: turb,
        dissolved_oxygen: do_,
        flow_rate: flow,
        temperature: temp,
        total_nitrogen: n,
        total_phosphorus: p,
        salt: salt.clone(),
    };

    client.open_window(admin, project_id);
    for i in 0..oracles.len() {
        client.commit_reading(&oracles.get(i).unwrap(), project_id, &nonce, &commitment);
    }

    // Advance past the commit phase (default 300 s / 60 ledgers).
    e.ledger().with_mut(|l| {
        l.timestamp += 301;
        l.sequence_number += 61;
    });
    client.begin_reveal_phase(project_id);

    let mut last = None;
    for i in 0..oracles.len() {
        last = client.reveal_reading(&oracles.get(i).unwrap(), project_id, &reveal_params);
    }
    last
}

#[test]
fn test_median_gas_with_ten_oracles_within_budget() {
    let e = Env::default();
    e.mock_all_auths();

    // Use the default test budget (not unlimited) so we can observe actual usage.
    let (admin, client, project_id, oracles) = setup_10_oracles(&e);

    // All 10 oracles submit the same "healthy system" readings from
    // doc/MATH.md Example A so the credit formula path is exercised.
    let reading: (i64, i64, i64, i64, i64, i64, i64) = (700, 10, 80, 500, 250, 8, 1);
    let salt = BytesN::from_array(&e, &[0xCCu8; 32]);
    let nonce = 1u64;
    let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
    let reveal_params = RevealParams {
        nonce,
        ph: reading.0,
        turbidity: reading.1,
        dissolved_oxygen: reading.2,
        flow_rate: reading.3,
        temperature: reading.4,
        total_nitrogen: reading.5,
        total_phosphorus: reading.6,
        salt: salt.clone(),
    };

    client.open_window(&admin, &project_id);

    // All 10 oracles commit.
    for i in 0..10u32 {
        client.commit_reading(&oracles.get(i).unwrap(), &project_id, &nonce, &commitment);
    }

    // Advance past commit phase.
    e.ledger().with_mut(|l| {
        l.timestamp += 301;
        l.sequence_number += 61;
    });
    client.begin_reveal_phase(&project_id);

    // First 9 reveals — none finalize the window (min_oracles = 10).
    for i in 0..9u32 {
        let result = client.reveal_reading(&oracles.get(i).unwrap(), &project_id, &reveal_params);
        assert!(result.is_none());
    }

    // Record budget before the 10th (finalizing) reveal.
    let budget_before = e.budget().cpu_instruction_cost();

    // 10th reveal — triggers finalization, calls median_i64 7×.
    let result =
        client.reveal_reading(&oracles.get(9).unwrap(), &project_id, &reveal_params);

    let budget_after = e.budget().cpu_instruction_cost();

    assert!(result.is_some());
    let res = result.unwrap();
    assert_eq!(res.oracle_count, 10);
    assert_eq!(res.total_credits, 100);

    // The finalization call (including 7 median computations) should consume
    // well under 5 million CPU instructions. With the old O(n²) host-call
    // insertion sort this would have been substantially higher. The exact
    // threshold is generous enough to accommodate host instrumentation
    // overhead but still catches regressions.
    let finalize_cost = budget_after - budget_before;
    assert!(
        finalize_cost < 5_000_000,
        "finalization with 10 oracles consumed {finalize_cost} CPU instructions; expected < 5M"
    );
}

#[test]
fn test_median_gas_scales_linearly_from_three_to_ten() {
    let e3 = Env::default();
    e3.mock_all_auths();
    let e10 = Env::default();
    e10.mock_all_auths();

    // ── 3-oracle setup ──
    let admin3 = Address::generate(&e3);
    let project_id3 = BytesN::from_array(&e3, &[1u8; 32]);
    let cid3 = e3.register_contract(None, VerificationOracle);
    let client3 = VerificationOracleClient::new(&e3, &cid3);
    let st3 = Address::generate(&e3);
    let tr3 = Address::generate(&e3);
    client3.initialize(&admin3, &st3, &tr3);
    // min_oracles defaults to 3 (matching the 3-oracle setup) and staking is
    // disabled by default, so no config update is needed.
    let mut oracles3 = Vec::new(&e3);
    for _ in 0..3u32 {
        let o = Address::generate(&e3);
        client3.add_oracle(&admin3, &o);
        oracles3.push_back(o);
    }

    // ── 10-oracle setup ──
    let (_admin10, client10, project_id10, oracles10) = setup_10_oracles(&e10);

    let reading: (i64, i64, i64, i64, i64, i64, i64) = (700, 10, 80, 500, 250, 8, 1);
    let salt3 = BytesN::from_array(&e3, &[0xDDu8; 32]);
    let salt10 = BytesN::from_array(&e10, &[0xDDu8; 32]);
    let nonce = 1u64;

    // ── 3-oracle: commit phase ──
    let commitment3 = sha256_commitment(&e3, nonce, reading.0, reading.1, reading.2, reading.3, reading.4, reading.5, reading.6, &salt3);
    let reveal3 = RevealParams {
        nonce,
        ph: reading.0,
        turbidity: reading.1,
        dissolved_oxygen: reading.2,
        flow_rate: reading.3,
        temperature: reading.4,
        total_nitrogen: reading.5,
        total_phosphorus: reading.6,
        salt: salt3.clone(),
    };
    client3.open_window(&admin3, &project_id3);
    for i in 0..3u32 {
        client3.commit_reading(&oracles3.get(i).unwrap(), &project_id3, &nonce, &commitment3);
    }
    e3.ledger().with_mut(|l| { l.timestamp += 301; l.sequence_number += 61; });
    client3.begin_reveal_phase(&project_id3);

    // First 2 reveals — don't finalize yet.
    for i in 0..2u32 {
        client3.reveal_reading(&oracles3.get(i).unwrap(), &project_id3, &reveal3);
    }
    let before3 = e3.budget().cpu_instruction_cost();
    let r3 = client3.reveal_reading(&oracles3.get(2).unwrap(), &project_id3, &reveal3);
    let after3 = e3.budget().cpu_instruction_cost();
    assert!(r3.is_some());
    let cost3 = after3 - before3;

    // ── 10-oracle: commit phase ──
    let commitment10 = sha256_commitment(&e10, nonce, reading.0, reading.1, reading.2, reading.3, reading.4, reading.5, reading.6, &salt10);
    let reveal10 = RevealParams {
        nonce,
        ph: reading.0,
        turbidity: reading.1,
        dissolved_oxygen: reading.2,
        flow_rate: reading.3,
        temperature: reading.4,
        total_nitrogen: reading.5,
        total_phosphorus: reading.6,
        salt: salt10.clone(),
    };
    let admin10 = Address::generate(&e10);
    // We don't have a direct handle to admin10 from setup_10_oracles, but
    // e10 has mock_all_auths so any address can act as admin.
    client10.open_window(&admin10, &project_id10);
    for i in 0..10u32 {
        client10.commit_reading(&oracles10.get(i).unwrap(), &project_id10, &nonce, &commitment10);
    }
    e10.ledger().with_mut(|l| { l.timestamp += 301; l.sequence_number += 61; });
    client10.begin_reveal_phase(&project_id10);

    // First 9 reveals — don't finalize yet.
    for i in 0..9u32 {
        client10.reveal_reading(&oracles10.get(i).unwrap(), &project_id10, &reveal10);
    }
    let before10 = e10.budget().cpu_instruction_cost();
    let r10 = client10.reveal_reading(&oracles10.get(9).unwrap(), &project_id10, &reveal10);
    let after10 = e10.budget().cpu_instruction_cost();
    assert!(r10.is_some());
    let cost10 = after10 - before10;

    // The median component now scales O(n) (copy to array) rather than
    // O(n²). The finalization cost should grow sub-quadratically from 3→10
    // oracles. With the old insertion sort, cost10 would be ~ (10²/3²) ≈
    // 11× cost3 just for median; with the new stack-based sort it's ~ 10/3
    // ≈ 3.3×. We allow a generous 8× ratio to account for the fixed host
    // overheads (window write, event emission, result storage) that don't
    // scale with oracle count.
    let ratio = cost10 as f64 / cost3 as f64;
    assert!(
        ratio < 8.0,
        "10-oracle finalization cost ({cost10}) should be less than 8× 3-oracle cost ({cost3}); got {ratio:.1}×"
    );
}
