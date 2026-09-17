//! Tests for Issue #155: `submit_reading` plaintext path removed.
//!
//! Verifies the acceptance criterion from the issue:
//! > After the change, a plaintext single-call submission is rejected, and
//! > every production finalization goes through commit-reveal.
//!
//! `submit_reading` now panics unconditionally with a deprecation message.
//! The Soroban test-contract client's `try_submit_reading` variant catches
//! the contract panic as an `Err`, letting us assert the rejection without
//! aborting the test process (unlike the plain `submit_reading` call which
//! would cause a non-unwinding panic — see the note in `credit_formula_bounds`
//! about why the test host cannot catch contract panics through the normal
//! dispatch path).

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};
use verification_oracle::{VerificationOracle, VerificationOracleClient};

fn setup_oracle(e: &Env) -> (Address, Address, VerificationOracleClient<'static>) {
    let admin = Address::generate(e);
    let oracle = Address::generate(e);
    let contract_id = e.register_contract(None, VerificationOracle);
    let client = VerificationOracleClient::new(e, &contract_id);
    let staking_token = Address::generate(e);
    let treasury = Address::generate(e);
    client.initialize(&admin, &staking_token, &treasury);
    client.add_oracle(&admin, &oracle);
    (admin, oracle, client)
}

/// The plaintext `submit_reading` entry point must be rejected for an active,
/// whitelisted oracle with a valid nonce and valid sensor values.
/// This is the core acceptance criterion of Issue #155.
#[test]
fn test_submit_reading_is_rejected_for_active_oracle() {
    let e = Env::default();
    e.mock_all_auths();

    let (_admin, oracle, client) = setup_oracle(&e);
    let project_id = BytesN::from_array(&e, &[1u8; 32]);

    let result = client.try_submit_reading(
        &oracle,
        &project_id,
        &1,
        &700,
        &10,
        &80,
        &500,
        &250,
        &8,
        &1,
    );
    assert!(
        result.is_err(),
        "submit_reading must be rejected: commit_reading + reveal_reading is the only valid path"
    );
}

/// The plaintext `submit_reading` entry point must also be rejected when
/// called by a non-whitelisted address — the deprecation check fires before
/// any oracle-active check, so any caller gets the same rejection.
#[test]
fn test_submit_reading_is_rejected_for_any_caller() {
    let e = Env::default();
    e.mock_all_auths();

    let (_admin, _oracle, client) = setup_oracle(&e);
    let project_id = BytesN::from_array(&e, &[2u8; 32]);
    let random = Address::generate(&e);

    let result = client.try_submit_reading(
        &random,
        &project_id,
        &1,
        &700,
        &10,
        &80,
        &500,
        &250,
        &8,
        &1,
    );
    assert!(
        result.is_err(),
        "submit_reading must be rejected regardless of caller identity"
    );
}

/// After `submit_reading` is rejected, the commit-reveal path still works
/// normally — the rejection does not corrupt any contract state.
#[test]
fn test_commit_reveal_succeeds_after_submit_reading_rejection() {
    use soroban_sdk::testutils::Ledger as _;
    use verification_oracle::{sha256_commitment, RevealParams};

    let e = Env::default();
    e.mock_all_auths();

    let (admin, oracle, client) = setup_oracle(&e);
    // min_oracles defaults to 3; add two more oracles.
    let oracle2 = Address::generate(&e);
    let oracle3 = Address::generate(&e);
    client.add_oracle(&admin, &oracle2);
    client.add_oracle(&admin, &oracle3);

    let project_id = BytesN::from_array(&e, &[3u8; 32]);

    // Attempt (and fail) the removed plaintext path.
    let rejected = client.try_submit_reading(
        &oracle,
        &project_id,
        &1,
        &700,
        &10,
        &80,
        &500,
        &250,
        &8,
        &1,
    );
    assert!(rejected.is_err());

    // Now run a proper commit-reveal round — it must succeed cleanly.
    let (ph, turb, do_, flow, temp, n, p) = (700i64, 10i64, 80i64, 500i64, 250i64, 8i64, 1i64);
    let salt = BytesN::from_array(&e, &[0xFFu8; 32]);
    let nonce: u64 = 1;
    let commitment = sha256_commitment(&e, nonce, ph, turb, do_, flow, temp, n, p, &salt);
    let params = RevealParams {
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

    client.open_window(&admin, &project_id);
    for o in [&oracle, &oracle2, &oracle3] {
        client.commit_reading(o, &project_id, &nonce, &commitment);
    }

    e.ledger().with_mut(|l| {
        l.timestamp += 301;
        l.sequence_number += 61;
    });
    client.begin_reveal_phase(&project_id);

    let mut result = None;
    for o in [&oracle, &oracle2, &oracle3] {
        result = client.reveal_reading(o, &project_id, &params);
    }

    let res = result.expect("commit-reveal round must finalize after three reveals");
    assert_eq!(res.oracle_count, 3);
    assert!(
        res.total_credits > 0,
        "finalization must produce positive credits"
    );
}
