//! Cross-contract edge case integration tests (issue #64).
//!
//! Complements `full_lifecycle.rs` (happy path) by covering failure and
//! boundary interactions between contracts: paused tokens, supply caps,
//! unauthorized registry callers, partial governance batch failures,
//! post-deploy admin races, concurrent oracle windows, mid-round slashing,
//! and pause timing during reveal.
//!
//! NOTE: two spots below are marked ASSUMPTION because they weren't
//! confirmed against the actual source during pairing — `credit_token`
//! having a `transfer_admin` function (test 5), and `verification_oracle`
//! having its own `pause`/`unpause` (test 8). If either doesn't exist,
//! `cargo test` will fail to compile with a clear "no method named ..."
//! error — just tell me the error and I'll adjust that one test.

use credit_factory::{CreditFactory, CreditFactoryClient};
use credit_token::{CreditToken, CreditTokenClient};
use governance::{Governance, GovernanceAction, GovernanceClient};
use retirement_registry::{RetirementRegistry, RetirementRegistryClient};
use soroban_sdk::{
    contract, contractimpl, symbol_short,
    testutils::{Address as _, Ledger},
    vec, Address, Bytes, BytesN, Env, IntoVal, String, Symbol, Val, Vec as SVec,
};
use verification_oracle::{
    sha256_commitment, OracleConfig, RevealParams, VerificationOracle, VerificationOracleClient,
};

const LEDGER_TIMESTAMP: u64 = 1_752_710_400;

fn base_config(
    e: &Env,
    staking_token: Address,
    treasury: Address,
    min_stake: i128,
) -> OracleConfig {
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
        staking_token,
        treasury,
        min_stake,
        unstake_cooldown_secs: 86400,
        commit_phase_secs: 300,
        min_reveal_ledgers: 0,
        max_reveal_ledgers: 60,
    }
}

fn reveal_params(e: &Env, nonce: u64) -> (RevealParams, BytesN<32>) {
    let salt = BytesN::from_array(e, &[0xF1u8; 32]);
    let params = RevealParams {
        nonce,
        ph: 700,
        turbidity: 10,
        dissolved_oxygen: 80,
        flow_rate: 500,
        temperature: 250,
        total_nitrogen: 8,
        total_phosphorus: 1,
        salt: salt.clone(),
    };
    (params, salt)
}

/// Deploys oracle + a native credit_token (no WASM upload needed since
/// invoke_contract works identically against native test contracts),
/// wires minter + project config, and returns everything needed to run a
/// commit/reveal round.
fn setup_oracle_and_token(
    e: &Env,
    admin: &Address,
    project_owner: &Address,
    min_stake: i128,
) -> (
    VerificationOracleClient<'static>,
    CreditTokenClient<'static>,
    BytesN<32>,
) {
    let token_id = e.register_contract(None, CreditToken);
    let token = CreditTokenClient::new(e, &token_id);
    token.initialize(
        admin,
        &String::from_str(e, "Test Wetland"),
        &String::from_str(e, "WC"),
        &7,
        &BytesN::from_array(e, &[9u8; 32]),
        &String::from_str(e, "Wetland_Restoration_v2.1"),
    );

    let oracle_id = e.register_contract(None, VerificationOracle);
    let oracle = VerificationOracleClient::new(e, &oracle_id);
    let staking_token = Address::generate(e);
    let treasury = Address::generate(e);
    oracle.initialize(admin, &staking_token, &treasury);
    oracle.update_config(admin, &base_config(e, staking_token, treasury, min_stake));

    token.set_minter(admin, &oracle_id);

    let project_id = BytesN::from_array(e, &[42u8; 32]);
    oracle.set_project_config(admin, &project_id, &token_id, project_owner, &10, &2, &300);

    (oracle, token, project_id)
}

fn add_three_oracles(
    e: &Env,
    admin: &Address,
    oracle: &VerificationOracleClient,
) -> (Address, Address, Address) {
    let o1 = Address::generate(e);
    let o2 = Address::generate(e);
    let o3 = Address::generate(e);
    oracle.add_oracle(admin, &o1);
    oracle.add_oracle(admin, &o2);
    oracle.add_oracle(admin, &o3);
    (o1, o2, o3)
}

fn run_commit_phase(
    e: &Env,
    oracle: &VerificationOracleClient,
    project_id: &BytesN<32>,
    oracles: &[Address],
    nonce: u64,
    commitment: &BytesN<32>,
) {
    oracle.open_window(&oracle.get_config().staking_token, project_id); // placeholder replaced below
}

// ─────────────────────────────────────────────────────────────────────────
// Test 1 — Oracle finalization when token is paused
// ─────────────────────────────────────────────────────────────────────────
#[test]
fn test_finalize_reveals_when_token_paused_reverts_whole_call() {
    let e = Env::default();
    e.mock_all_auths();
    e.ledger().with_mut(|l| l.timestamp = LEDGER_TIMESTAMP);

    let admin = Address::generate(&e);
    let project_owner = Address::generate(&e);
    let (oracle, token, project_id) = setup_oracle_and_token(&e, &admin, &project_owner, 0);
    let (o1, o2, o3) = add_three_oracles(&e, &admin, &oracle);

    let nonce = 1u64;
    let (params, salt) = reveal_params(&e, nonce);
    let commitment = sha256_commitment(
        &e,
        nonce,
        params.ph,
        params.turbidity,
        params.dissolved_oxygen,
        params.flow_rate,
        params.temperature,
        params.total_nitrogen,
        params.total_phosphorus,
        &salt,
    );

    oracle.open_window(&admin, &project_id);
    oracle.commit_reading(&o1, &project_id, &nonce, &commitment);
    oracle.commit_reading(&o2, &project_id, &nonce, &commitment);
    oracle.commit_reading(&o3, &project_id, &nonce, &commitment);

    e.ledger().with_mut(|l| {
        l.timestamp += 301;
        l.sequence_number += 61;
    });
    oracle.begin_reveal_phase(&project_id);

    oracle.reveal_reading(&o1, &project_id, &params);
    oracle.reveal_reading(&o2, &project_id, &params);

    // Pause the token right before the finalizing reveal.
    token.pause(&admin);
    assert!(token.paused());

    // Third reveal triggers finalize -> mint_credits_respecting_cap ->
    // token.mint_to(), which should panic while paused. Soroban reverts
    // the ENTIRE call on panic, so use try_invoke to observe the failure
    // without crashing the test process.
    let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
        &oracle.address,
        &Symbol::new(&e, "reveal_reading"),
        vec![&e, o3.to_val(), project_id.to_val(), params.into_val(&e)],
    );
    assert!(result.is_err(), "reveal must fail while token is paused");

    // Because Soroban reverts all state on panic, the window must NOT be
    // finalized and no credits minted — this is the key assertion for
    // this edge case, not just "it errored".
    assert!(oracle.get_last_result(&project_id).is_none());
    assert_eq!(token.total_supply(), 0);
}

// ─────────────────────────────────────────────────────────────────────────
// Test 2 — Oracle finalization when token max_supply is reached
// ─────────────────────────────────────────────────────────────────────────
#[test]
fn test_finalize_reveals_credits_capped_at_max_supply() {
    let e = Env::default();
    e.mock_all_auths();
    e.ledger().with_mut(|l| l.timestamp = LEDGER_TIMESTAMP);

    let admin = Address::generate(&e);
    let project_owner = Address::generate(&e);
    let (oracle, token, project_id) = setup_oracle_and_token(&e, &admin, &project_owner, 0);
    let (o1, o2, o3) = add_three_oracles(&e, &admin, &oracle);

    // Cap max_supply below the 100 credits the formula would produce.
    token.set_max_supply(&admin, &40);

    let nonce = 1u64;
    let (params, salt) = reveal_params(&e, nonce);
    let commitment = sha256_commitment(
        &e,
        nonce,
        params.ph,
        params.turbidity,
        params.dissolved_oxygen,
        params.flow_rate,
        params.temperature,
        params.total_nitrogen,
        params.total_phosphorus,
        &salt,
    );

    oracle.open_window(&admin, &project_id);
    oracle.commit_reading(&o1, &project_id, &nonce, &commitment);
    oracle.commit_reading(&o2, &project_id, &nonce, &commitment);
    oracle.commit_reading(&o3, &project_id, &nonce, &commitment);
    e.ledger().with_mut(|l| {
        l.timestamp += 301;
        l.sequence_number += 61;
    });
    oracle.begin_reveal_phase(&project_id);
    oracle.reveal_reading(&o1, &project_id, &params);
    oracle.reveal_reading(&o2, &project_id, &params);
    let result = oracle
        .reveal_reading(&o3, &project_id, &params)
        .expect("must finalize even when capped");

    // Formula still computes the full 100 credits...
    assert_eq!(result.total_credits, 100);
    // ...but only 40 are actually minted, respecting the cap.
    assert_eq!(result.credits_minted, 40);
    assert_eq!(token.total_supply(), 40);
    assert_eq!(token.balance(&project_owner), 40);
}

// ─────────────────────────────────────────────────────────────────────────
// Test 3 — Retirement registry linked but caller not authorized
// ─────────────────────────────────────────────────────────────────────────
#[test]
fn test_retire_reverts_when_token_not_authorized_on_registry() {
    let e = Env::default();
    e.mock_all_auths();
    e.ledger().with_mut(|l| l.timestamp = LEDGER_TIMESTAMP);

    let admin = Address::generate(&e);
    let holder = Address::generate(&e);

    let token_id = e.register_contract(None, CreditToken);
    let token = CreditTokenClient::new(&e, &token_id);
    token.initialize(
        &admin,
        &String::from_str(&e, "Test Wetland"),
        &String::from_str(&e, "WC"),
        &7,
        &BytesN::from_array(&e, &[9u8; 32]),
        &String::from_str(&e, "Wetland_Restoration_v2.1"),
    );
    token.set_minter(&admin, &admin);
    token.mint_to(&admin, &holder, &100);

    let registry_id = e.register_contract(None, RetirementRegistry);
    let registry = RetirementRegistryClient::new(&e, &registry_id);
    registry.initialize(&admin);

    // Link the registry, but deliberately DO NOT call
    // set_authorized_caller — this is the edge case.
    token.set_retirement_registry(&admin, &registry_id);

    let purpose = String::from_str(&e, "voluntary offset");
    let uri = String::from_str(&e, "ipfs://Qmtest");
    let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
        &token_id,
        &Symbol::new(&e, "retire"),
        vec![
            &e,
            holder.to_val(),
            10i128.into_val(&e),
            purpose.to_val(),
            uri.to_val(),
        ],
    );
    assert!(
        result.is_err(),
        "retire must fail: token not authorized on registry"
    );

    // Whole call reverted — balance and supply untouched, no cert created.
    assert_eq!(token.balance(&holder), 100);
    assert_eq!(token.total_retired(), 0);
    assert_eq!(registry.record_count(), 0);
}

// ─────────────────────────────────────────────────────────────────────────
// Test 4 — Governance batch: one action succeeds, one panics
// ─────────────────────────────────────────────────────────────────────────
#[contract]
pub struct MockTarget;

#[contractimpl]
impl MockTarget {
    pub fn set_value(e: Env, val: i128) {
        e.storage().instance().set(&Symbol::new(&e, "val"), &val);
    }
    pub fn get_value(e: Env) -> Option<i128> {
        e.storage().instance().get(&Symbol::new(&e, "val"))
    }
    pub fn always_fail(_e: Env) {
        panic!("always fails");
    }
}

#[test]
fn test_governance_execute_reverts_whole_batch_on_one_panic() {
    let e = Env::default();
    e.mock_all_auths();
    e.ledger().with_mut(|l| l.timestamp = LEDGER_TIMESTAMP);

    let admin = Address::generate(&e);
    let member = Address::generate(&e);

    let gov_id = e.register_contract(None, Governance);
    let gov = GovernanceClient::new(&e, &gov_id);
    gov.initialize(&admin, &vec![&e, admin.clone(), member.clone()]);

    let mock_id = e.register_contract(None, MockTarget);
    let mock = MockTargetClient::new(&e, &mock_id);
    assert_eq!(mock.get_value(), None);

    let action_ok = GovernanceAction {
        target: mock_id.clone(),
        function: Symbol::new(&e, "set_value"),
        args: vec![&e, 42i128.into_val(&e)],
    };
    let action_panics = GovernanceAction {
        target: mock_id.clone(),
        function: Symbol::new(&e, "always_fail"),
        args: SVec::new(&e),
    };
    let actions = vec![&e, action_ok, action_panics];

    let proposal_id = gov.propose(
        &member,
        &String::from_str(&e, "Mixed batch"),
        &String::from_str(&e, "one ok, one panics"),
        &actions,
    );
    gov.vote(&admin, &proposal_id, &true);
    gov.vote(&member, &proposal_id, &true);

    // Jump past timelock so execute() is callable.
    let config = gov.get_config();
    e.ledger()
        .with_mut(|l| l.timestamp += config.timelock_period + 1);

    let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
        &gov_id,
        &Symbol::new(&e, "execute"),
        vec![&e, proposal_id.into_val(&e)],
    );
    assert!(
        result.is_err(),
        "execute must revert when any action panics"
    );

    // Because Soroban reverts the whole call, the FIRST action's effect
    // (set_value(42)) must also be rolled back — this is the actual thing
    // this test is verifying, not just that execute() errored.
    assert_eq!(mock.get_value(), None);

    let proposal = gov.get_proposal(&proposal_id).unwrap();
    assert!(matches!(
        proposal.status,
        governance::ProposalStatus::Approved
    ));
}

// ─────────────────────────────────────────────────────────────────────────
// Test 5 — Factory deploys token, admin transfers before minter is set
// ─────────────────────────────────────────────────────────────────────────
#[test]
fn test_set_minter_fails_after_admin_transferred_before_wiring() {
    let e = Env::default();
    e.mock_all_auths();
    e.budget().reset_unlimited();
    e.ledger().with_mut(|l| l.timestamp = LEDGER_TIMESTAMP);

    let admin = Address::generate(&e);
    let new_admin = Address::generate(&e);
    let owner = Address::generate(&e);
    let oracle_id_placeholder = Address::generate(&e);

    let wasm_bytes = std::fs::read(env!("CREDIT_TOKEN_WASM"))
        .expect("credit_token.wasm should have been built by tests/build.rs");
    let token_wasm_hash = e
        .deployer()
        .upload_contract_wasm(Bytes::from_slice(&e, &wasm_bytes));

    let factory_id = e.register_contract(None, CreditFactory);
    let factory = CreditFactoryClient::new(&e, &factory_id);
    factory.initialize(&admin);

    let project_id = factory.register_project(
        &admin,
        &String::from_str(&e, "Race Condition Wetland"),
        &0i64,
        &0i64,
        &String::from_str(&e, "Wetland_Restoration_v2.1"),
        &owner,
        &10u64,
        &token_wasm_hash,
    );
    let project = factory.get_project(&project_id).unwrap();
    let token = CreditTokenClient::new(&e, &project.credit_token);

    // ASSUMPTION: credit_token exposes transfer_admin(admin, new_admin),
    // mirroring verification_oracle's transfer_admin. Adjust the function
    // name here if credit_token calls it something else.
    token.transfer_admin(&admin, &new_admin);

    // Original admin no longer matches stored admin — set_minter must fail.
    let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
        &project.credit_token,
        &Symbol::new(&e, "set_minter"),
        vec![&e, admin.to_val(), oracle_id_placeholder.to_val()],
    );
    assert!(
        result.is_err(),
        "old admin must not be able to set_minter after transfer"
    );

    // New admin CAN wire it correctly.
    token.set_minter(&new_admin, &oracle_id_placeholder);
}

// ─────────────────────────────────────────────────────────────────────────
// Test 6 — Multiple projects sharing one oracle, concurrent windows
// ─────────────────────────────────────────────────────────────────────────
#[test]
fn test_shared_oracle_concurrent_project_windows_are_isolated() {
    let e = Env::default();
    e.mock_all_auths();
    e.ledger().with_mut(|l| l.timestamp = LEDGER_TIMESTAMP);

    let admin = Address::generate(&e);
    let owner_a = Address::generate(&e);
    let owner_b = Address::generate(&e);

    let oracle_id = e.register_contract(None, VerificationOracle);
    let oracle = VerificationOracleClient::new(&e, &oracle_id);
    let staking_token = Address::generate(&e);
    let treasury = Address::generate(&e);
    oracle.initialize(&admin, &staking_token, &treasury);
    oracle.update_config(&admin, &base_config(&e, staking_token, treasury, 0));

    let token_a_id = e.register_contract(None, CreditToken);
    let token_a = CreditTokenClient::new(&e, &token_a_id);
    token_a.initialize(
        &admin,
        &String::from_str(&e, "A"),
        &String::from_str(&e, "WC"),
        &7,
        &BytesN::from_array(&e, &[1u8; 32]),
        &String::from_str(&e, "m"),
    );
    token_a.set_minter(&admin, &oracle_id);

    let token_b_id = e.register_contract(None, CreditToken);
    let token_b = CreditTokenClient::new(&e, &token_b_id);
    token_b.initialize(
        &admin,
        &String::from_str(&e, "B"),
        &String::from_str(&e, "WC"),
        &7,
        &BytesN::from_array(&e, &[2u8; 32]),
        &String::from_str(&e, "m"),
    );
    token_b.set_minter(&admin, &oracle_id);

    let project_a = BytesN::from_array(&e, &[0xAAu8; 32]);
    let project_b = BytesN::from_array(&e, &[0xBBu8; 32]);
    oracle.set_project_config(&admin, &project_a, &token_a_id, &owner_a, &10, &2, &300);
    oracle.set_project_config(&admin, &project_b, &token_b_id, &owner_b, &10, &2, &300);

    let (o1, o2, o3) = add_three_oracles(&e, &admin, &oracle);

    let nonce = 1u64;
    let (params, salt) = reveal_params(&e, nonce);
    let commitment = sha256_commitment(
        &e,
        nonce,
        params.ph,
        params.turbidity,
        params.dissolved_oxygen,
        params.flow_rate,
        params.temperature,
        params.total_nitrogen,
        params.total_phosphorus,
        &salt,
    );

    // Open BOTH windows before committing to either — this is the
    // "concurrent" part: the oracle set is mid-round on two projects at once.
    oracle.open_window(&admin, &project_a);
    oracle.open_window(&admin, &project_b);

    // Interleave commits across both projects.
    oracle.commit_reading(&o1, &project_a, &nonce, &commitment);
    oracle.commit_reading(&o1, &project_b, &nonce, &commitment);
    oracle.commit_reading(&o2, &project_a, &nonce, &commitment);
    oracle.commit_reading(&o2, &project_b, &nonce, &commitment);
    oracle.commit_reading(&o3, &project_a, &nonce, &commitment);
    oracle.commit_reading(&o3, &project_b, &nonce, &commitment);

    e.ledger().with_mut(|l| {
        l.timestamp += 301;
        l.sequence_number += 61;
    });
    oracle.begin_reveal_phase(&project_a);
    oracle.begin_reveal_phase(&project_b);

    oracle.reveal_reading(&o1, &project_a, &params);
    oracle.reveal_reading(&o1, &project_b, &params);
    oracle.reveal_reading(&o2, &project_a, &params);
    oracle.reveal_reading(&o2, &project_b, &params);
    let result_a = oracle.reveal_reading(&o3, &project_a, &params).unwrap();
    let result_b = oracle.reveal_reading(&o3, &project_b, &params).unwrap();

    // Each project's window resolved independently and minted to its own
    // beneficiary/token — no cross-contamination.
    assert_eq!(result_a.project_id, project_a);
    assert_eq!(result_b.project_id, project_b);
    assert_eq!(token_a.balance(&owner_a), 100);
    assert_eq!(token_b.balance(&owner_b), 100);
    assert_eq!(token_a.balance(&owner_b), 0);
    assert_eq!(token_b.balance(&owner_a), 0);
}

// ─────────────────────────────────────────────────────────────────────────
// Test 7 — Oracle stake slashed below min_stake mid commit-reveal round
// ─────────────────────────────────────────────────────────────────────────
#[test]
fn test_reveal_fails_after_stake_slashed_below_min_mid_round() {
    let e = Env::default();
    e.mock_all_auths();
    e.ledger().with_mut(|l| l.timestamp = LEDGER_TIMESTAMP);

    let admin = Address::generate(&e);
    let project_owner = Address::generate(&e);
    let (oracle, _token, project_id) = setup_oracle_and_token(&e, &admin, &project_owner, 1000);

    let o1 = Address::generate(&e);
    let o2 = Address::generate(&e);
    let o3 = Address::generate(&e);
    oracle.stake(&o1, &1500);
    oracle.stake(&o2, &1500);
    oracle.stake(&o3, &1500);
    oracle.add_oracle(&admin, &o1);
    oracle.add_oracle(&admin, &o2);
    oracle.add_oracle(&admin, &o3);

    let nonce = 1u64;
    let (params, salt) = reveal_params(&e, nonce);
    let commitment = sha256_commitment(
        &e,
        nonce,
        params.ph,
        params.turbidity,
        params.dissolved_oxygen,
        params.flow_rate,
        params.temperature,
        params.total_nitrogen,
        params.total_phosphorus,
        &salt,
    );

    oracle.open_window(&admin, &project_id);
    oracle.commit_reading(&o1, &project_id, &nonce, &commitment);
    oracle.commit_reading(&o2, &project_id, &nonce, &commitment);
    oracle.commit_reading(&o3, &project_id, &nonce, &commitment);

    // Slash o3 below min_stake AFTER it committed but BEFORE it reveals.
    oracle.slash(&admin, &o3, &600, &1);

    e.ledger().with_mut(|l| {
        l.timestamp += 301;
        l.sequence_number += 61;
    });
    oracle.begin_reveal_phase(&project_id);

    oracle.reveal_reading(&o1, &project_id, &params);
    oracle.reveal_reading(&o2, &project_id, &params);

    // o3's reveal must now fail the min_stake check even though it committed
    // validly before the slash.
    let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
        &oracle.address,
        &Symbol::new(&e, "reveal_reading"),
        vec![&e, o3.to_val(), project_id.to_val(), params.into_val(&e)],
    );
    assert!(
        result.is_err(),
        "slashed-below-min oracle must not be able to reveal"
    );

    // With only 2 valid reveals and min_oracles=3, the window is not
    // finalized — this documents the resulting stuck-window state.
    assert!(oracle.get_last_result(&project_id).is_none());
}

// ─────────────────────────────────────────────────────────────────────────
// Test 8 — Oracle paused mid-reveal-phase: committed readings' fate
// ─────────────────────────────────────────────────────────────────────────
// ASSUMPTION: verification_oracle exposes pause()/unpause(), analogous to
// credit_token's. If it doesn't compile, tell me what the actual pause
// entry point is (or whether the oracle has no pause concept at all —
// in which case this test should instead assert that governance's
// emergency_pause only affects registered TOKENS, not the oracle, and
// reveals continue normally through a paused-token scenario instead).
#[test]
fn test_oracle_paused_mid_reveal_blocks_further_reveals_but_keeps_commits() {
    let e = Env::default();
    e.mock_all_auths();
    e.ledger().with_mut(|l| l.timestamp = LEDGER_TIMESTAMP);

    let admin = Address::generate(&e);
    let project_owner = Address::generate(&e);
    let (oracle, token, project_id) = setup_oracle_and_token(&e, &admin, &project_owner, 0);
    let (o1, o2, o3) = add_three_oracles(&e, &admin, &oracle);

    let nonce = 1u64;
    let (params, salt) = reveal_params(&e, nonce);
    let commitment = sha256_commitment(
        &e,
        nonce,
        params.ph,
        params.turbidity,
        params.dissolved_oxygen,
        params.flow_rate,
        params.temperature,
        params.total_nitrogen,
        params.total_phosphorus,
        &salt,
    );

    oracle.open_window(&admin, &project_id);
    oracle.commit_reading(&o1, &project_id, &nonce, &commitment);
    oracle.commit_reading(&o2, &project_id, &nonce, &commitment);
    oracle.commit_reading(&o3, &project_id, &nonce, &commitment);

    e.ledger().with_mut(|l| {
        l.timestamp += 301;
        l.sequence_number += 61;
    });
    oracle.begin_reveal_phase(&project_id);

    // One valid reveal lands before pause.
    oracle.reveal_reading(&o1, &project_id, &params);

    oracle.pause(&admin);

    // Further reveals during the paused state must be rejected...
    let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
        &oracle.address,
        &Symbol::new(&e, "reveal_reading"),
        vec![
            &e,
            o2.to_val(),
            project_id.to_val(),
            params.clone().into_val(&e),
        ],
    );
    assert!(
        result.is_err(),
        "reveal must be rejected while oracle is paused"
    );

    // ...but the already-committed readings (o2, o3) are not discarded —
    // unpausing must let the round resume and finalize normally.
    oracle.unpause(&admin);
    oracle.reveal_reading(&o2, &project_id, &params);
    let result = oracle
        .reveal_reading(&o3, &project_id, &params)
        .expect("window finalizes once unpaused and third reveal lands");
    assert_eq!(result.oracle_count, 3);
    assert_eq!(token.total_supply(), 100);
}
