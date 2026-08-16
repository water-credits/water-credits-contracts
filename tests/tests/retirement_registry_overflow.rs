//! Overflow coverage for `retirement_registry::record_retirement` (issue #91).
//!
//! These tests use a separately authorized caller to reproduce the trust
//! boundary where the registry cannot assume the amount was validated by
//! `credit_token::retire`.

use retirement_registry::{RetirementRegistry, RetirementRegistryClient};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String};

fn setup() -> (Env, Address, RetirementRegistryClient<'static>) {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let contract_id = e.register_contract(None, RetirementRegistry);
    let client = RetirementRegistryClient::new(&e, &contract_id);
    client.initialize(&admin);

    (e, admin, client)
}

#[test]
fn fuzz_near_i128_max_retirements_never_wrap_total() {
    // Deterministic boundary fuzz cases cover increasingly large gaps below
    // i128::MAX while keeping each initial retirement valid.
    for remaining in [0i128, 1, 1_024, i64::MAX as i128] {
        let (e, admin, client) = setup();
        let authorized_caller = Address::generate(&e);
        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[91u8; 32]);
        let purpose = String::from_str(&e, "overflow boundary audit");
        let uri = String::from_str(&e, "ipfs://retirement-overflow-audit");

        client.set_authorized_caller(&admin, &authorized_caller, &true);

        let initial_amount = i128::MAX - remaining;
        client.record_retirement(
            &authorized_caller,
            &retiree,
            &project_id,
            &initial_amount,
            &purpose,
            &uri,
        );

        let result = client.try_record_retirement(
            &authorized_caller,
            &retiree,
            &project_id,
            &(remaining + 1),
            &purpose,
            &uri,
        );

        assert!(result.is_err());
        assert_eq!(client.total_retired(), initial_amount);
        assert_eq!(client.record_count(), 1);
        assert!(client.get_record(&2).is_none());
    }
}

#[test]
#[should_panic(expected = "total_retired overflow")]
fn total_retired_overflow_has_descriptive_panic() {
    let (e, admin, client) = setup();
    let authorized_caller = Address::generate(&e);
    let retiree = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[92u8; 32]);
    let purpose = String::from_str(&e, "overflow message audit");
    let uri = String::from_str(&e, "ipfs://retirement-overflow-message");

    client.set_authorized_caller(&admin, &authorized_caller, &true);
    client.record_retirement(
        &authorized_caller,
        &retiree,
        &project_id,
        &i128::MAX,
        &purpose,
        &uri,
    );
    client.record_retirement(
        &authorized_caller,
        &retiree,
        &project_id,
        &1,
        &purpose,
        &uri,
    );
}
