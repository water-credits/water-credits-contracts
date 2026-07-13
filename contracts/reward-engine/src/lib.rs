#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Env, Symbol, Vec};

#[cfg(test)]
extern crate std;

const EVENT_VERIFICATION_PUSHED: Symbol = symbol_short!("vw_pushed");
const EVENT_VERIFICATION_RESOLVED: Symbol = symbol_short!("vw_resved");

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VerificationKey {
    pub task_id: u64,
    pub user: Address,
}

#[contracttype]
pub enum DataKey {
    Admin,
    PendingList,
    PendingCount,
    ResolvedList,
    ResolvedCount,
    MaxArchiveSize,
}

fn has_admin(e: &Env) -> bool {
    e.storage().instance().has(&DataKey::Admin)
}

fn read_admin(e: &Env) -> Address {
    e.storage().instance().get(&DataKey::Admin).unwrap()
}

fn read_pending_list(e: &Env) -> Vec<VerificationKey> {
    e.storage()
        .instance()
        .get(&DataKey::PendingList)
        .unwrap_or_else(|| Vec::new(e))
}

fn save_pending_list(e: &Env, list: &Vec<VerificationKey>) {
    e.storage()
        .instance()
        .set(&DataKey::PendingList, list);
}

fn read_pending_count(e: &Env) -> u32 {
    e.storage()
        .instance()
        .get(&DataKey::PendingCount)
        .unwrap_or(0)
}

fn save_pending_count(e: &Env, count: u32) {
    e.storage()
        .instance()
        .set(&DataKey::PendingCount, &count);
}

fn read_resolved_list(e: &Env) -> Vec<VerificationKey> {
    e.storage()
        .instance()
        .get(&DataKey::ResolvedList)
        .unwrap_or_else(|| Vec::new(e))
}

fn save_resolved_list(e: &Env, list: &Vec<VerificationKey>) {
    e.storage()
        .instance()
        .set(&DataKey::ResolvedList, list);
}

fn read_resolved_count(e: &Env) -> u64 {
    e.storage()
        .instance()
        .get(&DataKey::ResolvedCount)
        .unwrap_or(0)
}

fn save_resolved_count(e: &Env, count: u64) {
    e.storage()
        .instance()
        .set(&DataKey::ResolvedCount, &count);
}

fn read_max_archive_size(e: &Env) -> u64 {
    e.storage()
        .instance()
        .get(&DataKey::MaxArchiveSize)
        .unwrap_or(1000)
}

fn find_pending_index(list: &Vec<VerificationKey>, task_id: u64, user: &Address) -> Option<u32> {
    for i in 0..list.len() {
        let vk = list.get(i).unwrap();
        if vk.task_id == task_id && vk.user == *user {
            return Some(i);
        }
    }
    None
}

#[contract]
pub struct RewardEngine;

#[contractimpl]
impl RewardEngine {
    /// Initialize the reward engine. Callable once.
    pub fn initialize(e: Env, admin: Address, max_archive_size: u64) {
        if has_admin(&e) {
            panic!("already initialized");
        }
        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage()
            .instance()
            .set(&DataKey::PendingList, &Vec::<VerificationKey>::new(&e));
        e.storage().instance().set(&DataKey::PendingCount, &0u32);
        e.storage()
            .instance()
            .set(&DataKey::ResolvedList, &Vec::<VerificationKey>::new(&e));
        e.storage().instance().set(&DataKey::ResolvedCount, &0u64);
        e.storage()
            .instance()
            .set(&DataKey::MaxArchiveSize, &max_archive_size);
    }

    /// Submit a verification key for a task. The user authorizes their own submission.
    /// Panics if the same (task_id, user) pair is already pending.
    pub fn push_verification_key(e: Env, task_id: u64, user: Address) {
        user.require_auth();

        let list = read_pending_list(&e);
        if find_pending_index(&list, task_id, &user).is_some() {
            panic!("verification already pending");
        }

        let mut list = list;
        list.push_back(VerificationKey {
            task_id,
            user: user.clone(),
        });
        save_pending_list(&e, &list);

        let count = read_pending_count(&e);
        save_pending_count(&e, count + 1);

        e.events()
            .publish((EVENT_VERIFICATION_PUSHED,), (task_id, user));
    }

    /// Resolve a pending verification. Admin only.
    /// Removes from the pending list (swap-and-pop) and appends to the resolved archive.
    /// The archive is bounded by max_archive_size; oldest entries are dropped when full.
    pub fn resolve_verification(e: Env, admin: Address, task_id: u64, user: Address) {
        admin.require_auth();
        if admin != read_admin(&e) {
            panic!("unauthorized");
        }

        let mut list = read_pending_list(&e);
        let idx = find_pending_index(&list, task_id, &user);
        let idx = match idx {
            Some(i) => i,
            None => panic!("verification not pending"),
        };

        let last_idx = list.len() - 1;
        if idx != last_idx {
            let last = list.get(last_idx).unwrap();
            list.set(idx, last);
        }
        list.pop_back();
        save_pending_list(&e, &list);

        let count = read_pending_count(&e);
        save_pending_count(&e, count - 1);

        let vk = VerificationKey {
            task_id,
            user: user.clone(),
        };

        let max = read_max_archive_size(&e);
        let mut resolved = read_resolved_list(&e);
        if resolved.len() as u64 >= max {
            resolved.remove(0);
        }
        resolved.push_back(vk);
        save_resolved_list(&e, &resolved);

        let resolved_count = read_resolved_count(&e);
        save_resolved_count(&e, resolved_count + 1);

        e.events()
            .publish((EVENT_VERIFICATION_RESOLVED,), (task_id, user));
    }

    /// Check whether a specific (task_id, user) verification is currently pending.
    pub fn is_pending(e: Env, task_id: u64, user: Address) -> bool {
        let list = read_pending_list(&e);
        find_pending_index(&list, task_id, &user).is_some()
    }

    /// Get the full list of pending verifications.
    /// Only iterates actual pending items (pruned on resolve), not historical entries.
    pub fn get_pending_verifications(e: Env) -> Vec<VerificationKey> {
        read_pending_list(&e)
    }

    /// Get the number of currently pending verifications.
    pub fn pending_count(e: Env) -> u32 {
        read_pending_count(&e)
    }

    /// Get the resolved verification archive (bounded by max_archive_size).
    pub fn get_resolved_archive(e: Env) -> Vec<VerificationKey> {
        read_resolved_list(&e)
    }

    /// Get the total number of verifications ever resolved.
    pub fn resolved_count(e: Env) -> u64 {
        read_resolved_count(&e)
    }

    /// Get the configured maximum archive size.
    pub fn max_archive_size(e: Env) -> u64 {
        read_max_archive_size(&e)
    }

    /// Update the maximum archive size. Admin only.
    pub fn set_max_archive_size(e: Env, admin: Address, max: u64) {
        admin.require_auth();
        if admin != read_admin(&e) {
            panic!("unauthorized");
        }
        e.storage()
            .instance()
            .set(&DataKey::MaxArchiveSize, &max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (Env, Address, RewardEngineClient<'static>) {
        let e = Env::default();
        let admin = Address::generate(&e);
        let contract_id = e.register_contract(None, RewardEngine);
        let client = RewardEngineClient::new(&e, &contract_id);
        client.initialize(&admin, &5);
        (e, admin, client)
    }

    fn setup_large_archive() -> (Env, Address, RewardEngineClient<'static>) {
        let e = Env::default();
        let admin = Address::generate(&e);
        let contract_id = e.register_contract(None, RewardEngine);
        let client = RewardEngineClient::new(&e, &contract_id);
        client.initialize(&admin, &3);
        (e, admin, client)
    }

    #[test]
    fn test_initialize_sets_defaults() {
        let (_e, admin, client) = setup();
        assert_eq!(client.pending_count(), 0);
        assert_eq!(client.resolved_count(), 0);
        assert_eq!(client.max_archive_size(), 5);
        assert_eq!(client.get_pending_verifications().len(), 0);
        assert_eq!(client.get_resolved_archive().len(), 0);
    }

    #[test]
    fn test_initialize_cannot_be_called_twice() {
        let (e, admin, client) = setup();
        let result = std::panic::catch_unwind(|| {
            client.initialize(&admin, &10);
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_push_verification_key_adds_to_pending() {
        let (e, _admin, client) = setup();
        e.mock_all_auths();

        let user = Address::generate(&e);
        client.push_verification_key(&1, &user);

        assert_eq!(client.pending_count(), 1);
        assert!(client.is_pending(&1, &user));
        let pending = client.get_pending_verifications();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending.get(0).unwrap().task_id, 1);
        assert_eq!(pending.get(0).unwrap().user, user);
    }

    #[test]
    fn test_push_multiple_distinct_verifications() {
        let (e, _admin, client) = setup();
        e.mock_all_auths();

        let user1 = Address::generate(&e);
        let user2 = Address::generate(&e);
        client.push_verification_key(&1, &user1);
        client.push_verification_key(&2, &user2);

        assert_eq!(client.pending_count(), 2);
        assert!(client.is_pending(&1, &user1));
        assert!(client.is_pending(&2, &user2));
    }

    #[test]
    fn test_push_duplicate_panics() {
        let (e, _admin, client) = setup();
        e.mock_all_auths();

        let user = Address::generate(&e);
        client.push_verification_key(&1, &user);

        let result = std::panic::catch_unwind(|| {
            client.push_verification_key(&1, &user);
        });
        assert!(result.is_err());
        assert_eq!(client.pending_count(), 1);
    }

    #[test]
    fn test_same_task_different_users_allowed() {
        let (e, _admin, client) = setup();
        e.mock_all_auths();

        let user1 = Address::generate(&e);
        let user2 = Address::generate(&e);
        client.push_verification_key(&1, &user1);
        client.push_verification_key(&1, &user2);

        assert_eq!(client.pending_count(), 2);
        assert!(client.is_pending(&1, &user1));
        assert!(client.is_pending(&1, &user2));
    }

    #[test]
    fn test_resolve_removes_from_pending() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let user = Address::generate(&e);
        client.push_verification_key(&1, &user);
        assert_eq!(client.pending_count(), 1);

        client.resolve_verification(&admin, &1, &user);
        assert_eq!(client.pending_count(), 0);
        assert!(!client.is_pending(&1, &user));
        assert_eq!(client.get_pending_verifications().len(), 0);
    }

    #[test]
    fn test_resolve_adds_to_archive() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let user = Address::generate(&e);
        client.push_verification_key(&1, &user);
        client.resolve_verification(&admin, &1, &user);

        assert_eq!(client.resolved_count(), 1);
        let archive = client.get_resolved_archive();
        assert_eq!(archive.len(), 1);
        assert_eq!(archive.get(0).unwrap().task_id, 1);
        assert_eq!(archive.get(0).unwrap().user, user);
    }

    #[test]
    fn test_resolve_non_pending_panics() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let user = Address::generate(&e);
        let result = std::panic::catch_unwind(|| {
            client.resolve_verification(&admin, &1, &user);
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_only_admin() {
        let (e, _admin, client) = setup();
        e.mock_all_auths();

        let user = Address::generate(&e);
        let non_admin = Address::generate(&e);
        client.push_verification_key(&1, &user);

        let result = std::panic::catch_unwind(|| {
            client.resolve_verification(&non_admin, &1, &user);
        });
        assert!(result.is_err());
        assert_eq!(client.pending_count(), 1);
    }

    #[test]
    fn test_resolve_swap_and_pop_order() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let user1 = Address::generate(&e);
        let user2 = Address::generate(&e);
        let user3 = Address::generate(&e);

        client.push_verification_key(&1, &user1);
        client.push_verification_key(&2, &user2);
        client.push_verification_key(&3, &user3);

        // Resolve the middle one
        client.resolve_verification(&admin, &2, &user2);
        assert_eq!(client.pending_count(), 2);

        let pending = client.get_pending_verifications();
        assert_eq!(pending.len(), 2);
        // After swap-and-pop, user3 should have moved to index 1
        let ids: Vec<u64> = Vec::from_array(&e, [
            pending.get(0).unwrap().task_id,
            pending.get(1).unwrap().task_id,
        ]);
        assert!(ids.contains(&1));
        assert!(ids.contains(&3));
    }

    #[test]
    fn test_archive_evicts_oldest_when_full() {
        let (e, admin, client) = setup_large_archive(); // max_archive_size = 3
        e.mock_all_auths();

        let users: Vec<Address> = (0..5).map(|_| Address::generate(&e)).collect();

        // Fill archive to capacity
        for i in 0..3u64 {
            client.push_verification_key(&i, &users.get(i as usize).unwrap());
            client.resolve_verification(&admin, &i, &users.get(i as usize).unwrap());
        }
        assert_eq!(client.resolved_count(), 3);
        assert_eq!(client.get_resolved_archive().len(), 3);

        // Push one more that exceeds capacity
        client.push_verification_key(&3, &users.get(3).unwrap());
        client.resolve_verification(&admin, &3, &users.get(3).unwrap());

        // Archive should still be bounded at 3, oldest evicted
        assert_eq!(client.get_resolved_archive().len(), 3);
        assert_eq!(client.resolved_count(), 4);

        // The archive should contain entries 1, 2, 3 (entry 0 was evicted)
        let archive = client.get_resolved_archive();
        assert_eq!(archive.get(0).unwrap().task_id, 1);
        assert_eq!(archive.get(1).unwrap().task_id, 2);
        assert_eq!(archive.get(2).unwrap().task_id, 3);
    }

    #[test]
    fn test_resolve_first_element() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let user1 = Address::generate(&e);
        let user2 = Address::generate(&e);
        client.push_verification_key(&1, &user1);
        client.push_verification_key(&2, &user2);

        // Resolve the first element
        client.resolve_verification(&admin, &1, &user1);
        assert_eq!(client.pending_count(), 1);
        assert!(!client.is_pending(&1, &user1));
        assert!(client.is_pending(&2, &user2));
    }

    #[test]
    fn test_resolve_last_element() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let user1 = Address::generate(&e);
        let user2 = Address::generate(&e);
        client.push_verification_key(&1, &user1);
        client.push_verification_key(&2, &user2);

        // Resolve the last element
        client.resolve_verification(&admin, &2, &user2);
        assert_eq!(client.pending_count(), 1);
        assert!(client.is_pending(&1, &user1));
        assert!(!client.is_pending(&2, &user2));
    }

    #[test]
    fn test_resolve_only_element() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let user = Address::generate(&e);
        client.push_verification_key(&1, &user);
        client.resolve_verification(&admin, &1, &user);

        assert_eq!(client.pending_count(), 0);
        assert_eq!(client.get_pending_verifications().len(), 0);
        assert_eq!(client.resolved_count(), 1);
    }

    #[test]
    fn test_set_max_archive_size() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        client.set_max_archive_size(&admin, &500);
        assert_eq!(client.max_archive_size(), 500);
    }

    #[test]
    fn test_set_max_archive_size_requires_admin() {
        let (e, _admin, client) = setup();
        e.mock_all_auths();

        let non_admin = Address::generate(&e);
        let result = std::panic::catch_unwind(|| {
            client.set_max_archive_size(&non_admin, &500);
        });
        assert!(result.is_err());
        assert_eq!(client.max_archive_size(), 5);
    }

    #[test]
    fn test_pending_count_tracks_across_operations() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let user1 = Address::generate(&e);
        let user2 = Address::generate(&e);
        let user3 = Address::generate(&e);

        assert_eq!(client.pending_count(), 0);

        client.push_verification_key(&1, &user1);
        assert_eq!(client.pending_count(), 1);

        client.push_verification_key(&2, &user2);
        assert_eq!(client.pending_count(), 2);

        client.push_verification_key(&3, &user3);
        assert_eq!(client.pending_count(), 3);

        client.resolve_verification(&admin, &2, &user2);
        assert_eq!(client.pending_count(), 2);

        client.resolve_verification(&admin, &1, &user1);
        assert_eq!(client.pending_count(), 1);

        client.resolve_verification(&admin, &3, &user3);
        assert_eq!(client.pending_count(), 0);
    }

    #[test]
    fn test_resolved_count_never_decreases() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let user1 = Address::generate(&e);
        let user2 = Address::generate(&e);

        assert_eq!(client.resolved_count(), 0);

        client.push_verification_key(&1, &user1);
        client.resolve_verification(&admin, &1, &user1);
        assert_eq!(client.resolved_count(), 1);

        client.push_verification_key(&2, &user2);
        client.resolve_verification(&admin, &2, &user2);
        assert_eq!(client.resolved_count(), 2);
    }

    #[test]
    fn test_is_pending_returns_false_after_resolve() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let user = Address::generate(&e);
        client.push_verification_key(&1, &user);
        assert!(client.is_pending(&1, &user));

        client.resolve_verification(&admin, &1, &user);
        assert!(!client.is_pending(&1, &user));
    }

    #[test]
    fn test_is_pending_returns_false_for_unknown() {
        let (e, _admin, client) = setup();
        e.mock_all_auths();

        let user = Address::generate(&e);
        assert!(!client.is_pending(&999, &user));
    }

    #[test]
    fn test_archive_preserves_order_of_resolution() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        let user1 = Address::generate(&e);
        let user2 = Address::generate(&e);
        let user3 = Address::generate(&e);

        client.push_verification_key(&1, &user1);
        client.push_verification_key(&2, &user2);
        client.push_verification_key(&3, &user3);

        client.resolve_verification(&admin, &3, &user3);
        client.resolve_verification(&admin, &1, &user1);
        client.resolve_verification(&admin, &2, &user2);

        let archive = client.get_resolved_archive();
        assert_eq!(archive.len(), 3);
        assert_eq!(archive.get(0).unwrap().task_id, 3);
        assert_eq!(archive.get(1).unwrap().task_id, 1);
        assert_eq!(archive.get(2).unwrap().task_id, 2);
    }

    #[test]
    fn test_archive_zero_max_evicts_everything() {
        let (e, admin, client) = setup();
        e.mock_all_auths();

        // Set max archive to 0
        client.set_max_archive_size(&admin, &0);

        let user = Address::generate(&e);
        client.push_verification_key(&1, &user);
        client.resolve_verification(&admin, &1, &user);

        // Archive should be empty since max is 0
        assert_eq!(client.get_resolved_archive().len(), 0);
        // But total count is still tracked
        assert_eq!(client.resolved_count(), 1);
    }
}
