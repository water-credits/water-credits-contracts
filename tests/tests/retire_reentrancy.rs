//! Adversarial coverage for `credit_token::retire` state ordering (issue #157).
//!
//! Soroban currently rejects contract re-entry at the host boundary. These
//! tests still model a hostile retirement registry so the token's accounting
//! and atomic rollback guarantees remain explicit if that trust boundary or
//! runtime behavior changes.

use credit_token::CreditTokenClient;
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, testutils::Address as _, vec, Address,
    BytesN, Env, IntoVal, InvokeError, String, Val,
};

const MOCK_RECORD_ID: u64 = 157;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryBehavior {
    CatchCallbacks,
    PropagateRetireCallback,
    Fail,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallbackOutcome {
    NotAttempted,
    Succeeded,
    Aborted,
    ContractError,
    ReturnConversionFailed,
    ErrorConversionFailed,
}

#[contracttype]
enum AttackKey {
    Recipient,
    Behavior,
    InAttack,
    AttackCount,
    RecordCount,
    FailureTouched,
    RetireSucceeded,
    TransferSucceeded,
    RetireOutcome,
    TransferOutcome,
}

#[contract]
struct MaliciousRegistry;

#[contractimpl]
impl MaliciousRegistry {
    pub fn initialize(e: Env, recipient: Address, behavior: RegistryBehavior) {
        e.storage()
            .instance()
            .set(&AttackKey::Recipient, &recipient);
        e.storage().instance().set(&AttackKey::Behavior, &behavior);
        e.storage().instance().set(&AttackKey::InAttack, &false);
        e.storage().instance().set(&AttackKey::AttackCount, &0u64);
        e.storage().instance().set(&AttackKey::RecordCount, &0u64);
        e.storage()
            .instance()
            .set(&AttackKey::FailureTouched, &false);
        e.storage()
            .instance()
            .set(&AttackKey::RetireSucceeded, &false);
        e.storage()
            .instance()
            .set(&AttackKey::TransferSucceeded, &false);
        e.storage()
            .instance()
            .set(&AttackKey::RetireOutcome, &CallbackOutcome::NotAttempted);
        e.storage()
            .instance()
            .set(&AttackKey::TransferOutcome, &CallbackOutcome::NotAttempted);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_retirement(
        e: Env,
        caller: Address,
        retiree: Address,
        _project_id: BytesN<32>,
        amount: i128,
        purpose: String,
        metadata_uri: String,
    ) -> u64 {
        let behavior: RegistryBehavior = e.storage().instance().get(&AttackKey::Behavior).unwrap();
        if behavior == RegistryBehavior::Fail {
            // This write must be reverted together with the token's pre-call
            // accounting when the registry frame fails.
            e.storage()
                .instance()
                .set(&AttackKey::FailureTouched, &true);
            panic!("registry failure");
        }

        let in_attack: bool = e
            .storage()
            .instance()
            .get(&AttackKey::InAttack)
            .unwrap_or(false);

        if !in_attack {
            e.storage().instance().set(&AttackKey::InAttack, &true);
            let attack_count: u64 = e
                .storage()
                .instance()
                .get(&AttackKey::AttackCount)
                .unwrap_or(0);
            e.storage()
                .instance()
                .set(&AttackKey::AttackCount, &(attack_count + 1));

            let retire_args = vec![
                &e,
                retiree.clone().into_val(&e),
                amount.into_val(&e),
                purpose.into_val(&e),
                metadata_uri.into_val(&e),
            ];
            if behavior == RegistryBehavior::PropagateRetireCallback {
                e.invoke_contract::<Val>(&caller, &symbol_short!("retire"), retire_args);
                unreachable!("a propagated reentrant callback must abort");
            }

            let retire_result = e.try_invoke_contract::<Val, InvokeError>(
                &caller,
                &symbol_short!("retire"),
                retire_args,
            );
            let retire_outcome = match retire_result {
                Ok(Ok(_)) => CallbackOutcome::Succeeded,
                Ok(Err(_)) => CallbackOutcome::ReturnConversionFailed,
                Err(Ok(InvokeError::Abort)) => CallbackOutcome::Aborted,
                Err(Ok(InvokeError::Contract(_))) => CallbackOutcome::ContractError,
                Err(Err(_)) => CallbackOutcome::ErrorConversionFailed,
            };
            let retire_succeeded = retire_outcome == CallbackOutcome::Succeeded;
            let prior_retire_success: bool = e
                .storage()
                .instance()
                .get(&AttackKey::RetireSucceeded)
                .unwrap_or(false);
            e.storage().instance().set(
                &AttackKey::RetireSucceeded,
                &(prior_retire_success || retire_succeeded),
            );
            e.storage()
                .instance()
                .set(&AttackKey::RetireOutcome, &retire_outcome);

            let recipient: Address = e.storage().instance().get(&AttackKey::Recipient).unwrap();
            let transfer_result = e.try_invoke_contract::<Val, InvokeError>(
                &caller,
                &symbol_short!("transfer"),
                vec![
                    &e,
                    retiree.into_val(&e),
                    recipient.into_val(&e),
                    amount.into_val(&e),
                ],
            );
            let transfer_outcome = match transfer_result {
                Ok(Ok(_)) => CallbackOutcome::Succeeded,
                Ok(Err(_)) => CallbackOutcome::ReturnConversionFailed,
                Err(Ok(InvokeError::Abort)) => CallbackOutcome::Aborted,
                Err(Ok(InvokeError::Contract(_))) => CallbackOutcome::ContractError,
                Err(Err(_)) => CallbackOutcome::ErrorConversionFailed,
            };
            let transfer_succeeded = transfer_outcome == CallbackOutcome::Succeeded;
            let prior_transfer_success: bool = e
                .storage()
                .instance()
                .get(&AttackKey::TransferSucceeded)
                .unwrap_or(false);
            e.storage().instance().set(
                &AttackKey::TransferSucceeded,
                &(prior_transfer_success || transfer_succeeded),
            );
            e.storage()
                .instance()
                .set(&AttackKey::TransferOutcome, &transfer_outcome);
            e.storage().instance().set(&AttackKey::InAttack, &false);
        }

        let record_count: u64 = e
            .storage()
            .instance()
            .get(&AttackKey::RecordCount)
            .unwrap_or(0);
        e.storage()
            .instance()
            .set(&AttackKey::RecordCount, &(record_count + 1));
        MOCK_RECORD_ID + record_count
    }

    pub fn attempted(e: Env) -> bool {
        Self::attack_count(e) > 0
    }

    pub fn attack_count(e: Env) -> u64 {
        e.storage()
            .instance()
            .get(&AttackKey::AttackCount)
            .unwrap_or(0)
    }

    pub fn record_count(e: Env) -> u64 {
        e.storage()
            .instance()
            .get(&AttackKey::RecordCount)
            .unwrap_or(0)
    }

    pub fn failure_touched(e: Env) -> bool {
        e.storage()
            .instance()
            .get(&AttackKey::FailureTouched)
            .unwrap_or(false)
    }

    pub fn retire_succeeded(e: Env) -> bool {
        e.storage()
            .instance()
            .get(&AttackKey::RetireSucceeded)
            .unwrap_or(false)
    }

    pub fn transfer_succeeded(e: Env) -> bool {
        e.storage()
            .instance()
            .get(&AttackKey::TransferSucceeded)
            .unwrap_or(false)
    }

    pub fn retire_outcome(e: Env) -> CallbackOutcome {
        e.storage()
            .instance()
            .get(&AttackKey::RetireOutcome)
            .unwrap_or(CallbackOutcome::NotAttempted)
    }

    pub fn transfer_outcome(e: Env) -> CallbackOutcome {
        e.storage()
            .instance()
            .get(&AttackKey::TransferOutcome)
            .unwrap_or(CallbackOutcome::NotAttempted)
    }
}

mod malformed_registry {
    use super::*;

    #[contracttype]
    enum MalformedKey {
        Touched,
    }

    #[contract]
    pub struct MalformedRegistry;

    #[contractimpl]
    impl MalformedRegistry {
        #[allow(clippy::too_many_arguments)]
        pub fn record_retirement(
            e: Env,
            _caller: Address,
            _retiree: Address,
            _project_id: BytesN<32>,
            _amount: i128,
            _purpose: String,
            _metadata_uri: String,
        ) -> soroban_sdk::Symbol {
            e.storage().instance().set(&MalformedKey::Touched, &true);
            symbol_short!("bad_id")
        }

        pub fn touched(e: Env) -> bool {
            e.storage()
                .instance()
                .get(&MalformedKey::Touched)
                .unwrap_or(false)
        }
    }
}

use malformed_registry::{MalformedRegistry, MalformedRegistryClient};

fn deploy_token(e: &Env, admin: &Address, project_id: &BytesN<32>) -> CreditTokenClient<'static> {
    let wasm = std::fs::read(env!("CREDIT_TOKEN_WASM"))
        .expect("credit_token.wasm should have been built by tests/build.rs");
    let token_id = e.register_contract_wasm(None, wasm.as_slice());
    let token = CreditTokenClient::new(e, &token_id);
    token.initialize(
        admin,
        &String::from_str(e, "Reentrancy Test Credit"),
        &String::from_str(e, "RTC"),
        project_id,
        &String::from_str(e, "issue-157"),
    );
    token
}

#[test]
fn malicious_registry_cannot_double_spend_committed_retirement() {
    let e = Env::default();
    e.mock_all_auths();
    e.budget().reset_unlimited();

    let admin = Address::generate(&e);
    let holder = Address::generate(&e);
    let attacker = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[157u8; 32]);
    let token = deploy_token(&e, &admin, &project_id);

    let registry_id = e.register_contract(None, MaliciousRegistry);
    let registry = MaliciousRegistryClient::new(&e, &registry_id);
    registry.initialize(&attacker, &RegistryBehavior::CatchCallbacks);
    token.set_retirement_registry(&admin, &registry_id);

    let amount = 500i128;
    token.mint_to(&admin, &holder, &amount);

    let certificate = token.retire(
        &holder,
        &amount,
        &String::from_str(&e, "adversarial retirement"),
        &String::from_str(&e, "ipfs://issue-157"),
    );

    assert!(registry.attempted());
    assert!(!registry.retire_succeeded());
    assert!(!registry.transfer_succeeded());
    assert_eq!(registry.retire_outcome(), CallbackOutcome::Aborted);
    assert_eq!(registry.transfer_outcome(), CallbackOutcome::Aborted);
    assert_eq!(registry.attack_count(), 1);
    assert_eq!(registry.record_count(), 1);
    assert_eq!(certificate.registry_record_id, Some(MOCK_RECORD_ID));

    assert_eq!(token.balance(&holder), 0);
    assert_eq!(token.balance(&attacker), 0);
    assert_eq!(token.total_supply(), 0);
    assert_eq!(token.total_retired(), amount);
    assert_eq!(token.get_certificate(&0), Some(certificate));
    assert!(token.get_certificate(&1).is_none());
}

#[test]
fn registry_failure_rolls_back_precommitted_accounting() {
    let e = Env::default();
    e.mock_all_auths();
    e.budget().reset_unlimited();

    let admin = Address::generate(&e);
    let holder = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[158u8; 32]);
    let token = deploy_token(&e, &admin, &project_id);

    let registry_id = e.register_contract(None, MaliciousRegistry);
    let registry = MaliciousRegistryClient::new(&e, &registry_id);
    registry.initialize(&Address::generate(&e), &RegistryBehavior::Fail);
    token.set_retirement_registry(&admin, &registry_id);

    let initial_balance = 700i128;
    let retirement_amount = 400i128;
    token.mint_to(&admin, &holder, &initial_balance);

    let result = token.try_retire(
        &holder,
        &retirement_amount,
        &String::from_str(&e, "rollback retirement"),
        &String::from_str(&e, "ipfs://issue-157-rollback"),
    );

    assert!(result.is_err());
    assert_eq!(token.balance(&holder), initial_balance);
    assert_eq!(token.total_supply(), initial_balance);
    assert_eq!(token.total_retired(), 0);
    assert!(token.get_certificate(&0).is_none());
    assert_eq!(registry.record_count(), 0);
    assert!(!registry.failure_touched());
}

#[test]
fn propagated_reentrant_callback_rolls_back_the_entire_retirement() {
    let e = Env::default();
    e.mock_all_auths();
    e.budget().reset_unlimited();

    let admin = Address::generate(&e);
    let holder = Address::generate(&e);
    let attacker = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[160u8; 32]);
    let token = deploy_token(&e, &admin, &project_id);

    let registry_id = e.register_contract(None, MaliciousRegistry);
    let registry = MaliciousRegistryClient::new(&e, &registry_id);
    registry.initialize(&attacker, &RegistryBehavior::PropagateRetireCallback);
    token.set_retirement_registry(&admin, &registry_id);

    let amount = 500i128;
    token.mint_to(&admin, &holder, &amount);

    let result = token.try_retire(
        &holder,
        &amount,
        &String::from_str(&e, "propagated callback"),
        &String::from_str(&e, "ipfs://issue-157-propagated"),
    );

    assert!(result.is_err());
    assert_eq!(token.balance(&holder), amount);
    assert_eq!(token.balance(&attacker), 0);
    assert_eq!(token.total_supply(), amount);
    assert_eq!(token.total_retired(), 0);
    assert!(token.get_certificate(&0).is_none());

    // The registry's writes made immediately before the hard callback are
    // reverted together with the token's pre-call accounting.
    assert_eq!(registry.attack_count(), 0);
    assert_eq!(registry.record_count(), 0);
    assert_eq!(registry.retire_outcome(), CallbackOutcome::NotAttempted);
}

#[test]
fn malformed_registry_record_id_rolls_back_both_contracts() {
    let e = Env::default();
    e.mock_all_auths();
    e.budget().reset_unlimited();

    let admin = Address::generate(&e);
    let holder = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[161u8; 32]);
    let token = deploy_token(&e, &admin, &project_id);

    let registry_id = e.register_contract(None, MalformedRegistry);
    let registry = MalformedRegistryClient::new(&e, &registry_id);
    token.set_retirement_registry(&admin, &registry_id);

    let initial_balance = 700i128;
    let amount = 400i128;
    token.mint_to(&admin, &holder, &initial_balance);

    let result = token.try_retire(
        &holder,
        &amount,
        &String::from_str(&e, "malformed record id"),
        &String::from_str(&e, "ipfs://issue-157-malformed-id"),
    );

    assert!(result.is_err());
    assert_eq!(token.balance(&holder), initial_balance);
    assert_eq!(token.total_supply(), initial_balance);
    assert_eq!(token.total_retired(), 0);
    assert!(token.get_certificate(&0).is_none());
    assert!(!registry.touched());
}

#[test]
fn repeated_adversarial_retirements_preserve_balances_supply_and_record_ids() {
    let e = Env::default();
    e.mock_all_auths();
    e.budget().reset_unlimited();

    let admin = Address::generate(&e);
    let first_holder = Address::generate(&e);
    let second_holder = Address::generate(&e);
    let attacker = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[159u8; 32]);
    let token = deploy_token(&e, &admin, &project_id);

    let registry_id = e.register_contract(None, MaliciousRegistry);
    let registry = MaliciousRegistryClient::new(&e, &registry_id);
    registry.initialize(&attacker, &RegistryBehavior::CatchCallbacks);
    token.set_retirement_registry(&admin, &registry_id);

    token.mint_to(&admin, &first_holder, &900);
    token.mint_to(&admin, &second_holder, &700);

    let first = token.retire(
        &first_holder,
        &600,
        &String::from_str(&e, "first adversarial retirement"),
        &String::from_str(&e, "ipfs://issue-157-first"),
    );
    let second = token.retire(
        &second_holder,
        &500,
        &String::from_str(&e, "second adversarial retirement"),
        &String::from_str(&e, "ipfs://issue-157-second"),
    );
    let third = token.retire(
        &first_holder,
        &200,
        &String::from_str(&e, "third adversarial retirement"),
        &String::from_str(&e, "ipfs://issue-157-third"),
    );

    assert!(!registry.retire_succeeded());
    assert!(!registry.transfer_succeeded());
    assert_eq!(registry.retire_outcome(), CallbackOutcome::Aborted);
    assert_eq!(registry.transfer_outcome(), CallbackOutcome::Aborted);
    assert_eq!(registry.attack_count(), 3);
    assert_eq!(registry.record_count(), 3);
    assert_eq!(token.balance(&first_holder), 100);
    assert_eq!(token.balance(&second_holder), 200);
    assert_eq!(token.balance(&attacker), 0);
    assert_eq!(token.total_supply(), 300);
    assert_eq!(token.total_retired(), 1_300);
    assert_eq!(token.total_supply() + token.total_retired(), 1_600);

    assert_eq!(first.registry_record_id, Some(MOCK_RECORD_ID));
    assert_eq!(second.registry_record_id, Some(MOCK_RECORD_ID + 1));
    assert_eq!(third.registry_record_id, Some(MOCK_RECORD_ID + 2));
    assert_eq!(token.get_certificate(&0), Some(first));
    assert_eq!(token.get_certificate(&1), Some(second));
    assert_eq!(token.get_certificate(&2), Some(third));
    assert!(token.get_certificate(&3).is_none());
}
