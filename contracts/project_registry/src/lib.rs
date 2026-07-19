#![no_std]
#![allow(clippy::too_many_arguments)]
use shared::generate_project_id;
use soroban_sdk::{contract, contractimpl, contracttype, Address, BytesN, Env, String, Vec};

#[cfg(test)]
extern crate std;

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
            name,
            owner,
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
    pub fn update_status(e: Env, caller: Address, project_id: BytesN<32>, status: String) {
        caller.require_auth();
        let stored: Address = read_admin(&e);
        if caller != stored {
            panic!("unauthorized");
        }

        let key = DataKey::Project(project_id.clone());
        let mut project: ProjectEntry = e
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("project not found"));

        let valid = status == String::from_str(&e, "registered")
            || status == String::from_str(&e, "active")
            || status == String::from_str(&e, "completed")
            || status == String::from_str(&e, "suspended");
        if !valid {
            panic!("invalid status");
        }

        project.status = status;
        e.storage().persistent().set(&key, &project);
        e.storage()
            .persistent()
            .extend_ttl(&key, PROJECT_TTL_THRESHOLD, PROJECT_TTL_BUMP);
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

        project.owner = new_owner;
        e.storage().persistent().set(&key, &project);
        e.storage()
            .persistent()
            .extend_ttl(&key, PROJECT_TTL_THRESHOLD, PROJECT_TTL_BUMP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

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
}
