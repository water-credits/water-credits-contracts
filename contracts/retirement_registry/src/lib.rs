#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, Address, BytesN, Env, String, Symbol, Vec,
};

#[cfg(test)]
extern crate std;

// ── Events ──
const EVENT_INITIALIZED: Symbol = symbol_short!("init");
const EVENT_RETIREMENT_RECORDED: Symbol = symbol_short!("ret_rec");
const EVENT_AUTH_CALLER_SET: Symbol = symbol_short!("auth_set");

// ── TTL constants ──
/// Retirement records are permanent audit trails: 10 years.
const RECORD_TTL_THRESHOLD: u32 = 63_072_000;
const RECORD_TTL_BUMP: u32 = 63_072_000;
/// Index entries share the record lifetime.
const INDEX_TTL_THRESHOLD: u32 = 63_072_000;
const INDEX_TTL_BUMP: u32 = 63_072_000;
/// AuthorizedCaller entries: 1 year.
const AUTH_TTL_THRESHOLD: u32 = 6_307_200;
const AUTH_TTL_BUMP: u32 = 6_307_200;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RetirementRecord {
    pub id: u64,
    pub retiree: Address,
    pub project_id: BytesN<32>,
    pub amount: i128,
    pub purpose: String,
    pub metadata_uri: String,
    pub timestamp: u64,
}

/// Storage key enum.
///
/// Instance:  Admin, RecordCount, TotalRetired
/// Persistent: Record(u64), AuthorizedCaller(Address),
///             RetireeIndex(Address, u64), ProjectIndex(BytesN<32>, u64)
///
/// The old Vec<u64> secondary indexes (RetireeRecords, ProjectRecords) are
/// replaced by compound keys:
///   RetireeIndex(retiree, position)  → record_id: u64
///   RetireeCount(retiree)            → count: u64   (how many entries this retiree has)
///   ProjectIndex(project_id, pos)   → record_id: u64
///   ProjectCount(project_id)        → count: u64
#[contracttype]
pub enum DataKey {
    // ── Instance ──
    Admin,
    RecordCount,
    TotalRetired,
    // ── Persistent ──
    Record(u64),
    AuthorizedCaller(Address),
    RetireeIndex(Address, u64),
    RetireeCount(Address),
    ProjectIndex(BytesN<32>, u64),
    ProjectCount(BytesN<32>),
}

fn has_admin(e: &Env) -> bool {
    e.storage().instance().has(&DataKey::Admin)
}

fn read_admin(e: &Env) -> Address {
    e.storage().instance().get(&DataKey::Admin).unwrap()
}

#[contract]
pub struct RetirementRegistry;

#[contractimpl]
impl RetirementRegistry {
    /// Initialize the retirement registry with an admin. Callable once.
    pub fn initialize(e: Env, admin: Address) {
        if has_admin(&e) {
            panic!("already initialized");
        }
        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::RecordCount, &0u64);
        e.storage().instance().set(&DataKey::TotalRetired, &0i128);

        e.events().publish((EVENT_INITIALIZED,), (admin,));
    }

    /// Record a retirement. Only callable by admin or an authorized caller contract.
    /// Returns the unique record ID.
    pub fn record_retirement(
        e: Env,
        caller: Address,
        retiree: Address,
        project_id: BytesN<32>,
        amount: i128,
        purpose: String,
        metadata_uri: String,
    ) -> u64 {
        caller.require_auth();
        let stored: Address = read_admin(&e);
        let auth_key = DataKey::AuthorizedCaller(caller.clone());
        let authorized: bool = e.storage().persistent().get(&auth_key).unwrap_or(false);
        if caller != stored && !authorized {
            panic!("unauthorized");
        }

        if amount <= 0 {
            panic!("amount must be positive");
        }

        let count: u64 = e.storage().instance().get(&DataKey::RecordCount).unwrap();
        let record_id = count + 1;
        let timestamp = e.ledger().timestamp();

        let record = RetirementRecord {
            id: record_id,
            retiree: retiree.clone(),
            project_id: project_id.clone(),
            amount,
            purpose: purpose.clone(),
            metadata_uri: metadata_uri.clone(),
            timestamp,
        };

        // Persist the record
        let rec_key = DataKey::Record(record_id);
        e.storage().persistent().set(&rec_key, &record);
        e.storage()
            .persistent()
            .extend_ttl(&rec_key, RECORD_TTL_THRESHOLD, RECORD_TTL_BUMP);

        // Update retiree compound-key index
        let retiree_count_key = DataKey::RetireeCount(retiree.clone());
        let retiree_pos: u64 = e
            .storage()
            .persistent()
            .get(&retiree_count_key)
            .unwrap_or(0);
        let idx_key = DataKey::RetireeIndex(retiree.clone(), retiree_pos);
        e.storage().persistent().set(&idx_key, &record_id);
        e.storage()
            .persistent()
            .extend_ttl(&idx_key, INDEX_TTL_THRESHOLD, INDEX_TTL_BUMP);
        let new_retiree_pos = retiree_pos + 1;
        e.storage()
            .persistent()
            .set(&retiree_count_key, &new_retiree_pos);
        e.storage().persistent().extend_ttl(
            &retiree_count_key,
            INDEX_TTL_THRESHOLD,
            INDEX_TTL_BUMP,
        );

        // Update project compound-key index
        let project_count_key = DataKey::ProjectCount(project_id.clone());
        let project_pos: u64 = e
            .storage()
            .persistent()
            .get(&project_count_key)
            .unwrap_or(0);
        let pidx_key = DataKey::ProjectIndex(project_id.clone(), project_pos);
        e.storage().persistent().set(&pidx_key, &record_id);
        e.storage()
            .persistent()
            .extend_ttl(&pidx_key, INDEX_TTL_THRESHOLD, INDEX_TTL_BUMP);
        let new_project_pos = project_pos + 1;
        e.storage()
            .persistent()
            .set(&project_count_key, &new_project_pos);
        e.storage().persistent().extend_ttl(
            &project_count_key,
            INDEX_TTL_THRESHOLD,
            INDEX_TTL_BUMP,
        );

        // Update global scalars
        let total: i128 = e.storage().instance().get(&DataKey::TotalRetired).unwrap();
        e.storage()
            .instance()
            .set(&DataKey::TotalRetired, &(total + amount));
        e.storage()
            .instance()
            .set(&DataKey::RecordCount, &record_id);

        e.events().publish(
            (EVENT_RETIREMENT_RECORDED,),
            (record_id, retiree, project_id, amount, purpose, timestamp),
        );

        record_id
    }

    /// Get a retirement record by its ID. Returns None if not found.
    pub fn get_record(e: Env, id: u64) -> Option<RetirementRecord> {
        let key = DataKey::Record(id);
        let result: Option<RetirementRecord> = e.storage().persistent().get(&key);
        if result.is_some() {
            e.storage()
                .persistent()
                .extend_ttl(&key, RECORD_TTL_THRESHOLD, RECORD_TTL_BUMP);
        }
        result
    }

    /// Get the global total amount of credits retired across all projects.
    pub fn total_retired(e: Env) -> i128 {
        e.storage().instance().get(&DataKey::TotalRetired).unwrap()
    }

    /// Get the total number of retirement records in the registry.
    pub fn record_count(e: Env) -> u64 {
        e.storage().instance().get(&DataKey::RecordCount).unwrap()
    }

    /// Authorize or revoke a contract address to record retirements. Admin only.
    pub fn set_authorized_caller(e: Env, admin: Address, caller: Address, authorized: bool) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        let key = DataKey::AuthorizedCaller(caller.clone());
        e.storage().persistent().set(&key, &authorized);
        e.storage()
            .persistent()
            .extend_ttl(&key, AUTH_TTL_THRESHOLD, AUTH_TTL_BUMP);

        e.events()
            .publish((EVENT_AUTH_CALLER_SET,), (caller, authorized));
    }

    /// Get paginated retirement records for a given retiree address.
    /// `offset` is the zero-based start position; `limit` is the max entries to return.
    pub fn get_retirements_by_retiree(
        e: Env,
        retiree: Address,
        offset: u64,
        limit: u32,
    ) -> Vec<RetirementRecord> {
        let count_key = DataKey::RetireeCount(retiree.clone());
        let total: u64 = e.storage().persistent().get(&count_key).unwrap_or(0);

        let mut records: Vec<RetirementRecord> = Vec::new(&e);
        let end = (offset + limit as u64).min(total);
        for pos in offset..end {
            let idx_key = DataKey::RetireeIndex(retiree.clone(), pos);
            if let Some(record_id) = e.storage().persistent().get::<_, u64>(&idx_key) {
                let rec_key = DataKey::Record(record_id);
                if let Some(record) = e
                    .storage()
                    .persistent()
                    .get::<_, RetirementRecord>(&rec_key)
                {
                    e.storage().persistent().extend_ttl(
                        &rec_key,
                        RECORD_TTL_THRESHOLD,
                        RECORD_TTL_BUMP,
                    );
                    records.push_back(record);
                }
            }
        }
        records
    }

    /// Get paginated retirement records for a given project ID.
    /// `offset` is the zero-based start position; `limit` is the max entries to return.
    pub fn get_retirements_by_project(
        e: Env,
        project_id: BytesN<32>,
        offset: u64,
        limit: u32,
    ) -> Vec<RetirementRecord> {
        let count_key = DataKey::ProjectCount(project_id.clone());
        let total: u64 = e.storage().persistent().get(&count_key).unwrap_or(0);

        let mut records: Vec<RetirementRecord> = Vec::new(&e);
        let end = (offset + limit as u64).min(total);
        for pos in offset..end {
            let idx_key = DataKey::ProjectIndex(project_id.clone(), pos);
            if let Some(record_id) = e.storage().persistent().get::<_, u64>(&idx_key) {
                let rec_key = DataKey::Record(record_id);
                if let Some(record) = e
                    .storage()
                    .persistent()
                    .get::<_, RetirementRecord>(&rec_key)
                {
                    e.storage().persistent().extend_ttl(
                        &rec_key,
                        RECORD_TTL_THRESHOLD,
                        RECORD_TTL_BUMP,
                    );
                    records.push_back(record);
                }
            }
        }
        records
    }

    /// Get the total number of retirements for a specific retiree.
    pub fn retiree_count(e: Env, retiree: Address) -> u64 {
        e.storage()
            .persistent()
            .get(&DataKey::RetireeCount(retiree))
            .unwrap_or(0)
    }

    /// Get the total number of retirements for a specific project.
    pub fn project_retirement_count(e: Env, project_id: BytesN<32>) -> u64 {
        e.storage()
            .persistent()
            .get(&DataKey::ProjectCount(project_id))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::Events;
    use soroban_sdk::TryFromVal;

    fn setup() -> (Env, Address, RetirementRegistryClient<'static>) {
        let e = Env::default();
        let admin = Address::generate(&e);
        let contract_id = e.register_contract(None, RetirementRegistry);
        let client = RetirementRegistryClient::new(&e, &contract_id);
        client.initialize(&admin);
        (e, admin, client)
    }

    #[test]
    fn test_initialize() {
        let (_e, _admin, client) = setup();
        assert_eq!(client.record_count(), 0);
        assert_eq!(client.total_retired(), 0);
    }

    #[test]
    fn test_record_retirement_succeeds() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[1u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        let id = client.record_retirement(&admin, &retiree, &project_id, &500, &purpose, &uri);
        assert_eq!(id, 1);

        let record = client.get_record(&id).unwrap();
        assert_eq!(record.retiree, retiree);
        assert_eq!(record.amount, 500);
        assert_eq!(record.purpose, purpose);
        assert_eq!(record.metadata_uri, uri);

        assert_eq!(client.total_retired(), 500);
        assert_eq!(client.record_count(), 1);
    }

    #[test]
    fn test_record_retirement_multiple_entries() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree1 = Address::generate(&e);
        let retiree2 = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[1u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        client.record_retirement(&admin, &retiree1, &project_id, &300, &purpose, &uri);
        client.record_retirement(&admin, &retiree1, &project_id, &200, &purpose, &uri);
        client.record_retirement(&admin, &retiree2, &project_id, &100, &purpose, &uri);

        assert_eq!(client.record_count(), 3);
        assert_eq!(client.total_retired(), 600);

        // Paginated query for retiree1 — page 0, up to 10 results
        let records1 = client.get_retirements_by_retiree(&retiree1, &0, &10);
        assert_eq!(records1.len(), 2);
        assert_eq!(records1.get(0).unwrap().amount, 300);
        assert_eq!(records1.get(1).unwrap().amount, 200);

        let records2 = client.get_retirements_by_retiree(&retiree2, &0, &10);
        assert_eq!(records2.len(), 1);
        assert_eq!(records2.get(0).unwrap().amount, 100);
    }

    #[test]
    fn test_record_authorized_only() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[1u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        // Authorized admin can record
        client.record_retirement(&admin, &retiree, &project_id, &500, &purpose, &uri);
        assert_eq!(client.total_retired(), 500);
    }

    #[test]
    fn test_get_record_nonexistent() {
        let (_e, _admin, client) = setup();
        let record = client.get_record(&999);
        assert!(record.is_none());
    }

    #[test]
    fn test_empty_retiree_records() {
        let (e, _admin, client) = setup();
        let retiree = Address::generate(&e);
        let records = client.get_retirements_by_retiree(&retiree, &0, &10);
        assert_eq!(records.len(), 0);
    }

    #[test]
    fn test_get_retirements_by_project_single() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree1 = Address::generate(&e);
        let retiree2 = Address::generate(&e);
        let project_a = BytesN::from_array(&e, &[1u8; 32]);
        let project_b = BytesN::from_array(&e, &[2u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        client.record_retirement(&admin, &retiree1, &project_a, &300, &purpose, &uri);
        client.record_retirement(&admin, &retiree2, &project_a, &200, &purpose, &uri);
        client.record_retirement(&admin, &retiree1, &project_b, &100, &purpose, &uri);

        let proj_a_records = client.get_retirements_by_project(&project_a, &0, &10);
        assert_eq!(proj_a_records.len(), 2);

        let total_a: i128 = (0..proj_a_records.len())
            .map(|i| proj_a_records.get(i).unwrap().amount)
            .sum();
        assert_eq!(total_a, 500);

        let proj_b_records = client.get_retirements_by_project(&project_b, &0, &10);
        assert_eq!(proj_b_records.len(), 1);
        assert_eq!(proj_b_records.get(0).unwrap().amount, 100);
    }

    #[test]
    fn test_get_retirements_by_project_empty() {
        let (e, _admin, client) = setup();
        let project_id = BytesN::from_array(&e, &[0xffu8; 32]);
        let records = client.get_retirements_by_project(&project_id, &0, &10);
        assert_eq!(records.len(), 0);
    }

    // ── New: pagination correctness tests ──

    #[test]
    fn test_pagination_offset_and_limit() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[5u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        // Record 5 retirements for the same retiree
        for amount in [100i128, 200, 300, 400, 500] {
            client.record_retirement(&admin, &retiree, &project_id, &amount, &purpose, &uri);
        }

        assert_eq!(client.retiree_count(&retiree), 5);

        // First page (offset=0, limit=2) → records 0 and 1 → amounts 100, 200
        let page1 = client.get_retirements_by_retiree(&retiree, &0, &2);
        assert_eq!(page1.len(), 2);
        assert_eq!(page1.get(0).unwrap().amount, 100);
        assert_eq!(page1.get(1).unwrap().amount, 200);

        // Second page (offset=2, limit=2) → records 2 and 3 → amounts 300, 400
        let page2 = client.get_retirements_by_retiree(&retiree, &2, &2);
        assert_eq!(page2.len(), 2);
        assert_eq!(page2.get(0).unwrap().amount, 300);
        assert_eq!(page2.get(1).unwrap().amount, 400);

        // Third page (offset=4, limit=2) → only 1 record remaining → amount 500
        let page3 = client.get_retirements_by_retiree(&retiree, &4, &2);
        assert_eq!(page3.len(), 1);
        assert_eq!(page3.get(0).unwrap().amount, 500);

        // Page past the end → empty
        let page4 = client.get_retirements_by_retiree(&retiree, &5, &2);
        assert_eq!(page4.len(), 0);
    }

    #[test]
    fn test_retiree_count_helper() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[6u8; 32]);
        let purpose = String::from_str(&e, "compliance");
        let uri = String::from_str(&e, "ipfs://X");

        assert_eq!(client.retiree_count(&retiree), 0);
        client.record_retirement(&admin, &retiree, &project_id, &100, &purpose, &uri);
        assert_eq!(client.retiree_count(&retiree), 1);
        client.record_retirement(&admin, &retiree, &project_id, &200, &purpose, &uri);
        assert_eq!(client.retiree_count(&retiree), 2);
    }

    #[test]
    fn test_project_retirement_count_helper() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[7u8; 32]);
        let purpose = String::from_str(&e, "community");
        let uri = String::from_str(&e, "ipfs://Y");

        assert_eq!(client.project_retirement_count(&project_id), 0);
        client.record_retirement(&admin, &retiree, &project_id, &100, &purpose, &uri);
        assert_eq!(client.project_retirement_count(&project_id), 1);
    }

    // ── Event tests ──

    #[test]
    fn test_initialize_emits_event() {
        let e = Env::default();
        let admin = Address::generate(&e);
        let contract_id = e.register_contract(None, RetirementRegistry);
        let client = RetirementRegistryClient::new(&e, &contract_id);

        client.initialize(&admin);

        let events = e.events().all();
        assert_eq!(events.len(), 1);
        let (_contract, topics, _data) = &events.get(0).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("init"));
    }

    #[test]
    fn test_record_retirement_emits_event() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[9u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        let record_id =
            client.record_retirement(&admin, &retiree, &project_id, &500, &purpose, &uri);

        let events = e.events().all();
        // initialize(1) + record_retirement(1) = 2
        assert_eq!(events.len(), 2);
        let (_contract, topics, data) = &events.get(1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("ret_rec"));

        let (ev_id, ev_retiree, ev_project_id, ev_amount, ev_purpose, ev_timestamp) =
            <(u64, Address, BytesN<32>, i128, String, u64)>::try_from_val(&e, data).unwrap();
        assert_eq!(ev_id, record_id);
        assert_eq!(ev_retiree, retiree);
        assert_eq!(ev_project_id, project_id);
        assert_eq!(ev_amount, 500);
        assert_eq!(ev_purpose, purpose);
        assert_eq!(ev_timestamp, e.ledger().timestamp());
    }

    #[test]
    fn test_set_authorized_caller_emits_event() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let caller = Address::generate(&e);
        client.set_authorized_caller(&admin, &caller, &true);

        let events = e.events().all();
        // initialize(1) + set_authorized_caller(1) = 2
        assert_eq!(events.len(), 2);
        let (_contract, topics, data) = &events.get(1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("auth_set"));

        let (ev_caller, ev_authorized) = <(Address, bool)>::try_from_val(&e, data).unwrap();
        assert_eq!(ev_caller, caller);
        assert!(ev_authorized);
    }
}
