#![no_std]
use soroban_sdk::{Bytes, BytesN, Env, String};

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
    let mut preimage: Bytes = Bytes::new(e);

    let count_bytes = count.to_be_bytes();
    preimage.append(&Bytes::from_array(e, &count_bytes));

    let ts_bytes = timestamp.to_be_bytes();
    preimage.append(&Bytes::from_array(e, &ts_bytes));

    let name_len = name.len();
    preimage.append(&Bytes::from_array(e, &name_len.to_be_bytes()));
    let name_len_usize = name_len as usize;
    if name_len_usize > 0 {
        let mut name_buf = [0u8; 256];
        name.copy_into_slice(&mut name_buf[..name_len_usize]);
        preimage.append(&Bytes::from_slice(e, &name_buf[..name_len_usize]));
    }

    let methodology_len = methodology.len();
    preimage.append(&Bytes::from_array(e, &methodology_len.to_be_bytes()));
    let methodology_len_usize = methodology_len as usize;
    if methodology_len_usize > 0 {
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
}
