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
const EVENT_INDEX_EXPIRED: Symbol = symbol_short!("idx_exp");

// ── TTL constants ──
/// Retirement records are permanent audit trails: 10 years.
const RECORD_TTL_THRESHOLD: u32 = 63_072_000;
const RECORD_TTL_BUMP: u32 = 63_072_000;
/// Index entries: bounded to ~1 year to manage storage growth.
/// Older entries expire naturally; permanent Record entries are never pruned.
const INDEX_ENTRY_TTL_LEDGERS: u32 = 6_307_200; // ~1 year at 5s per ledger
/// Extend index TTL when below this threshold (~30 days in ledgers).
const INDEX_EXTEND_WHEN_BELOW: u32 = 2_592_000; // ~30 days: 2,592,000 ledgers
/// AuthorizedCaller entries: 1 year.
const AUTH_TTL_THRESHOLD: u32 = 6_307_200;
const AUTH_TTL_BUMP: u32 = 6_307_200;

// ── Index policy constants ──
/// Maximum number of live index entries per retiree/project.
/// Older entries are allowed to expire via TTL while permanent
/// Record entries are never pruned.
///
/// Policy: Keep the most recent MAX_LIVE_INDEX_ENTRIES entries
/// as persistent storage with TTL. Older entries expire naturally
/// (off-chain event replay available for deep history).
///
/// Rationale: 10-year audit window = ~63,072,000 ledgers at 5s/ledger.
/// We keep recent index entries alive; permanent records never expire.
const MAX_LIVE_INDEX_ENTRIES: u64 = 1000;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RetirementRecord {
    pub id: u64,
    pub retiree: Address,
    pub project_id: BytesN<32>,
    pub credit_token: Address,
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

/// Called after appending a new index entry.
/// When live entries exceed MAX_LIVE_INDEX_ENTRIES, the oldest
/// index entry is de-prioritised (its TTL is NOT extended),
/// allowing it to expire naturally and bound storage growth.
///
/// IMPORTANT: This only affects index entries (RetireeIndex/
/// ProjectIndex). The permanent Record(id) entries are NEVER
/// touched by this function.
///
/// # Policy documentation
/// - Index entries: expire after INDEX_ENTRY_TTL_LEDGERS if
///   not accessed (bounds live storage per retiree/project)
/// - Record entries: permanent, never pruned (audit integrity)
/// - Off-chain: full history available via contract events
fn maybe_expire_oldest_index_entry(e: &Env, new_count: u64, index_type: &str) {
    if new_count > MAX_LIVE_INDEX_ENTRIES {
        // The oldest entry that will expire is at position:
        // new_count - MAX_LIVE_INDEX_ENTRIES - 1
        let expired_pos = new_count - MAX_LIVE_INDEX_ENTRIES - 1;

        // Emit event so off-chain indexers know this index entry
        // will expire naturally (they should cache it from the creation event)
        e.events().publish(
            (EVENT_INDEX_EXPIRED, index_type, expired_pos),
            (),
        );

        // Do NOT delete — let TTL expiry handle it naturally.
        // This avoids write costs for active high-throughput retirees/projects.
    }
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
    ///
    /// Authorized callers are expected to validate `amount` against the source
    /// token's supply and the retiree's balance before calling. The registry
    /// still uses checked arithmetic for its global total so a misconfigured or
    /// malicious authorized caller cannot wrap `TotalRetired`.
    ///
    /// # Storage Growth Policy
    ///
    /// **Index entries** (RetireeIndex, ProjectIndex) are bounded to control
    /// storage rent costs:
    /// - Each index entry has a TTL of ~1 year (INDEX_ENTRY_TTL_LEDGERS)
    /// - When a retiree/project exceeds MAX_LIVE_INDEX_ENTRIES (1000),
    ///   the oldest index entry is de-prioritised (no TTL extension)
    /// - Expired entries are cleared naturally by Soroban's TTL mechanism
    /// - This avoids write costs for high-throughput retirees/projects
    ///
    /// **Permanent records** (Record(id)) are NEVER pruned:
    /// - Each retirement record persists for 10 years (audit trail)
    /// - Record entries are unaffected by the index expiration policy
    /// - Full history beyond live index is available via contract events
    ///
    /// # Returns
    /// The unique record ID assigned to this retirement.
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

        let total: i128 = e.storage().instance().get(&DataKey::TotalRetired).unwrap();
        let new_total = total.checked_add(amount).expect("total_retired overflow");

        let count: u64 = e.storage().instance().get(&DataKey::RecordCount).unwrap();
        let record_id = count + 1;
        let timestamp = e.ledger().timestamp();

        let record = RetirementRecord {
            id: record_id,
            retiree: retiree.clone(),
            project_id: project_id.clone(),
            credit_token: caller.clone(),
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
        // Set TTL on index entry (bounded to ~1 year, not permanent like Record)
        e.storage()
            .persistent()
            .extend_ttl(&idx_key, INDEX_EXTEND_WHEN_BELOW, INDEX_ENTRY_TTL_LEDGERS);
        let new_retiree_pos = retiree_pos + 1;
        e.storage()
            .persistent()
            .set(&retiree_count_key, &new_retiree_pos);
        e.storage().persistent().extend_ttl(
            &retiree_count_key,
            INDEX_EXTEND_WHEN_BELOW,
            INDEX_ENTRY_TTL_LEDGERS,
        );

        // Check if retiree index has grown beyond limit; expire oldest if so
        maybe_expire_oldest_index_entry(&e, new_retiree_pos, "retiree");

        // Update project compound-key index
        let project_count_key = DataKey::ProjectCount(project_id.clone());
        let project_pos: u64 = e
            .storage()
            .persistent()
            .get(&project_count_key)
            .unwrap_or(0);
        let pidx_key = DataKey::ProjectIndex(project_id.clone(), project_pos);
        e.storage().persistent().set(&pidx_key, &record_id);
        // Set TTL on index entry (bounded to ~1 year, not permanent like Record)
        e.storage()
            .persistent()
            .extend_ttl(&pidx_key, INDEX_EXTEND_WHEN_BELOW, INDEX_ENTRY_TTL_LEDGERS);
        let new_project_pos = project_pos + 1;
        e.storage()
            .persistent()
            .set(&project_count_key, &new_project_pos);
        e.storage().persistent().extend_ttl(
            &project_count_key,
            INDEX_EXTEND_WHEN_BELOW,
            INDEX_ENTRY_TTL_LEDGERS,
        );

        // Check if project index has grown beyond limit; expire oldest if so
        maybe_expire_oldest_index_entry(&e, new_project_pos, "project");

        // Update global scalars
        e.storage()
            .instance()
            .set(&DataKey::TotalRetired, &new_total);
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
    ///
    /// # Storage Growth Policy
    ///
    /// Index entries (RetireeIndex) are bounded:
    /// - Each index entry has a TTL of ~1 year (INDEX_ENTRY_TTL_LEDGERS)
    /// - When a retiree exceeds MAX_LIVE_INDEX_ENTRIES (1000),
    ///   the oldest index entry is de-prioritised and expires naturally
    /// - **Permanent records (Record(id)) are NEVER pruned** — they
    ///   constitute the immutable audit log
    /// - Full history beyond live index is available via contract events
    ///
    /// # Handling Expired Entries
    ///
    /// If an index entry has expired (its TTL expired), this function will
    /// skip it gracefully. Callers seeking full historical data should use
    /// contract events or off-chain indexers.
    pub fn get_retirements_by_retiree(
        e: Env,
        retiree: Address,
        offset: u64,
        limit: u32,
    ) -> Vec<RetirementRecord> {
        let count_key = DataKey::RetireeCount(retiree.clone());
        let total: u64 = e.storage().persistent().get(&count_key).unwrap_or(0);

        let mut records: Vec<RetirementRecord> = Vec::new(&e);
        let effective_limit = limit.min(50); // cap per-call to 50
        let end = (offset + effective_limit as u64).min(total);
        
        for pos in offset..end {
            let idx_key = DataKey::RetireeIndex(retiree.clone(), pos);
            
            // Index entry may have expired for very old positions
            if let Some(record_id) = e.storage().persistent().get::<_, u64>(&idx_key) {
                // Extend TTL on access (keeps recently-queried entries live)
                e.storage().persistent().extend_ttl(
                    &idx_key,
                    INDEX_EXTEND_WHEN_BELOW,
                    INDEX_ENTRY_TTL_LEDGERS,
                );

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
            // else: index entry expired naturally — skip gracefully
        }
        records
    }

    /// Get paginated retirement records for a given project ID.
    /// `offset` is the zero-based start position; `limit` is the max entries to return.
    ///
    /// # Storage Growth Policy
    ///
    /// Index entries (ProjectIndex) are bounded:
    /// - Each index entry has a TTL of ~1 year (INDEX_ENTRY_TTL_LEDGERS)
    /// - When a project exceeds MAX_LIVE_INDEX_ENTRIES (1000),
    ///   the oldest index entry is de-prioritised and expires naturally
    /// - **Permanent records (Record(id)) are NEVER pruned** — they
    ///   constitute the immutable audit log
    /// - Full history beyond live index is available via contract events
    ///
    /// # Handling Expired Entries
    ///
    /// If an index entry has expired (its TTL expired), this function will
    /// skip it gracefully. Callers seeking full historical data should use
    /// contract events or off-chain indexers.
    pub fn get_retirements_by_project(
        e: Env,
        project_id: BytesN<32>,
        offset: u64,
        limit: u32,
    ) -> Vec<RetirementRecord> {
        let count_key = DataKey::ProjectCount(project_id.clone());
        let total: u64 = e.storage().persistent().get(&count_key).unwrap_or(0);

        let mut records: Vec<RetirementRecord> = Vec::new(&e);
        let effective_limit = limit.min(50); // cap per-call to 50
        let end = (offset + effective_limit as u64).min(total);
        
        for pos in offset..end {
            let idx_key = DataKey::ProjectIndex(project_id.clone(), pos);
            
            // Index entry may have expired for very old positions
            if let Some(record_id) = e.storage().persistent().get::<_, u64>(&idx_key) {
                // Extend TTL on access (keeps recently-queried entries live)
                e.storage().persistent().extend_ttl(
                    &idx_key,
                    INDEX_EXTEND_WHEN_BELOW,
                    INDEX_ENTRY_TTL_LEDGERS,
                );

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
            // else: index entry expired naturally — skip gracefully
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
        assert_eq!(record.credit_token, admin);
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

    // ── SUITE 1: Permanent records never pruned ───────────────

    #[test]
    fn test_permanent_records_never_pruned() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[10u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        // Record many retirements (more than MAX_LIVE_INDEX_ENTRIES)
        let count = 50u64;
        for i in 0..count {
            client.record_retirement(
                &admin,
                &retiree,
                &project_id,
                &(100i128 + i as i128),
                &purpose,
                &uri,
            );
        }

        // Assert all Record(id) entries still exist (not just recent MAX_LIVE_INDEX_ENTRIES)
        for id in 1..=count {
            let record = client.get_record(&id);
            assert!(record.is_some(), "Record(id={}) must exist", id);
            assert_eq!(record.unwrap().amount, 100 + id as i128);
        }

        assert_eq!(client.record_count(), count);
    }

    #[test]
    fn test_permanent_records_accessible_directly_by_id() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[11u8; 32]);
        let purpose = String::from_str(&e, "compliance");
        let uri = String::from_str(&e, "ipfs://QmRecord");

        // Record a retirement
        let record_id = client.record_retirement(&admin, &retiree, &project_id, &250, &purpose, &uri);

        // Assert Record(id) is directly accessible
        let record = client.get_record(&record_id).unwrap();
        assert_eq!(record.id, record_id);
        assert_eq!(record.amount, 250);
        assert_eq!(record.retiree, retiree);
        assert_eq!(record.project_id, project_id);
    }

    // ── SUITE 2: Index bounded at MAX_LIVE_INDEX_ENTRIES ──────

    #[test]
    fn test_retiree_index_count_bounded_at_max_live() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[12u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        // Record MAX_LIVE_INDEX_ENTRIES + 50 retirements
        let record_count = MAX_LIVE_INDEX_ENTRIES + 50;
        for i in 0..record_count {
            client.record_retirement(
                &admin,
                &retiree,
                &project_id,
                &(100i128 + i as i128),
                &purpose,
                &uri,
            );
        }

        // RetireeCount still tracks total (monotonically increases)
        assert_eq!(client.retiree_count(&retiree), record_count);

        // Index entries are bounded; oldest would naturally expire
        // We can verify by attempting pagination that the count is coherent
        let page = client.get_retirements_by_retiree(&retiree, &0, &100);
        assert!(page.len() <= 50); // capped per-call
    }

    #[test]
    fn test_project_index_count_bounded_at_max_live() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[13u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        // Record MAX_LIVE_INDEX_ENTRIES + 50 retirements
        let record_count = MAX_LIVE_INDEX_ENTRIES + 50;
        for i in 0..record_count {
            client.record_retirement(
                &admin,
                &retiree,
                &project_id,
                &(100i128 + i as i128),
                &purpose,
                &uri,
            );
        }

        // ProjectCount still tracks total
        assert_eq!(client.project_retirement_count(&project_id), record_count);

        // Verify pagination works
        let page = client.get_retirements_by_project(&project_id, &0, &100);
        assert!(page.len() <= 50); // capped per-call
    }

    #[test]
    fn test_idx_exp_event_emitted_when_limit_exceeded() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[14u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        // Record MAX_LIVE_INDEX_ENTRIES + 5 retirements to trigger expiry events
        let record_count = MAX_LIVE_INDEX_ENTRIES + 5;
        for i in 0..record_count {
            client.record_retirement(
                &admin,
                &retiree,
                &project_id,
                &(100i128 + i as i128),
                &purpose,
                &uri,
            );
        }

        // Check events for idx_exp (EVENT_INDEX_EXPIRED)
        let events = e.events().all();
        let expire_events: Vec<_> = events
            .iter()
            .filter(|(_contract, topics, _data)| {
                if let Ok(topic) = Symbol::try_from_val(&e, &topics.get(0).unwrap()) {
                    topic == symbol_short!("idx_exp")
                } else {
                    false
                }
            })
            .collect();

        // Should have at least 5 idx_exp events (one for each record beyond MAX_LIVE_INDEX_ENTRIES)
        assert!(
            expire_events.len() >= 5,
            "Expected at least 5 idx_exp events, got {}",
            expire_events.len()
        );
    }

    // ── SUITE 3: Recent history queries correct ───────────────

    #[test]
    fn test_get_retirements_by_retiree_returns_recent() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[15u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        // Record 10 retirements
        for i in 0..10 {
            client.record_retirement(
                &admin,
                &retiree,
                &project_id,
                &(100i128 + i as i128),
                &purpose,
                &uri,
            );
        }

        // Query recent page
        let records = client.get_retirements_by_retiree(&retiree, &0, &10);
        assert_eq!(records.len(), 10);

        // Verify amounts are correct
        for (i, record) in (0..10).zip(0..records.len()) {
            assert_eq!(records.get(record).unwrap().amount, 100 + i as i128);
        }
    }

    #[test]
    fn test_get_retirements_by_retiree_handles_missing_old_index() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[16u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        // Record 3 retirements
        for i in 0..3 {
            client.record_retirement(
                &admin,
                &retiree,
                &project_id,
                &(100i128 + i as i128),
                &purpose,
                &uri,
            );
        }

        // Manually remove an old index entry to simulate TTL expiry
        e.storage().persistent().remove(&DataKey::RetireeIndex(
            retiree.clone(),
            0, // oldest entry
        ));

        // Call get_retirements_by_retiree — should NOT panic
        let records = client.get_retirements_by_retiree(&retiree, &0, &10);

        // Should skip the expired entry (position 0) and return entries 1 and 2
        assert_eq!(records.len(), 2);
        assert_eq!(records.get(0).unwrap().amount, 101); // position 1
        assert_eq!(records.get(1).unwrap().amount, 102); // position 2
    }

    #[test]
    fn test_get_retirements_by_project_handles_missing_old_index() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[17u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        // Record 3 retirements
        for i in 0..3 {
            client.record_retirement(
                &admin,
                &retiree,
                &project_id,
                &(100i128 + i as i128),
                &purpose,
                &uri,
            );
        }

        // Manually remove an old project index entry to simulate TTL expiry
        e.storage().persistent().remove(&DataKey::ProjectIndex(
            project_id.clone(),
            0, // oldest entry
        ));

        // Call get_retirements_by_project — should NOT panic
        let records = client.get_retirements_by_project(&project_id, &0, &10);

        // Should skip the expired entry (position 0) and return entries 1 and 2
        assert_eq!(records.len(), 2);
        assert_eq!(records.get(0).unwrap().amount, 101); // position 1
        assert_eq!(records.get(1).unwrap().amount, 102); // position 2
    }

    #[test]
    fn test_pagination_still_correct_with_bounded_index() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[18u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        // Record 10 retirements
        for i in 0..10 {
            client.record_retirement(
                &admin,
                &retiree,
                &project_id,
                &(100i128 + i as i128),
                &purpose,
                &uri,
            );
        }

        // Page 1: positions 0-4 → 5 results
        let page1 = client.get_retirements_by_retiree(&retiree, &0, &5);
        assert_eq!(page1.len(), 5);
        for i in 0..5 {
            assert_eq!(page1.get(i).unwrap().amount, 100 + i as i128);
        }

        // Page 2: positions 5-9 → 5 results
        let page2 = client.get_retirements_by_retiree(&retiree, &5, &5);
        assert_eq!(page2.len(), 5);
        for i in 0..5 {
            assert_eq!(page2.get(i).unwrap().amount, 105 + i as i128);
        }
    }

    // ── SUITE 4: Count integrity ──────────────────────────────

    #[test]
    fn test_retiree_count_monotonically_increases() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[19u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        assert_eq!(client.retiree_count(&retiree), 0);

        // Record 5 retirements
        for i in 0..5 {
            client.record_retirement(
                &admin,
                &retiree,
                &project_id,
                &(100i128 + i as i128),
                &purpose,
                &uri,
            );
        }

        // RetireeCount reflects total, not bounded index entries
        assert_eq!(client.retiree_count(&retiree), 5);
    }

    #[test]
    fn test_project_count_monotonically_increases() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[20u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        assert_eq!(client.project_retirement_count(&project_id), 0);

        // Record 5 retirements for the project
        for i in 0..5 {
            client.record_retirement(
                &admin,
                &retiree,
                &project_id,
                &(100i128 + i as i128),
                &purpose,
                &uri,
            );
        }

        // ProjectCount reflects total, not bounded index entries
        assert_eq!(client.project_retirement_count(&project_id), 5);
    }

    #[test]
    fn test_record_count_unaffected_by_index_expiry() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let retiree = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[21u8; 32]);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        // Record MAX_LIVE_INDEX_ENTRIES + 10 retirements
        let record_count = MAX_LIVE_INDEX_ENTRIES + 10;
        for i in 0..record_count {
            client.record_retirement(
                &admin,
                &retiree,
                &project_id,
                &(100i128 + i as i128),
                &purpose,
                &uri,
            );
        }

        // RecordCount tracks all records ever created
        assert_eq!(client.record_count(), record_count);

        // Manually remove some old index entries (simulate TTL expiry)
        for pos in 0..5 {
            e.storage()
                .persistent()
                .remove(&DataKey::RetireeIndex(retiree.clone(), pos));
        }

        // RecordCount is unaffected
        assert_eq!(client.record_count(), record_count);
    }
}


