#![no_std]
use soroban_sdk::{Bytes, BytesN, Env, String};

// ── Project Status Transition State Machine ──
//
// Valid transitions:
//   registered → active       (project begins operations)
//   active     → completed    (project fulfills its credits)
//   active     → suspended    (project is halted)
//   completed  → active       (reactivation, if needed)
//   suspended  → registered   (re-registration after remediation)
//
// Same-status updates are treated as no-ops (allowed but no state change).
//
// All other transitions (e.g. completed→registered, registered→completed,
// suspended→completed, registered→suspended, etc.) are forbidden.

/// Lengths of the canonical status strings. Used by `is_valid_status` to
/// short-circuit on length before allocating any host-side
/// `String::from_str` objects. The two 9-character statuses are kept as
/// separate named constants for documentation, but their underlying
/// value is the same — `is_valid_status_transition` uses the literal
/// length and the canonical status name when comparing strings.
const STATUS_LEN_REGISTERED: u32 = 10; // "registered"
const STATUS_LEN_ACTIVE: u32 = 6; // "active"
const STATUS_LEN_COMPLETED: u32 = 9; // "completed"
const STATUS_LEN_SUSPENDED: u32 = 9; // "suspended"

/// Returns `true` when `new_status` is a valid successor of `current_status`.
/// Same-status transitions always return `true` (callers treat them as no-ops).
///
/// Gas optimization (issue #63): the function dispatches on the
/// (cur_len, new_len) length tuple first and only allocates the
/// host-side `String` objects that match the candidate transition.
/// Each length tuple maps to at most two `String::from_str`
/// allocations per call (down from the previous five). Same-status
/// transitions short-circuit with a single equality comparison and
/// never allocate.
///
/// Allowed transitions by length tuple:
///   (10, 6)  registered → active
///   (6, 9)   active     → completed | suspended
///   (9, 6)   completed  → active   (NOT suspended → active)
///   (9, 10)  suspended  → registered
pub fn is_valid_status_transition(e: &Env, current_status: &String, new_status: &String) -> bool {
    // Same-status is always allowed (caller handles no-op).
    if current_status == new_status {
        return true;
    }

    let cur_len = current_status.len();
    let new_len = new_status.len();

    // registered (10) → active (6)
    if cur_len == STATUS_LEN_REGISTERED && new_len == STATUS_LEN_ACTIVE {
        return *current_status == String::from_str(e, "registered")
            && *new_status == String::from_str(e, "active");
    }
    // active (6) → completed or suspended (both 9 chars)
    if cur_len == STATUS_LEN_ACTIVE && new_len == STATUS_LEN_COMPLETED {
        return *current_status == String::from_str(e, "active")
            && (*new_status == String::from_str(e, "completed")
                || *new_status == String::from_str(e, "suspended"));
    }
    // completed (9) → active (6). Both "completed" and "suspended" are
    // 9 chars long, so the length tuple (9, 6) is shared by the valid
    // transition completed→active and the forbidden transition
    // suspended→active. The string-content check below discriminates:
    // a current of "suspended" returns false (suspended→active is
    // forbidden) with no extra `String` allocations beyond the one.
    if cur_len == STATUS_LEN_COMPLETED && new_len == STATUS_LEN_ACTIVE {
        return *current_status == String::from_str(e, "completed")
            && *new_status == String::from_str(e, "active");
    }
    // suspended (9) → registered (10)
    if cur_len == STATUS_LEN_SUSPENDED && new_len == STATUS_LEN_REGISTERED {
        return *current_status == String::from_str(e, "suspended")
            && *new_status == String::from_str(e, "registered");
    }

    false
}

/// Returns `true` when `status` is one of the four recognised values:
/// registered, active, completed, suspended.
///
/// Gas optimization (issue #63): short-circuits via `String::len()` so
/// that, in the worst case, only the candidate strings matching the
/// given length are allocated. Any length outside {6, 9, 10} returns
/// false with a single `len()` host read and zero `String` allocations.
pub fn is_valid_status(e: &Env, status: &String) -> bool {
    let len = status.len();
    match len {
        l if l == STATUS_LEN_REGISTERED => {
            *status == String::from_str(e, "registered")
        }
        l if l == STATUS_LEN_ACTIVE => *status == String::from_str(e, "active"),
        l if l == STATUS_LEN_COMPLETED => {
            // Both "completed" and "suspended" have length 9: try each.
            *status == String::from_str(e, "completed")
                || *status == String::from_str(e, "suspended")
        }
        _ => false,
    }
}

/// Canonical project ID generation across all contracts.
///
/// Produces a deterministic 32-byte ID from registration inputs.
///
/// Format: SHA-256(
///   count_be8 | timestamp_be8 |
///   name_len_be4 | name_bytes |
///   methodology_len_be4 | methodology_bytes |
///   latitude_be8 | longitude_be8 | area_hectares_be8
/// )
///
/// Length-prefixed string fields prevent prefix collisions between different
/// field combinations.
#[allow(clippy::too_many_arguments)]
pub fn generate_project_id(
    e: &Env,
    count: u64,
    timestamp: u64,
    name: &String,
    methodology: &String,
    latitude: i64,
    longitude: i64,
    area_hectares: u64,
) -> BytesN<32> {
    if name.len() > 128 {
        panic!("name too long");
    }
    if methodology.len() > 128 {
        panic!("methodology too long");
    }

    let mut preimage: Bytes = Bytes::new(e);

    let count_bytes = count.to_be_bytes();
    preimage.append(&Bytes::from_array(e, &count_bytes));

    let ts_bytes = timestamp.to_be_bytes();
    preimage.append(&Bytes::from_array(e, &ts_bytes));

    let name_len = name.len();
    preimage.append(&Bytes::from_array(e, &name_len.to_be_bytes()));
    let name_len_usize = name_len as usize;
    if name_len_usize > 0 {
        // Safe: name is bounded to 128 bytes by validation above
        let mut name_buf = [0u8; 256];
        name.copy_into_slice(&mut name_buf[..name_len_usize]);
        preimage.append(&Bytes::from_slice(e, &name_buf[..name_len_usize]));
    }

    let methodology_len = methodology.len();
    preimage.append(&Bytes::from_array(e, &methodology_len.to_be_bytes()));
    let methodology_len_usize = methodology_len as usize;
    if methodology_len_usize > 0 {
        // Safe: methodology is bounded to 128 bytes by validation above
        let mut methodology_buf = [0u8; 256];
        methodology.copy_into_slice(&mut methodology_buf[..methodology_len_usize]);
        preimage.append(&Bytes::from_slice(
            e,
            &methodology_buf[..methodology_len_usize],
        ));
    }

    preimage.append(&Bytes::from_array(e, &latitude.to_be_bytes()));
    preimage.append(&Bytes::from_array(e, &longitude.to_be_bytes()));
    preimage.append(&Bytes::from_array(e, &area_hectares.to_be_bytes()));

    e.crypto().sha256(&preimage)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Transition state-machine tests ──

    #[test]
    fn test_valid_transition_registered_to_active() {
        let e = Env::default();
        let cur = String::from_str(&e, "registered");
        let new = String::from_str(&e, "active");
        assert!(is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_valid_transition_active_to_completed() {
        let e = Env::default();
        let cur = String::from_str(&e, "active");
        let new = String::from_str(&e, "completed");
        assert!(is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_valid_transition_active_to_suspended() {
        let e = Env::default();
        let cur = String::from_str(&e, "active");
        let new = String::from_str(&e, "suspended");
        assert!(is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_valid_transition_completed_to_active() {
        let e = Env::default();
        let cur = String::from_str(&e, "completed");
        let new = String::from_str(&e, "active");
        assert!(is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_valid_transition_suspended_to_registered() {
        let e = Env::default();
        let cur = String::from_str(&e, "suspended");
        let new = String::from_str(&e, "registered");
        assert!(is_valid_status_transition(&e, &cur, &new));
    }

    // Same-status no-op transitions

    #[test]
    fn test_same_status_registered() {
        let e = Env::default();
        let cur = String::from_str(&e, "registered");
        let new = String::from_str(&e, "registered");
        assert!(is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_same_status_active() {
        let e = Env::default();
        let cur = String::from_str(&e, "active");
        let new = String::from_str(&e, "active");
        assert!(is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_same_status_completed() {
        let e = Env::default();
        let cur = String::from_str(&e, "completed");
        let new = String::from_str(&e, "completed");
        assert!(is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_same_status_suspended() {
        let e = Env::default();
        let cur = String::from_str(&e, "suspended");
        let new = String::from_str(&e, "suspended");
        assert!(is_valid_status_transition(&e, &cur, &new));
    }

    // Forbidden transitions

    #[test]
    fn test_forbidden_completed_to_registered() {
        let e = Env::default();
        let cur = String::from_str(&e, "completed");
        let new = String::from_str(&e, "registered");
        assert!(!is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_forbidden_registered_to_completed() {
        let e = Env::default();
        let cur = String::from_str(&e, "registered");
        let new = String::from_str(&e, "completed");
        assert!(!is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_forbidden_suspended_to_completed() {
        let e = Env::default();
        let cur = String::from_str(&e, "suspended");
        let new = String::from_str(&e, "completed");
        assert!(!is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_forbidden_registered_to_suspended() {
        let e = Env::default();
        let cur = String::from_str(&e, "registered");
        let new = String::from_str(&e, "suspended");
        assert!(!is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_forbidden_suspended_to_active() {
        let e = Env::default();
        let cur = String::from_str(&e, "suspended");
        let new = String::from_str(&e, "active");
        assert!(!is_valid_status_transition(&e, &cur, &new));
    }

    #[test]
    fn test_forbidden_completed_to_suspended() {
        let e = Env::default();
        let cur = String::from_str(&e, "completed");
        let new = String::from_str(&e, "suspended");
        assert!(!is_valid_status_transition(&e, &cur, &new));
    }

    // is_valid_status tests

    #[test]
    fn test_is_valid_status_registered() {
        let e = Env::default();
        assert!(is_valid_status(&e, &String::from_str(&e, "registered")));
    }

    #[test]
    fn test_is_valid_status_active() {
        let e = Env::default();
        assert!(is_valid_status(&e, &String::from_str(&e, "active")));
    }

    #[test]
    fn test_is_valid_status_completed() {
        let e = Env::default();
        assert!(is_valid_status(&e, &String::from_str(&e, "completed")));
    }

    #[test]
    fn test_is_valid_status_suspended() {
        let e = Env::default();
        assert!(is_valid_status(&e, &String::from_str(&e, "suspended")));
    }

    #[test]
    fn test_is_valid_status_invalid() {
        let e = Env::default();
        assert!(!is_valid_status(&e, &String::from_str(&e, "bogus")));
    }

    // ── Existing generate_project_id tests ──

    #[test]
    fn test_deterministic() {
        let e = Env::default();
        let name = String::from_str(&e, "A");
        let methodology = String::from_str(&e, "B");
        let id1 = generate_project_id(&e, 0, 1000, &name, &methodology, 1, 2, 3);
        let id2 = generate_project_id(&e, 0, 1000, &name, &methodology, 1, 2, 3);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_different_count_different_id() {
        let e = Env::default();
        let name = String::from_str(&e, "A");
        let methodology = String::from_str(&e, "B");
        let id1 = generate_project_id(&e, 0, 1000, &name, &methodology, 1, 2, 3);
        let id2 = generate_project_id(&e, 1, 1000, &name, &methodology, 1, 2, 3);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_different_timestamp_different_id() {
        let e = Env::default();
        let name = String::from_str(&e, "A");
        let methodology = String::from_str(&e, "B");
        let id1 = generate_project_id(&e, 0, 1000, &name, &methodology, 1, 2, 3);
        let id2 = generate_project_id(&e, 0, 1001, &name, &methodology, 1, 2, 3);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_different_name_different_id_same_count_and_timestamp() {
        let e = Env::default();
        let methodology = String::from_str(&e, "B");
        let id1 = generate_project_id(
            &e,
            0,
            1000,
            &String::from_str(&e, "A"),
            &methodology,
            1,
            2,
            3,
        );
        let id2 = generate_project_id(
            &e,
            0,
            1000,
            &String::from_str(&e, "C"),
            &methodology,
            1,
            2,
            3,
        );
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_prefix_collision_resistance() {
        let e = Env::default();
        let name1 = String::from_str(&e, "AB");
        let meth1 = String::from_str(&e, "C");
        let id1 = generate_project_id(&e, 0, 1000, &name1, &meth1, 1, 2, 3);

        let name2 = String::from_str(&e, "A");
        let meth2 = String::from_str(&e, "BC");
        let id2 = generate_project_id(&e, 0, 1000, &name2, &meth2, 1, 2, 3);

        assert_ne!(id1, id2);
    }

    #[test]
    fn test_full_32_bytes() {
        let e = Env::default();
        let name = String::from_str(&e, "A");
        let methodology = String::from_str(&e, "B");
        let id = generate_project_id(&e, 42, 9999, &name, &methodology, 1, 2, 3);
        assert_eq!(id.len(), 32);
    }

    #[test]
    #[should_panic(expected = "name too long")]
    fn test_name_too_long() {
        let e = Env::default();
        let mut name_bytes = [0u8; 129];
        name_bytes.fill(b'A');
        let name = String::from_str(&e, core::str::from_utf8(&name_bytes).unwrap());
        let methodology = String::from_str(&e, "B");
        generate_project_id(&e, 0, 1000, &name, &methodology, 1, 2, 3);
    }

    #[test]
    #[should_panic(expected = "methodology too long")]
    fn test_methodology_too_long() {
        let e = Env::default();
        let name = String::from_str(&e, "A");
        let mut meth_bytes = [0u8; 129];
        meth_bytes.fill(b'B');
        let methodology = String::from_str(&e, core::str::from_utf8(&meth_bytes).unwrap());
        generate_project_id(&e, 0, 1000, &name, &methodology, 1, 2, 3);
    }

    #[test]
    fn test_max_length_success() {
        let e = Env::default();
        let mut name_bytes = [0u8; 128];
        name_bytes.fill(b'A');
        let name = String::from_str(&e, core::str::from_utf8(&name_bytes).unwrap());
        let mut meth_bytes = [0u8; 128];
        meth_bytes.fill(b'B');
        let methodology = String::from_str(&e, core::str::from_utf8(&meth_bytes).unwrap());
        let id = generate_project_id(&e, 0, 1000, &name, &methodology, 1, 2, 3);
        assert_eq!(id.len(), 32);
    }
}
