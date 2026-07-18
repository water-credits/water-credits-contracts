#![no_std]
use soroban_sdk::{Bytes, BytesN, Env};

/// Canonical project ID generation across all contracts.
///
/// Produces a deterministic 32-byte ID from a monotonically increasing count
/// and the ledger timestamp at registration time.
///
/// Format: SHA-256( count_be8 ‖ timestamp_be8 )
///
/// Both `count` and `timestamp` are encoded as 8-byte big-endian values,
/// concatenated into a 16-byte preimage, and hashed with SHA-256 to produce
/// the 32-byte project ID.
///
/// This is the single source of truth for project ID generation.
/// All contracts that create or reference project IDs MUST use this function.
pub fn generate_project_id(e: &Env, count: u64, timestamp: u64) -> BytesN<32> {
    let mut preimage: Bytes = Bytes::new(e);
    {
        let count_bytes = count.to_be_bytes();
        preimage.append(&Bytes::from_array(e, &count_bytes));
    }
    {
        let ts_bytes = timestamp.to_be_bytes();
        preimage.append(&Bytes::from_array(e, &ts_bytes));
    }
    e.crypto().sha256(&preimage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deterministic() {
        let e = Env::default();
        let id1 = generate_project_id(&e, 0, 1000);
        let id2 = generate_project_id(&e, 0, 1000);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_different_count_different_id() {
        let e = Env::default();
        let id1 = generate_project_id(&e, 0, 1000);
        let id2 = generate_project_id(&e, 1, 1000);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_different_timestamp_different_id() {
        let e = Env::default();
        let id1 = generate_project_id(&e, 0, 1000);
        let id2 = generate_project_id(&e, 0, 1001);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_full_32_bytes() {
        let e = Env::default();
        let id = generate_project_id(&e, 42, 9999);
        assert_eq!(id.len(), 32);
    }
}
