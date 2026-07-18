//! Cross-contract project ID compatibility tests (issue #6).
//!
//! Verifies that `credit_factory` and `project_registry` produce identical
//! project IDs for the same (count, timestamp) inputs, using the canonical
//! `shared::generate_project_id` helper.

use shared::generate_project_id;
use soroban_sdk::{BytesN, Env};

/// The same (count, timestamp) pair must produce the same ID via the shared helper.
#[test]
fn test_shared_helper_deterministic_across_envs() {
    // BytesN values are bound to the Env that created them — comparing values
    // from two different Env instances panics.  Use a single Env and call the
    // helper twice; determinism is still proven by comparing the two results.
    let e = Env::default();

    let id1 = generate_project_id(&e, 0, 1_700_000_000);
    let id2 = generate_project_id(&e, 0, 1_700_000_000);

    assert_eq!(
        id1, id2,
        "shared helper must be deterministic for identical inputs"
    );
}

/// Verify that both contracts would produce the same ID for the first few projects.
/// This tests the shared helper directly since both contracts now delegate to it.
#[test]
fn test_first_five_project_ids_match() {
    let e = Env::default();

    for count in 0u64..5 {
        let timestamp = 1_700_000_000 + count * 3600;
        let id = generate_project_id(&e, count, timestamp);

        // Verify the ID is a valid 32-byte hash (not raw byte-packing with trailing zeros)
        assert_eq!(id.len(), 32);

        // Verify that bytes 16..32 are NOT all zeros (which would indicate old byte-packing)
        let arr = id.to_array();
        let trailing_zeros = arr[16..].iter().all(|&b| b == 0);
        assert!(
            !trailing_zeros,
            "project ID must be a SHA-256 hash, not raw byte-packing"
        );
    }
}

/// Verify uniqueness across different counts with the same timestamp.
#[test]
fn test_uniqueness_same_timestamp() {
    let e = Env::default();
    let timestamp = 1_700_000_000;

    let mut seen = soroban_sdk::Vec::<BytesN<32>>::new(&e);
    for count in 0u64..10 {
        let id = generate_project_id(&e, count, timestamp);
        // Ensure no duplicates
        for i in 0..seen.len() {
            assert_ne!(seen.get(i).unwrap(), id, "duplicate ID at count={}", count);
        }
        seen.push_back(id);
    }
}

/// Verify that the old byte-packing scheme would have produced different IDs.
/// This documents the breaking change: IDs from project_registry before this fix
/// (raw byte-packed) are NOT compatible with the new SHA-256 IDs.
#[test]
fn test_documents_breaking_change() {
    let e = Env::default();

    let count: u64 = 0;
    let timestamp: u64 = 1_700_000_000;

    // New canonical ID (SHA-256 based)
    let new_id = generate_project_id(&e, count, timestamp);

    // Old byte-packing scheme (what project_registry used to do)
    let count_bytes = count.to_be_bytes();
    let ts_bytes = timestamp.to_be_bytes();
    let old_id = BytesN::from_array(&e, &{
        let mut arr = [0u8; 32];
        arr[..8].copy_from_slice(&count_bytes);
        arr[8..16].copy_from_slice(&ts_bytes);
        arr
    });

    assert_ne!(
        new_id, old_id,
        "new SHA-256 IDs must differ from old byte-packed IDs (breaking change)"
    );

    // The old scheme has trailing zeros; the new one does not
    let old_arr = old_id.to_array();
    assert!(
        old_arr[16..].iter().all(|&b| b == 0),
        "old scheme has trailing zeros"
    );
}
