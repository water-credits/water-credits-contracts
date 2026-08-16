//! End-to-end coverage for governance emergency pause propagation (issue #92).
//!
//! Both contracts run from their compiled WASM artifacts. The test wires the
//! governance contract as the token pause guardian, executes pause and unpause
//! proposals, and verifies token behavior on both sides of the pause.

use credit_token::CreditTokenClient;
use governance::{GovernanceAction, GovernanceClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, BytesN, Env, String, Symbol, Vec,
};

fn deploy_wasm(e: &Env, path: &str) -> Address {
    let wasm = std::fs::read(path).expect("contract WASM should be built by tests/build.rs");
    e.register_contract_wasm(None, wasm.as_slice())
}

fn execute_protocol_action(
    e: &Env,
    governance: &GovernanceClient,
    governance_id: &Address,
    member: &Address,
    function: &str,
) {
    let action = GovernanceAction {
        target: governance_id.clone(),
        function: Symbol::new(e, function),
        args: Vec::new(e),
    };
    let proposal_id = governance.propose(
        member,
        &String::from_str(e, function),
        &String::from_str(e, "Exercise the governance emergency control"),
        &Vec::from_array(e, [action]),
    );
    governance.vote(member, &proposal_id, &true);

    let proposal = governance.get_proposal(&proposal_id).unwrap();
    let mut ledger = e.ledger().get();
    ledger.timestamp = proposal.timelock_ends_at + 1;
    e.ledger().set(ledger);

    governance.execute(member, &proposal_id);
}

#[test]
fn governance_pause_and_unpause_control_real_token_wasm() {
    let e = Env::default();
    e.mock_all_auths();
    e.budget().reset_unlimited();

    let admin = Address::generate(&e);
    let member = Address::generate(&e);
    let beneficiary = Address::generate(&e);

    let governance_id = deploy_wasm(&e, env!("GOVERNANCE_WASM"));
    let governance = GovernanceClient::new(&e, &governance_id);
    governance.initialize(&admin, &Vec::from_array(&e, [member.clone()]));

    let token_id = deploy_wasm(&e, env!("CREDIT_TOKEN_WASM"));
    let token = CreditTokenClient::new(&e, &token_id);
    token.initialize(
        &admin,
        &String::from_str(&e, "Governed Water Credit"),
        &String::from_str(&e, "GWC"),
        &BytesN::from_array(&e, &[92u8; 32]),
        &String::from_str(&e, "Wetland_Restoration_v2.1"),
    );

    token.set_pause_guardian(&admin, &governance_id);
    governance.register_token(&admin, &token_id);
    assert_eq!(token.pause_guardian(), Some(governance_id.clone()));
    assert_eq!(
        governance.list_registered_tokens(),
        Vec::from_array(&e, [token_id])
    );

    execute_protocol_action(&e, &governance, &governance_id, &member, "emergency_pause");

    assert!(governance.is_protocol_paused());
    assert!(token.paused());
    assert!(token.try_mint_to(&admin, &beneficiary, &100).is_err());
    assert_eq!(token.balance(&beneficiary), 0);

    execute_protocol_action(
        &e,
        &governance,
        &governance_id,
        &member,
        "emergency_unpause",
    );

    assert!(!governance.is_protocol_paused());
    assert!(!token.paused());
    token.mint_to(&admin, &beneficiary, &100);
    assert_eq!(token.balance(&beneficiary), 100);
}
