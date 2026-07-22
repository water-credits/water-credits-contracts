#![no_std]
#![allow(clippy::too_many_arguments)]
use shared::{generate_project_id, is_valid_status, is_valid_status_transition};
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, vec, Address, BytesN, Env, String, Symbol,
    Val, Vec,
};

#[cfg(test)]
extern crate std;

// ── Events ──
const EVENT_PROJ_REG: Symbol = symbol_short!("proj_reg");
const EVENT_INITIALIZED: Symbol = symbol_short!("init");
const EVENT_STATUS_CHANGED: Symbol = symbol_short!("stat_chg");
const EVENT_OWNER_CHANGED: Symbol = symbol_short!("ownr_chg");
const EVENT_ADMIN_PROPOSED: Symbol = symbol_short!("adm_prop");
const EVENT_ADMIN_ACCEPTED: Symbol = symbol_short!("adm_acpt");
const EVENT_ADMIN_CANCELLED: Symbol = symbol_short!("adm_canc");

// ── Data Types ──

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectInfo {
    pub id: BytesN<32>,
    pub name: String,
    pub latitude: i64,
    pub longitude: i64,
    pub methodology: String,
    pub owner: Address,
    pub status: String,
    pub credit_token: Address,
    pub registration_date: u64,
    pub area_hectares: u64,
}

/// Storage key enum.
///
/// Instance:   Admin, ProjectCount
/// Persistent: Project(BytesN<32>)
#[contracttype]
pub enum DataKey {
    // ── Instance ──
    Admin,
    /// Pending new admin address for propose-then-accept transfer.
    PendingAdmin,
    /// Ledger sequence at which a pending admin transfer expires.
    AdminTransferExpiration,
    ProjectCount,
    // ── Persistent ──
    Project(BytesN<32>),
}

// ── TTL constants ──
/// Projects are permanent registrations: 10 years.
const PROJECT_TTL_THRESHOLD: u32 = 63_072_000;
const PROJECT_TTL_BUMP: u32 = 63_072_000;
/// Admin transfer proposal TTL: ~30 days (518,400 ledgers at 5 sec/ledger).
const ADMIN_TRANSFER_TTL: u32 = 518_400;

fn has_admin(e: &Env) -> bool {
    e.storage().instance().has(&DataKey::Admin)
}

fn read_admin(e: &Env) -> Address {
    e.storage().instance().get(&DataKey::Admin).unwrap()
}

#[contract]
pub struct CreditFactory;

#[contractimpl]
#[allow(clippy::too_many_arguments)]
impl CreditFactory {
    /// Initialize the factory with an admin address. Callable once.
    pub fn initialize(e: Env, admin: Address) {
        if has_admin(&e) {
            panic!("already initialized");
        }
        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::ProjectCount, &0u64);

        e.events().publish((EVENT_INITIALIZED,), (admin,));
    }

    /// Return the current admin address.
    pub fn admin(e: Env) -> Address {
        read_admin(&e)
    }

    /// Register a new water restoration project. Deploys a new credit_token contract and returns a SHA-256 project ID.
    pub fn register_project(
        e: Env,
        admin: Address,
        name: String,
        latitude: i64,
        longitude: i64,
        methodology: String,
        owner: Address,
        area_hectares: u64,
        credit_token_wasm_hash: BytesN<32>,
    ) -> BytesN<32> {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }

        if name.len() == 0 {
            panic!("name must not be empty");
        }
        if !(-90000000..=90000000).contains(&latitude) {
            panic!("invalid latitude");
        }
        if !(-180000000..=180000000).contains(&longitude) {
            panic!("invalid longitude");
        }
        if area_hectares == 0 {
            panic!("area must be positive");
        }

        let count: u64 = e.storage().instance().get(&DataKey::ProjectCount).unwrap();
        let timestamp = e.ledger().timestamp();
        let project_id: BytesN<32> = generate_project_id(
            &e,
            count,
            timestamp,
            &name,
            &methodology,
            latitude,
            longitude,
            area_hectares,
        );

        if e.storage()
            .persistent()
            .has(&DataKey::Project(project_id.clone()))
        {
            panic!("project id collision");
        }

        // Deploy new credit_token contract
        let salt = project_id.clone();
        let token_address: Address = e
            .deployer()
            .with_current_contract(salt)
            .deploy(credit_token_wasm_hash);

        // Prepare initialize args and call the token
        let token_symbol = String::from_str(&e, "WC");
        let init_args: Vec<Val> = vec![
            &e,
            admin.clone().to_val(),
            name.clone().to_val(),
            token_symbol.to_val(),
            project_id.clone().to_val(),
            methodology.clone().to_val(),
        ];
        e.invoke_contract::<()>(&token_address, &Symbol::new(&e, "initialize"), init_args);

        let project = ProjectInfo {
            id: project_id.clone(),
            name,
            latitude,
            longitude,
            methodology,
            owner,
            status: String::from_str(&e, "registered"),
            credit_token: token_address,
            registration_date: timestamp,
            area_hectares,
        };

        let proj_key = DataKey::Project(project_id.clone());
        e.storage().persistent().set(&proj_key, &project);
        e.storage()
            .persistent()
            .extend_ttl(&proj_key, PROJECT_TTL_THRESHOLD, PROJECT_TTL_BUMP);
        e.storage()
            .instance()
            .set(&DataKey::ProjectCount, &(count + 1));

        e.events().publish((EVENT_PROJ_REG,), (project_id.clone(),));

        project_id
    }

    /// Get project info by its unique ID. Returns None if not found.
    pub fn get_project(e: Env, project_id: BytesN<32>) -> Option<ProjectInfo> {
        let key = DataKey::Project(project_id);
        let result: Option<ProjectInfo> = e.storage().persistent().get(&key);
        if result.is_some() {
            e.storage()
                .persistent()
                .extend_ttl(&key, PROJECT_TTL_THRESHOLD, PROJECT_TTL_BUMP);
        }
        result
    }

    /// Update a project's status. Valid statuses: registered, active, completed, suspended.
    ///
    /// Enforces a strict state machine:
    ///   registered → active, active → completed, active → suspended,
    ///   completed → active, suspended → registered.
    ///
    /// Same-status updates are no-ops (returns without writing).
    pub fn update_project_status(e: Env, admin: Address, project_id: BytesN<32>, status: String) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }

        if !is_valid_status(&e, &status) {
            panic!("invalid status");
        }

        let key = DataKey::Project(project_id.clone());
        let mut project: ProjectInfo = e
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("project not found"));

        // Same-status no-op: return early without touching storage.
        if project.status == status {
            return;
        }

        if !is_valid_status_transition(&e, &project.status, &status) {
            panic!("invalid status transition");
        }

        let old_status = project.status.clone();
        project.status = status.clone();
        e.storage().persistent().set(&key, &project);
        e.storage()
            .persistent()
            .extend_ttl(&key, PROJECT_TTL_THRESHOLD, PROJECT_TTL_BUMP);

        e.events()
            .publish((EVENT_STATUS_CHANGED,), (project_id, old_status, status));
    }

    /// Return the total number of registered projects.
    pub fn project_count(e: Env) -> u64 {
        e.storage().instance().get(&DataKey::ProjectCount).unwrap()
    }

    /// Transfer project ownership to a new wallet address.
    /// Can be called by admin or the current project owner.
    pub fn update_project_owner(
        e: Env,
        caller: Address,
        project_id: BytesN<32>,
        new_owner: Address,
    ) {
        caller.require_auth();
        let admin = read_admin(&e);
        let key = DataKey::Project(project_id.clone());
        let mut project: ProjectInfo = e
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("project not found"));

        if caller != admin && caller != project.owner {
            panic!("unauthorized");
        }

        let old_owner = project.owner.clone();
        project.owner = new_owner.clone();
        e.storage().persistent().set(&key, &project);
        e.storage()
            .persistent()
            .extend_ttl(&key, PROJECT_TTL_THRESHOLD, PROJECT_TTL_BUMP);

        e.events()
            .publish((EVENT_OWNER_CHANGED,), (project_id, old_owner, new_owner));
    }

    // ── Admin Transfer (Propose-Then-Accept) ──

    /// Propose transferring admin to a new address. The new admin must call `accept_admin()`
    /// within the TTL window (~30 days). Replaces any pending proposal.
    pub fn propose_transfer_admin(e: Env, admin: Address, new_admin: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        if new_admin == admin {
            panic!("new admin must differ");
        }

        let expiration = e.ledger().sequence().checked_add(ADMIN_TRANSFER_TTL).expect("overflow");
        e.storage().instance().set(&DataKey::PendingAdmin, &new_admin);
        e.storage().instance().set(&DataKey::AdminTransferExpiration, &expiration);

        e.events().publish((EVENT_ADMIN_PROPOSED,), (admin, new_admin, expiration));
    }

    /// Accept a pending admin transfer. Callable only by the proposed new admin.
    /// Atomically switches the admin if the proposal has not expired.
    pub fn accept_admin(e: Env, pending: Address) {
        pending.require_auth();
        let proposed: Address = e.storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .unwrap_or_else(|| panic!("no pending transfer"));

        if pending != proposed {
            panic!("unauthorized");
        }

        let expiration: u32 = e.storage()
            .instance()
            .get(&DataKey::AdminTransferExpiration)
            .unwrap_or(0);
        if expiration > 0 && e.ledger().sequence() >= expiration {
            panic!("transfer expired");
        }

        let old_admin = read_admin(&e);
        e.storage().instance().set(&DataKey::Admin, &pending);
        e.storage().instance().remove(&DataKey::PendingAdmin);
        e.storage().instance().remove(&DataKey::AdminTransferExpiration);

        e.events().publish((EVENT_ADMIN_ACCEPTED,), (old_admin, pending));
    }

    /// Cancel a pending admin transfer. Only the current admin can cancel.
    pub fn cancel_transfer_admin(e: Env, admin: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        let pending: Option<Address> = e.storage().instance().get(&DataKey::PendingAdmin);
        if pending.is_none() {
            panic!("no pending transfer");
        }

        let old_pending = pending.unwrap();
        e.storage().instance().remove(&DataKey::PendingAdmin);
        e.storage().instance().remove(&DataKey::AdminTransferExpiration);

        e.events().publish((EVENT_ADMIN_CANCELLED,), (admin, old_pending));
    }

    /// Return the pending admin address, if any.
    pub fn pending_admin(e: Env) -> Option<Address> {
        e.storage().instance().get(&DataKey::PendingAdmin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{Address, Bytes, Env, TryFromVal};

    fn setup_with_client() -> (
        Env,
        Address,
        Address,
        BytesN<32>,
        CreditFactoryClient<'static>,
    ) {
        let e = Env::default();
        let admin = Address::generate(&e);
        let owner = Address::generate(&e);
        let wasm =
            include_bytes!("../../../target/wasm32-unknown-unknown/release/credit_token.wasm");
        let wasm_hash = e
            .deployer()
            .upload_contract_wasm(Bytes::from_slice(&e, wasm));
        let contract_id = e.register_contract(None, CreditFactory);
        let client = CreditFactoryClient::new(&e, &contract_id);

        client.initialize(&admin);

        (e, admin, owner, wasm_hash, client)
    }

    // ── Admin Transfer Tests ──

    #[test]
    fn test_propose_and_accept_admin() {
        let (e, admin, _owner, _wasm_hash, client) = setup_with_client();
        let new_admin = Address::generate(&e);
        e.mock_all_auths();

        client.propose_transfer_admin(&admin, &new_admin);
        assert_eq!(client.pending_admin(), Some(new_admin.clone()));

        client.accept_admin(&new_admin);
        // New admin is now the admin
        assert_eq!(client.admin(), new_admin);
        // Pending state cleared
        assert!(client.pending_admin().is_none());
    }

    #[test]
    fn test_propose_transfer_admin_emits_event() {
        let (e, admin, _owner, _wasm_hash, client) = setup_with_client();
        let new_admin = Address::generate(&e);
        e.mock_all_auths();

        client.propose_transfer_admin(&admin, &new_admin);

        let events = e.events().all();
        // initialize(1) + propose_transfer_admin(1) = 2
        assert_eq!(events.len(), 2);
        let (_contract, topics, _data) = &events.get(1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("adm_prop"));
    }

    #[test]
    fn test_accept_admin_emits_event() {
        let (e, admin, _owner, _wasm_hash, client) = setup_with_client();
        let new_admin = Address::generate(&e);
        e.mock_all_auths();

        client.propose_transfer_admin(&admin, &new_admin);
        client.accept_admin(&new_admin);

        let events = e.events().all();
        // initialize(1) + propose(1) + accept(1) = 3
        assert_eq!(events.len(), 3);
        let (_contract, topics, _data) = &events.get(2).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("adm_acpt"));
    }

    #[test]
    fn test_cancel_transfer_admin() {
        let (e, admin, _owner, _wasm_hash, client) = setup_with_client();
        let new_admin = Address::generate(&e);
        e.mock_all_auths();

        client.propose_transfer_admin(&admin, &new_admin);
        assert_eq!(client.pending_admin(), Some(new_admin.clone()));

        client.cancel_transfer_admin(&admin);
        assert!(client.pending_admin().is_none());
    }

    #[test]
    fn test_cancel_transfer_admin_emits_event() {
        let (e, admin, _owner, _wasm_hash, client) = setup_with_client();
        let new_admin = Address::generate(&e);
        e.mock_all_auths();

        client.propose_transfer_admin(&admin, &new_admin);
        client.cancel_transfer_admin(&admin);

        let events = e.events().all();
        // initialize(1) + propose(1) + cancel(1) = 3
        assert_eq!(events.len(), 3);
        let (_contract, topics, _data) = &events.get(2).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("adm_canc"));
    }

    #[test]
    #[should_panic(expected = "no pending transfer")]
    fn test_accept_admin_without_proposal_panics() {
        let (e, _admin, _owner, _wasm_hash, client) = setup_with_client();
        let someone = Address::generate(&e);
        e.mock_all_auths();

        client.accept_admin(&someone);
    }

    #[test]
    #[should_panic(expected = "unauthorized")]
    fn test_accept_admin_wrong_caller_panics() {
        let (e, admin, _owner, _wasm_hash, client) = setup_with_client();
        let new_admin = Address::generate(&e);
        let impostor = Address::generate(&e);
        e.mock_all_auths();

        client.propose_transfer_admin(&admin, &new_admin);
        client.accept_admin(&impostor);
    }

    #[test]
    fn test_initialize_sets_admin() {
        let e = Env::default();
        let admin = Address::generate(&e);
        let contract_id = e.register_contract(None, CreditFactory);
        let client = CreditFactoryClient::new(&e, &contract_id);

        client.initialize(&admin);

        assert_eq!(client.admin(), admin);
        assert_eq!(client.project_count(), 0);
    }

    #[test]
    fn test_register_project_succeeds() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let name = String::from_str(&e, "Green Valley Wetland");
        let methodology = String::from_str(&e, "Wetland_Restoration_v2.1");

        let project_id = client.register_project(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &500,
            &wasm_hash,
        );

        let project = client.get_project(&project_id).unwrap();
        assert_eq!(project.name, name);
        assert_eq!(project.latitude, 38897700);
        assert_eq!(project.longitude, -77036500);
        assert_eq!(project.methodology, methodology);
        assert_eq!(project.owner, owner);
        assert_eq!(project.status, String::from_str(&e, "registered"));
        assert_eq!(project.area_hectares, 500);
        assert_eq!(client.project_count(), 1);
    }

    #[test]
    fn test_register_project_increments_count() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let name1 = String::from_str(&e, "Green Valley Wetland");
        let name2 = String::from_str(&e, "Blue River Restoration");
        let methodology = String::from_str(&e, "Wetland_Restoration_v2.1");

        let _id1 = client.register_project(
            &admin,
            &name1,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &500,
            &wasm_hash,
        );
        assert_eq!(client.project_count(), 1);

        let _id2 = client.register_project(
            &admin,
            &name2,
            &38900000,
            &(-77040000),
            &methodology,
            &owner,
            &300,
            &wasm_hash,
        );
        assert_eq!(client.project_count(), 2);
    }

    #[test]
    fn test_register_project_unique_ids() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let name = String::from_str(&e, "Test Project");
        let methodology = String::from_str(&e, "Test_v1");

        let id1 = client.register_project(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &500,
            &wasm_hash,
        );
        let id2 = client.register_project(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &500,
            &wasm_hash,
        );

        assert_ne!(id1, id2);
    }

    #[test]
    fn test_register_project_emits_event() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let name = String::from_str(&e, "Event Test");
        let methodology = String::from_str(&e, "Test_v1");
        let _project_id = client.register_project(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &100,
            &wasm_hash,
        );

        let events = e.events().all();
        // initialize(1) + register_project(1) = 2
        assert_eq!(events.len(), 2);
        let (_contract, topics, _data) = &events.get(1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("proj_reg"));
    }

    #[test]
    fn test_get_project_nonexistent_returns_none() {
        let (e, _admin, _owner, _wasm_hash, client) = setup_with_client();
        let fake_id = BytesN::from_array(&e, &[0xffu8; 32]);

        let result = client.get_project(&fake_id);
        assert!(result.is_none());
    }

    #[test]
    fn test_update_project_status_active() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let name = String::from_str(&e, "Status Test");
        let methodology = String::from_str(&e, "Test_v1");
        let project_id = client.register_project(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &100,
            &wasm_hash,
        );

        let new_status = String::from_str(&e, "active");
        client.update_project_status(&admin, &project_id, &new_status);

        let project = client.get_project(&project_id).unwrap();
        assert_eq!(project.status, new_status);
    }

    #[test]
    fn test_update_project_status_full_cycle() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let name = String::from_str(&e, "Full Cycle");
        let methodology = String::from_str(&e, "Test_v1");
        let project_id = client.register_project(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &100,
            &wasm_hash,
        );

        for status in [
            String::from_str(&e, "active"),
            String::from_str(&e, "completed"),
        ] {
            client.update_project_status(&admin, &project_id, &status);
            let project = client.get_project(&project_id).unwrap();
            assert_eq!(project.status, status);
        }
    }

    // ── Valid transition tests ──

    #[test]
    fn test_transition_registered_to_active() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();
        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "T1"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );
        client.update_project_status(&admin, &pid, &String::from_str(&e, "active"));
        assert_eq!(
            client.get_project(&pid).unwrap().status,
            String::from_str(&e, "active")
        );
    }

    #[test]
    fn test_transition_active_to_completed() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();
        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "T2"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );
        client.update_project_status(&admin, &pid, &String::from_str(&e, "active"));
        client.update_project_status(&admin, &pid, &String::from_str(&e, "completed"));
        assert_eq!(
            client.get_project(&pid).unwrap().status,
            String::from_str(&e, "completed")
        );
    }

    #[test]
    fn test_transition_active_to_suspended() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();
        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "T3"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );
        client.update_project_status(&admin, &pid, &String::from_str(&e, "active"));
        client.update_project_status(&admin, &pid, &String::from_str(&e, "suspended"));
        assert_eq!(
            client.get_project(&pid).unwrap().status,
            String::from_str(&e, "suspended")
        );
    }

    #[test]
    fn test_transition_completed_to_active() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();
        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "T4"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );
        client.update_project_status(&admin, &pid, &String::from_str(&e, "active"));
        client.update_project_status(&admin, &pid, &String::from_str(&e, "completed"));
        client.update_project_status(&admin, &pid, &String::from_str(&e, "active"));
        assert_eq!(
            client.get_project(&pid).unwrap().status,
            String::from_str(&e, "active")
        );
    }

    #[test]
    fn test_transition_suspended_to_registered() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();
        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "T5"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );
        client.update_project_status(&admin, &pid, &String::from_str(&e, "active"));
        client.update_project_status(&admin, &pid, &String::from_str(&e, "suspended"));
        client.update_project_status(&admin, &pid, &String::from_str(&e, "registered"));
        assert_eq!(
            client.get_project(&pid).unwrap().status,
            String::from_str(&e, "registered")
        );
    }

    // ── Same-status no-op tests ──

    #[test]
    fn test_same_status_noop_registered() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();
        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "Noop1"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );
        // Should not panic — no-op
        client.update_project_status(&admin, &pid, &String::from_str(&e, "registered"));
        assert_eq!(
            client.get_project(&pid).unwrap().status,
            String::from_str(&e, "registered")
        );
    }

    #[test]
    fn test_same_status_noop_active() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();
        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "Noop2"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );
        client.update_project_status(&admin, &pid, &String::from_str(&e, "active"));
        // Should not panic — no-op
        client.update_project_status(&admin, &pid, &String::from_str(&e, "active"));
        assert_eq!(
            client.get_project(&pid).unwrap().status,
            String::from_str(&e, "active")
        );
    }

    // ── Forbidden transition tests ──

    #[test]
    #[should_panic(expected = "invalid status transition")]
    fn test_forbidden_completed_to_registered() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();
        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "F1"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );
        client.update_project_status(&admin, &pid, &String::from_str(&e, "active"));
        client.update_project_status(&admin, &pid, &String::from_str(&e, "completed"));
        // This should panic
        client.update_project_status(&admin, &pid, &String::from_str(&e, "registered"));
    }

    #[test]
    #[should_panic(expected = "invalid status transition")]
    fn test_forbidden_registered_to_completed() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();
        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "F2"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );
        // This should panic — registered → completed is not allowed
        client.update_project_status(&admin, &pid, &String::from_str(&e, "completed"));
    }

    #[test]
    #[should_panic(expected = "invalid status transition")]
    fn test_forbidden_suspended_to_completed() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();
        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "F3"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );
        client.update_project_status(&admin, &pid, &String::from_str(&e, "active"));
        client.update_project_status(&admin, &pid, &String::from_str(&e, "suspended"));
        // This should panic
        client.update_project_status(&admin, &pid, &String::from_str(&e, "completed"));
    }

    #[test]
    fn test_update_project_owner_by_admin() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let name = String::from_str(&e, "Ownership Test");
        let methodology = String::from_str(&e, "Test_v1");
        let project_id = client.register_project(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &100,
            &wasm_hash,
        );

        let new_owner = Address::generate(&e);
        client.update_project_owner(&admin, &project_id, &new_owner);
        let project = client.get_project(&project_id).unwrap();
        assert_eq!(project.owner, new_owner);
    }

    #[test]
    fn test_update_project_owner_by_current_owner() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let name = String::from_str(&e, "Owner Self-Transfer");
        let methodology = String::from_str(&e, "Test_v1");
        let project_id = client.register_project(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &100,
            &wasm_hash,
        );

        let new_owner = Address::generate(&e);
        client.update_project_owner(&owner, &project_id, &new_owner);
        let project = client.get_project(&project_id).unwrap();
        assert_eq!(project.owner, new_owner);
    }

    #[test]
    fn test_duplicate_registrations_produce_different_ids() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let name = String::from_str(&e, "Test Project");
        let methodology = String::from_str(&e, "Test_v1");

        let id1 = client.register_project(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &500,
            &wasm_hash,
        );

        let id2 = client.register_project(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &500,
            &wasm_hash,
        );

        assert_ne!(id1, id2);
        assert_eq!(client.project_count(), 2);
    }

    #[test]
    fn test_register_project_different_names_produce_different_ids() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let name1 = String::from_str(&e, "Project A");
        let name2 = String::from_str(&e, "Project B");
        let methodology = String::from_str(&e, "Test_v1");

        let id1 = client.register_project(
            &admin,
            &name1,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &500,
            &wasm_hash,
        );
        let id2 = client.register_project(
            &admin,
            &name2,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &500,
            &wasm_hash,
        );

        assert_ne!(id1, id2);
    }

    // ── Event tests ──

    #[test]
    fn test_initialize_emits_event() {
        let e = Env::default();
        let admin = Address::generate(&e);
        let contract_id = e.register_contract(None, CreditFactory);
        let client = CreditFactoryClient::new(&e, &contract_id);

        client.initialize(&admin);

        let events = e.events().all();
        assert_eq!(events.len(), 1);
        let (_contract, topics, _data) = &events.get(0).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("init"));
    }

    #[test]
    fn test_update_project_status_emits_event() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "Status Event"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );

        client.update_project_status(&admin, &pid, &String::from_str(&e, "active"));

        let events = e.events().all();
        // initialize(1) + register_project(1) + update_project_status(1) = 3
        assert_eq!(events.len(), 3);
        let (_contract, topics, data) = &events.get(2).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("stat_chg"));

        let (ev_id, ev_old, ev_new) =
            <(BytesN<32>, String, String)>::try_from_val(&e, data).unwrap();
        assert_eq!(ev_id, pid);
        assert_eq!(ev_old, String::from_str(&e, "registered"));
        assert_eq!(ev_new, String::from_str(&e, "active"));
    }

    #[test]
    fn test_update_project_owner_emits_event() {
        let (e, admin, owner, wasm_hash, client) = setup_with_client();
        e.mock_all_auths();

        let pid = client.register_project(
            &admin,
            &String::from_str(&e, "Owner Event"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
            &wasm_hash,
        );

        let new_owner = Address::generate(&e);
        client.update_project_owner(&admin, &pid, &new_owner);

        let events = e.events().all();
        // initialize(1) + register_project(1) + update_project_owner(1) = 3
        assert_eq!(events.len(), 3);
        let (_contract, topics, data) = &events.get(2).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("ownr_chg"));

        let (ev_id, ev_old, ev_new) =
            <(BytesN<32>, Address, Address)>::try_from_val(&e, data).unwrap();
        assert_eq!(ev_id, pid);
        assert_eq!(ev_old, owner);
        assert_eq!(ev_new, new_owner);
    }
}
