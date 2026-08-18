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

/// Returns `true` when `new_status` is a valid successor of `current_status`.
/// Same-status transitions always return `true` (callers treat them as no-ops).
///
/// Allowed transitions:
///   registered → active
///   active     → completed
///   active     → suspended
///   completed  → active
///   suspended  → registered
pub fn is_valid_status_transition(e: &Env, current_status: &String, new_status: &String) -> bool {
    // Same-status is always allowed (caller handles no-op).
    if current_status == new_status {
        return true;
    }

    let registered = String::from_str(e, "registered");
    let active = String::from_str(e, "active");
    let completed = String::from_str(e, "completed");
    let suspended = String::from_str(e, "suspended");

    // registered → active
    if *current_status == registered && *new_status == active {
        return true;
    }
    // active → completed
    if *current_status == active && *new_status == completed {
        return true;
    }
    // active → suspended
    if *current_status == active && *new_status == suspended {
        return true;
    }
    // completed → active
    if *current_status == completed && *new_status == active {
        return true;
    }
    // suspended → registered
    if *current_status == suspended && *new_status == registered {
        return true;
    }

    false
}

/// Returns `true` when `status` is one of the four recognised values:
/// registered, active, completed, suspended.
pub fn is_valid_status(e: &Env, status: &String) -> bool {
    *status == String::from_str(e, "registered")
        || *status == String::from_str(e, "active")
        || *status == String::from_str(e, "completed")
        || *status == String::from_str(e, "suspended")
}

/// Canonical project ID generation across all contracts.
///
/// Produces a deterministic 32-byte ID from registration inputs only.
///
/// Format: SHA-256(
///   count_be8 |
///   name_len_be4 | name_bytes |
///   methodology_len_be4 | methodology_bytes |
///   latitude_be8 | longitude_be8 | area_hectares_be8
/// )
///
/// Length-prefixed string fields prevent prefix collisions between different
/// field combinations.
///
/// The ledger timestamp is deliberately **not** part of the preimage (Issue
/// #96). Hashing it made the ID depend on which ledger the registration
/// transaction happened to land in, so an off-chain system could not
/// pre-compute the ID before submitting: a one-ledger delay (fee bump,
/// congestion) silently changed the result. `count` — the registering
/// contract's monotonically increasing project counter — supplies the
/// uniqueness the timestamp used to provide, including for re-registrations
/// of otherwise identical project details. Callers still record the
/// registration timestamp in their own project state (`ProjectInfo`
/// `registration_date` / `ProjectEntry` `registered_at`); it is display
/// metadata, not an ID input.
#[allow(clippy::too_many_arguments)]
pub fn generate_project_id(
    e: &Env,
    count: u64,
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

    /// The ID is a pure function of its arguments — the ledger clock is not
    /// one of them (Issue #96). Varying the ledger timestamp requires the
    /// `testutils` `Ledger` trait, which this crate does not depend on, so
    /// that half of the property is asserted through the real contract entry
    /// points in `tests/tests/project_id_determinism.rs`.
    #[test]
    fn test_deterministic() {
        let e = Env::default();
        let name = String::from_str(&e, "A");
        let methodology = String::from_str(&e, "B");
        let id1 = generate_project_id(&e, 0, &name, &methodology, 1, 2, 3);
        let id2 = generate_project_id(&e, 0, &name, &methodology, 1, 2, 3);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_different_count_different_id() {
        let e = Env::default();
        let name = String::from_str(&e, "A");
        let methodology = String::from_str(&e, "B");
        let id1 = generate_project_id(&e, 0, &name, &methodology, 1, 2, 3);
        let id2 = generate_project_id(&e, 1, &name, &methodology, 1, 2, 3);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_different_name_different_id_same_count() {
        let e = Env::default();
        let methodology = String::from_str(&e, "B");
        let id1 = generate_project_id(&e, 0, &String::from_str(&e, "A"), &methodology, 1, 2, 3);
        let id2 = generate_project_id(&e, 0, &String::from_str(&e, "C"), &methodology, 1, 2, 3);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_prefix_collision_resistance() {
        let e = Env::default();
        let name1 = String::from_str(&e, "AB");
        let meth1 = String::from_str(&e, "C");
        let id1 = generate_project_id(&e, 0, &name1, &meth1, 1, 2, 3);

        let name2 = String::from_str(&e, "A");
        let meth2 = String::from_str(&e, "BC");
        let id2 = generate_project_id(&e, 0, &name2, &meth2, 1, 2, 3);

        assert_ne!(id1, id2);
    }

    #[test]
    fn test_full_32_bytes() {
        let e = Env::default();
        let name = String::from_str(&e, "A");
        let methodology = String::from_str(&e, "B");
        let id = generate_project_id(&e, 42, &name, &methodology, 1, 2, 3);
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
        generate_project_id(&e, 0, &name, &methodology, 1, 2, 3);
    }

    #[test]
    #[should_panic(expected = "methodology too long")]
    fn test_methodology_too_long() {
        let e = Env::default();
        let name = String::from_str(&e, "A");
        let mut meth_bytes = [0u8; 129];
        meth_bytes.fill(b'B');
        let methodology = String::from_str(&e, core::str::from_utf8(&meth_bytes).unwrap());
        generate_project_id(&e, 0, &name, &methodology, 1, 2, 3);
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
        let id = generate_project_id(&e, 0, &name, &methodology, 1, 2, 3);
        assert_eq!(id.len(), 32);
    }
}
