#![no_std]
#![allow(clippy::too_many_arguments)]
use shared::{generate_project_id, is_valid_status, is_valid_status_transition};
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, Address, BytesN, Env, String, Symbol, Vec,
};

#[cfg(test)]
extern crate std;

// ── Events ──
const EVENT_INITIALIZED: Symbol = symbol_short!("init");
const EVENT_PROJECT_REGISTERED: Symbol = symbol_short!("proj_reg");
const EVENT_STATUS_CHANGED: Symbol = symbol_short!("stat_chg");
const EVENT_OWNER_CHANGED: Symbol = symbol_short!("ownr_chg");

// ── TTL constants ──
/// Projects are permanent registrations: 10 years.
const PROJECT_TTL_THRESHOLD: u32 = 63_072_000;
const PROJECT_TTL_BUMP: u32 = 63_072_000;
/// Index entries match project lifetime.
const INDEX_TTL_THRESHOLD: u32 = 63_072_000;
const INDEX_TTL_BUMP: u32 = 63_072_000;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectEntry {
    pub id: BytesN<32>,
    pub name: String,
    pub owner: Address,
    pub latitude: i64,
    pub longitude: i64,
    pub methodology: String,
    pub area_hectares: u64,
    pub status: String,
    pub registered_at: u64,
}

/// Storage key enum.
///
/// Instance:   Admin, ProjectCount
/// Persistent: Project(BytesN<32>), ProjectIdAt(u64)
///
/// The old ProjectIds Vec<BytesN<32>> is replaced by a compound-key index:
///   ProjectIdAt(position: u64) → BytesN<32>
/// Iteration: for pos in 0..ProjectCount { get ProjectIdAt(pos) }
#[contracttype]
pub enum DataKey {
    // ── Instance ──
    Admin,
    ProjectCount,
    // ── Persistent ──
    Project(BytesN<32>),
    ProjectIdAt(u64),
}

fn has_admin(e: &Env) -> bool {
    e.storage().instance().has(&DataKey::Admin)
}

fn read_admin(e: &Env) -> Address {
    e.storage().instance().get(&DataKey::Admin).unwrap()
}

#[contract]
pub struct ProjectRegistry;

#[contractimpl]
#[allow(clippy::too_many_arguments)]
impl ProjectRegistry {
    /// Initialize the project registry with an admin. Callable once.
    pub fn initialize(e: Env, admin: Address) {
        if has_admin(&e) {
            panic!("already initialized");
        }
        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::ProjectCount, &0u64);

        e.events().publish((EVENT_INITIALIZED,), (admin,));
    }

    /// Register a new project. Admin only. Returns the unique project ID.
    pub fn register(
        e: Env,
        caller: Address,
        name: String,
        latitude: i64,
        longitude: i64,
        methodology: String,
        owner: Address,
        area_hectares: u64,
    ) -> BytesN<32> {
        caller.require_auth();
        let stored: Address = read_admin(&e);
        if caller != stored {
            panic!("unauthorized");
        }

        if name.len() == 0 {
            panic!("name must not be empty");
        }
        if area_hectares == 0 {
            panic!("area must be positive");
        }

        let count: u64 = e.storage().instance().get(&DataKey::ProjectCount).unwrap();
        let timestamp = e.ledger().timestamp();

        let project_id = generate_project_id(
            &e,
            count,
            timestamp,
            &name,
            &methodology,
            latitude,
            longitude,
            area_hectares,
        );

        let project = ProjectEntry {
            id: project_id.clone(),
            name: name.clone(),
            owner: owner.clone(),
            latitude,
            longitude,
            methodology,
            area_hectares,
            status: String::from_str(&e, "registered"),
            registered_at: timestamp,
        };

        // Store project entry in persistent storage
        let proj_key = DataKey::Project(project_id.clone());
        e.storage().persistent().set(&proj_key, &project);
        e.storage()
            .persistent()
            .extend_ttl(&proj_key, PROJECT_TTL_THRESHOLD, PROJECT_TTL_BUMP);

        // Append to positional index
        let idx_key = DataKey::ProjectIdAt(count);
        e.storage().persistent().set(&idx_key, &project_id);
        e.storage()
            .persistent()
            .extend_ttl(&idx_key, INDEX_TTL_THRESHOLD, INDEX_TTL_BUMP);

        e.storage()
            .instance()
            .set(&DataKey::ProjectCount, &(count + 1));

        e.events().publish(
            (EVENT_PROJECT_REGISTERED,),
            (project_id.clone(), owner, name, timestamp),
        );

        project_id
    }

    /// Get a project entry by its ID. Returns None if not found.
    pub fn get(e: Env, project_id: BytesN<32>) -> Option<ProjectEntry> {
        let key = DataKey::Project(project_id);
        let result: Option<ProjectEntry> = e.storage().persistent().get(&key);
        if result.is_some() {
            e.storage()
                .persistent()
                .extend_ttl(&key, PROJECT_TTL_THRESHOLD, PROJECT_TTL_BUMP);
        }
        result
    }

    /// Update a project's status. Valid statuses: registered, active, completed, suspended. Admin only.
    ///
    /// Enforces a strict state machine:
    ///   registered → active, active → completed, active → suspended,
    ///   completed → active, suspended → registered.
    ///
    /// Same-status updates are no-ops (returns without writing).
    pub fn update_status(e: Env, caller: Address, project_id: BytesN<32>, status: String) {
        caller.require_auth();
        let stored: Address = read_admin(&e);
        if caller != stored {
            panic!("unauthorized");
        }

        if !is_valid_status(&e, &status) {
            panic!("invalid status");
        }

        let key = DataKey::Project(project_id.clone());
        let mut project: ProjectEntry = e
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

    /// Get the total number of registered projects.
    pub fn count(e: Env) -> u64 {
        e.storage().instance().get(&DataKey::ProjectCount).unwrap()
    }

    /// List registered projects with pagination.
    /// `offset` is the zero-based start position; `limit` is the max entries to return.
    pub fn list_all(e: Env) -> Vec<ProjectEntry> {
        let total: u64 = e.storage().instance().get(&DataKey::ProjectCount).unwrap();
        let mut projects: Vec<ProjectEntry> = Vec::new(&e);
        for pos in 0..total {
            let idx_key = DataKey::ProjectIdAt(pos);
            if let Some(id) = e.storage().persistent().get::<_, BytesN<32>>(&idx_key) {
                let proj_key = DataKey::Project(id);
                if let Some(project) = e.storage().persistent().get::<_, ProjectEntry>(&proj_key) {
                    e.storage().persistent().extend_ttl(
                        &proj_key,
                        PROJECT_TTL_THRESHOLD,
                        PROJECT_TTL_BUMP,
                    );
                    projects.push_back(project);
                }
            }
        }
        projects
    }

    /// List registered projects with pagination.
    /// `offset` is the zero-based start position; `limit` is the max entries to return.
    pub fn list_paginated(e: Env, offset: u64, limit: u32) -> Vec<ProjectEntry> {
        let total: u64 = e.storage().instance().get(&DataKey::ProjectCount).unwrap();
        let end = (offset + limit as u64).min(total);
        let mut projects: Vec<ProjectEntry> = Vec::new(&e);
        for pos in offset..end {
            let idx_key = DataKey::ProjectIdAt(pos);
            if let Some(id) = e.storage().persistent().get::<_, BytesN<32>>(&idx_key) {
                let proj_key = DataKey::Project(id);
                if let Some(project) = e.storage().persistent().get::<_, ProjectEntry>(&proj_key) {
                    e.storage().persistent().extend_ttl(
                        &proj_key,
                        PROJECT_TTL_THRESHOLD,
                        PROJECT_TTL_BUMP,
                    );
                    projects.push_back(project);
                }
            }
        }
        projects
    }

    /// Transfer ownership of a project to a new address.
    /// Only the current project owner or the admin can call this.
    pub fn update_owner(e: Env, caller: Address, project_id: BytesN<32>, new_owner: Address) {
        caller.require_auth();
        let admin = read_admin(&e);
        let key = DataKey::Project(project_id.clone());
        let mut project: ProjectEntry = e
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::Events;
    use soroban_sdk::TryFromVal;

    fn setup() -> (Env, Address, ProjectRegistryClient<'static>) {
        let e = Env::default();
        let admin = Address::generate(&e);
        let contract_id = e.register_contract(None, ProjectRegistry);
        let client = ProjectRegistryClient::new(&e, &contract_id);
        client.initialize(&admin);
        (e, admin, client)
    }

    #[test]
    fn test_initialize() {
        let (_e, _admin, client) = setup();
        assert_eq!(client.count(), 0);
        let all = client.list_all();
        assert_eq!(all.len(), 0);
    }

    #[test]
    fn test_register_project() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let owner = Address::generate(&e);
        let name = String::from_str(&e, "Green Valley Wetland");
        let methodology = String::from_str(&e, "Wetland_Restoration_v2.1");

        let id = client.register(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &500,
        );

        let project = client.get(&id).unwrap();
        assert_eq!(project.name, name);
        assert_eq!(project.owner, owner);
        assert_eq!(project.status, String::from_str(&e, "registered"));
        assert_eq!(project.area_hectares, 500);
        assert_eq!(client.count(), 1);
    }

    #[test]
    fn test_register_multiple_projects() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let owner = Address::generate(&e);
        let id1 = client.register(
            &admin,
            &String::from_str(&e, "Project A"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &500,
        );
        let id2 = client.register(
            &admin,
            &String::from_str(&e, "Project B"),
            &38900000,
            &(-77040000),
            &String::from_str(&e, "v2"),
            &owner,
            &300,
        );

        assert_eq!(client.count(), 2);
        assert_ne!(id1, id2);

        let all = client.list_all();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_update_status() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "Status Test"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &500,
        );

        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        let project = client.get(&id).unwrap();
        assert_eq!(project.status, String::from_str(&e, "active"));

        client.update_status(&admin, &id, &String::from_str(&e, "completed"));
        let project = client.get(&id).unwrap();
        assert_eq!(project.status, String::from_str(&e, "completed"));
    }

    #[test]
    fn test_invalid_status_safe() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "Safe"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &500,
        );

        // Valid status transitions
        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        client.update_status(&admin, &id, &String::from_str(&e, "completed"));
        let project = client.get(&id).unwrap();
        assert_eq!(project.status, String::from_str(&e, "completed"));
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let (_e, _admin, client) = setup();
        let fake_id = BytesN::from_array(&_e, &[0xffu8; 32]);
        let result = client.get(&fake_id);
        assert!(result.is_none());
    }

    #[test]
    fn test_empty_list_all() {
        let (_e, _admin, client) = setup();
        let all = client.list_all();
        assert_eq!(all.len(), 0);
    }

    #[test]
    fn test_update_owner_by_admin() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let owner = Address::generate(&e);
        let new_owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "Owner Test"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &500,
        );

        client.update_owner(&admin, &id, &new_owner);
        let project = client.get(&id).unwrap();
        assert_eq!(project.owner, new_owner);
    }

    #[test]
    fn test_update_owner_by_current_owner() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let owner = Address::generate(&e);
        let new_owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "Owner Transfer"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &500,
        );

        client.update_owner(&owner, &id, &new_owner);
        let project = client.get(&id).unwrap();
        assert_eq!(project.owner, new_owner);
    }

    // ── Valid transition tests ──

    #[test]
    fn test_transition_registered_to_active() {
        let (e, admin, client) = setup();
        e.mock_all_auths();
        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "T1"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );
        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        assert_eq!(
            client.get(&id).unwrap().status,
            String::from_str(&e, "active")
        );
    }

    #[test]
    fn test_transition_active_to_completed() {
        let (e, admin, client) = setup();
        e.mock_all_auths();
        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "T2"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );
        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        client.update_status(&admin, &id, &String::from_str(&e, "completed"));
        assert_eq!(
            client.get(&id).unwrap().status,
            String::from_str(&e, "completed")
        );
    }

    #[test]
    fn test_transition_active_to_suspended() {
        let (e, admin, client) = setup();
        e.mock_all_auths();
        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "T3"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );
        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        client.update_status(&admin, &id, &String::from_str(&e, "suspended"));
        assert_eq!(
            client.get(&id).unwrap().status,
            String::from_str(&e, "suspended")
        );
    }

    #[test]
    fn test_transition_completed_to_active() {
        let (e, admin, client) = setup();
        e.mock_all_auths();
        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "T4"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );
        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        client.update_status(&admin, &id, &String::from_str(&e, "completed"));
        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        assert_eq!(
            client.get(&id).unwrap().status,
            String::from_str(&e, "active")
        );
    }

    #[test]
    fn test_transition_suspended_to_registered() {
        let (e, admin, client) = setup();
        e.mock_all_auths();
        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "T5"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );
        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        client.update_status(&admin, &id, &String::from_str(&e, "suspended"));
        client.update_status(&admin, &id, &String::from_str(&e, "registered"));
        assert_eq!(
            client.get(&id).unwrap().status,
            String::from_str(&e, "registered")
        );
    }

    // ── Same-status no-op tests ──

    #[test]
    fn test_same_status_noop_registered() {
        let (e, admin, client) = setup();
        e.mock_all_auths();
        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "Noop1"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );
        // Should not panic — no-op
        client.update_status(&admin, &id, &String::from_str(&e, "registered"));
        assert_eq!(
            client.get(&id).unwrap().status,
            String::from_str(&e, "registered")
        );
    }

    #[test]
    fn test_same_status_noop_active() {
        let (e, admin, client) = setup();
        e.mock_all_auths();
        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "Noop2"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );
        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        // Should not panic — no-op
        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        assert_eq!(
            client.get(&id).unwrap().status,
            String::from_str(&e, "active")
        );
    }

    // ── Forbidden transition tests ──

    #[test]
    #[should_panic(expected = "invalid status transition")]
    fn test_forbidden_completed_to_registered() {
        let (e, admin, client) = setup();
        e.mock_all_auths();
        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "F1"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );
        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        client.update_status(&admin, &id, &String::from_str(&e, "completed"));
        // This should panic
        client.update_status(&admin, &id, &String::from_str(&e, "registered"));
    }

    #[test]
    #[should_panic(expected = "invalid status transition")]
    fn test_forbidden_registered_to_completed() {
        let (e, admin, client) = setup();
        e.mock_all_auths();
        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "F2"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );
        // This should panic — registered → completed is not allowed
        client.update_status(&admin, &id, &String::from_str(&e, "completed"));
    }

    #[test]
    #[should_panic(expected = "invalid status transition")]
    fn test_forbidden_suspended_to_completed() {
        let (e, admin, client) = setup();
        e.mock_all_auths();
        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "F3"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );
        client.update_status(&admin, &id, &String::from_str(&e, "active"));
        client.update_status(&admin, &id, &String::from_str(&e, "suspended"));
        // This should panic
        client.update_status(&admin, &id, &String::from_str(&e, "completed"));
    }

    // ── Event tests ──

    #[test]
    fn test_initialize_emits_event() {
        let e = Env::default();
        let admin = Address::generate(&e);
        let contract_id = e.register_contract(None, ProjectRegistry);
        let client = ProjectRegistryClient::new(&e, &contract_id);

        client.initialize(&admin);

        let events = e.events().all();
        assert_eq!(events.len(), 1);
        let (_contract, topics, _data) = &events.get(0).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("init"));
    }

    #[test]
    fn test_register_emits_event() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let owner = Address::generate(&e);
        let name = String::from_str(&e, "Event Test Project");
        let methodology = String::from_str(&e, "v1");

        let id = client.register(
            &admin,
            &name,
            &38897700,
            &(-77036500),
            &methodology,
            &owner,
            &500,
        );

        let events = e.events().all();
        // initialize(1) + register(1) = 2
        assert_eq!(events.len(), 2);
        let (_contract, topics, data) = &events.get(1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("proj_reg"));

        let (ev_id, ev_owner, ev_name, _ev_timestamp) =
            <(BytesN<32>, Address, String, u64)>::try_from_val(&e, data).unwrap();
        assert_eq!(ev_id, id);
        assert_eq!(ev_owner, owner);
        assert_eq!(ev_name, name);
    }

    #[test]
    fn test_update_status_emits_event() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "Status Event"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );

        client.update_status(&admin, &id, &String::from_str(&e, "active"));

        let events = e.events().all();
        // initialize(1) + register(1) + update_status(1) = 3
        assert_eq!(events.len(), 3);
        let (_contract, topics, data) = &events.get(2).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("stat_chg"));

        let (ev_id, ev_old, ev_new) =
            <(BytesN<32>, String, String)>::try_from_val(&e, data).unwrap();
        assert_eq!(ev_id, id);
        assert_eq!(ev_old, String::from_str(&e, "registered"));
        assert_eq!(ev_new, String::from_str(&e, "active"));
    }

    #[test]
    fn test_update_status_noop_does_not_emit_event() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "Noop Event"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );

        // Same-status update is a no-op and must not emit a status-changed event.
        client.update_status(&admin, &id, &String::from_str(&e, "registered"));

        let events = e.events().all();
        // Only initialize(1) + register(1) should be present — no stat_chg event.
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_update_owner_emits_event() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let owner = Address::generate(&e);
        let new_owner = Address::generate(&e);
        let id = client.register(
            &admin,
            &String::from_str(&e, "Owner Event"),
            &38897700,
            &(-77036500),
            &String::from_str(&e, "v1"),
            &owner,
            &100,
        );

        client.update_owner(&admin, &id, &new_owner);

        let events = e.events().all();
        // initialize(1) + register(1) + update_owner(1) = 3
        assert_eq!(events.len(), 3);
        let (_contract, topics, data) = &events.get(2).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("ownr_chg"));

        let (ev_id, ev_old, ev_new) =
            <(BytesN<32>, Address, Address)>::try_from_val(&e, data).unwrap();
        assert_eq!(ev_id, id);
        assert_eq!(ev_old, owner);
        assert_eq!(ev_new, new_owner);
    }
}
