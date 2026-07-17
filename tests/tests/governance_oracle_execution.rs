//! Integration test: governance proposal executes a real `update_config` call
//! on `verification_oracle`, proving the cross-contract dispatch in
//! `Governance::execute` works end-to-end against a production contract (not
//! just the unit-test mock target).
//!
//! Authorization model: governance holds admin authority over a target
//! contract by being transferred that contract's `admin` address (see
//! `VerificationOracle::transfer_admin`). When governance's `execute()`
//! invokes the target via `e.invoke_contract`, the target's
//! `admin.require_auth()` check auto-authorizes because the authorizing
//! address equals the invoking contract's own address — no extra signature
//! is needed.

use governance::{Governance, GovernanceAction, GovernanceClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, Env, IntoVal, String, Symbol, Vec,
};
use verification_oracle::{OracleConfig, VerificationOracle, VerificationOracleClient};

#[test]
fn test_proposal_updates_oracle_config_via_cross_contract_execution() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let member1 = Address::generate(&e);
    let member2 = Address::generate(&e);
    let staking_token = Address::generate(&e);
    let treasury = Address::generate(&e);

    let oracle_id = e.register_contract(None, VerificationOracle);
    let oracle_client = VerificationOracleClient::new(&e, &oracle_id);
    oracle_client.initialize(&admin, &staking_token, &treasury);

    let gov_id = e.register_contract(None, Governance);
    let gov_client = GovernanceClient::new(&e, &gov_id);
    gov_client.initialize(
        &admin,
        &Vec::from_array(&e, [member1.clone(), member2.clone()]),
    );

    // Delegate oracle admin authority to the governance contract.
    oracle_client.transfer_admin(&admin, &gov_id);

    let old_config = oracle_client.get_config();
    assert_eq!(old_config.credit_per_kg_n, 10);

    let mut new_config = old_config.clone();
    new_config.credit_per_kg_n = 25;
    new_config.min_stake = 5000;

    let action = GovernanceAction {
        target: oracle_id.clone(),
        function: Symbol::new(&e, "update_config"),
        args: Vec::from_array(
            &e,
            [gov_id.clone().to_val(), new_config.clone().into_val(&e)],
        ),
    };
    let actions = Vec::from_array(&e, [action]);

    let proposal_id = gov_client.propose(
        &member1,
        &String::from_str(&e, "Raise nitrogen credit rate"),
        &String::from_str(&e, "Update oracle credit_per_kg_n from 10 to 25"),
        &actions,
    );

    gov_client.vote(&member1, &proposal_id, &true);
    gov_client.vote(&member2, &proposal_id, &true);

    let proposal = gov_client.get_proposal(&proposal_id).unwrap();
    let mut info = e.ledger().get();
    info.timestamp = proposal.timelock_ends_at + 1;
    e.ledger().set(info);

    gov_client.execute(&member1, &proposal_id);

    let updated: OracleConfig = oracle_client.get_config();
    assert_eq!(updated.credit_per_kg_n, 25);
    assert_eq!(updated.min_stake, 5000);

    let proposal = gov_client.get_proposal(&proposal_id).unwrap();
    assert!(matches!(
        proposal.status,
        governance::ProposalStatus::Executed
    ));
}
