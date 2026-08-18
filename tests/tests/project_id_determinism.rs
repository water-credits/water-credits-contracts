//! Ledger-independence of the canonical project ID (issue #96).
//!
//! `shared::generate_project_id` used to fold `e.ledger().timestamp()` into the
//! SHA-256 preimage, so the ID a registration produced depended on which ledger
//! the transaction happened to land in. An off-chain system (the NestJS backend
//! pre-computing the ID so it can be written to PostgreSQL atomically with the
//! on-chain transaction) could not know that timestamp in advance: a one-ledger
//! delay from a fee bump or congestion silently changed the ID and invalidated
//! the pre-computation.
//!
//! These tests drive the **real contract entry points** — `ProjectRegistry::register`
//! and `CreditFactory::register_project`, the latter deploying the real
//! `credit_token` WASM — at deliberately different ledger timestamps, and assert
//! that the returned ID does not move. They also assert the timestamp is still
//! recorded in the stored project entry, since it remains display metadata.

use credit_factory::{CreditFactory, CreditFactoryClient};
use project_registry::{ProjectRegistry, ProjectRegistryClient};
use shared::generate_project_id;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Bytes, BytesN, Env, String,
};

/// Two ledger timestamps a day apart — the "same transaction, different ledger"
/// scenario the issue describes, exaggerated so a partial fix cannot pass.
const LEDGER_TS_EARLY: u64 = 1_700_000_000;
const LEDGER_TS_LATE: u64 = 1_700_086_400;

struct Fixture {
    e: Env,
    admin: Address,
    owner: Address,
}

fn fixture_at(timestamp: u64) -> Fixture {
    let e = Env::default();
    e.mock_all_auths();
    // Deploying the real credit_token WASM exceeds the default test budget;
    // these tests verify ID derivation, not metering.
    e.budget().reset_unlimited();
    e.ledger().with_mut(|l| l.timestamp = timestamp);

    let admin = Address::generate(&e);
    let owner = Address::generate(&e);
    Fixture { e, admin, owner }
}

// ── Fixed registration inputs, identical across every ledger under test ──
const NAME: &str = "Green Valley Wetland";
const METHODOLOGY: &str = "Wetland_Restoration_v2.1";
const LATITUDE: i64 = 38_897_700;
const LONGITUDE: i64 = -77_036_500;
const AREA_HECTARES: u64 = 500;

/// Register the fixed project through the real `project_registry` entry point.
/// Returns the on-chain ID and the `registered_at` the contract stored.
///
/// The ID comes back as a raw `[u8; 32]` because these tests compare IDs
/// produced in *different* `Env`s, and `BytesN` equality panics when its two
/// operands belong to different host environments.
fn register_via_registry(f: &Fixture) -> ([u8; 32], u64) {
    let registry_id = f.e.register_contract(None, ProjectRegistry);
    let registry = ProjectRegistryClient::new(&f.e, &registry_id);
    registry.initialize(&f.admin);

    let project_id = registry.register(
        &f.admin,
        &String::from_str(&f.e, NAME),
        &LATITUDE,
        &LONGITUDE,
        &String::from_str(&f.e, METHODOLOGY),
        &f.owner,
        &AREA_HECTARES,
    );

    let entry = registry.get(&project_id).unwrap();
    (project_id.to_array(), entry.registered_at)
}

/// Register the fixed project through the real `credit_factory` entry point,
/// which additionally deploys and initializes the compiled credit_token WASM.
/// Returns the on-chain ID and the `registration_date` the contract stored
/// (raw bytes, for the same cross-`Env` reason as `register_via_registry`).
fn register_via_factory(f: &Fixture) -> ([u8; 32], u64) {
    let wasm_bytes = std::fs::read(env!("CREDIT_TOKEN_WASM"))
        .expect("credit_token.wasm should have been built by tests/build.rs");
    let token_wasm_hash =
        f.e.deployer()
            .upload_contract_wasm(Bytes::from_slice(&f.e, &wasm_bytes));

    let factory_id = f.e.register_contract(None, CreditFactory);
    let factory = CreditFactoryClient::new(&f.e, &factory_id);
    factory.initialize(&f.admin);

    let project_id = factory.register_project(
        &f.admin,
        &String::from_str(&f.e, NAME),
        &LATITUDE,
        &LONGITUDE,
        &String::from_str(&f.e, METHODOLOGY),
        &f.owner,
        &AREA_HECTARES,
        &token_wasm_hash,
    );

    let project = factory.get_project(&project_id).unwrap();
    (project_id.to_array(), project.registration_date)
}

/// The headline: the same registration submitted in two different ledgers
/// yields the same project ID through the real `project_registry` entry point.
#[test]
fn test_registry_project_id_is_identical_across_ledger_timestamps() {
    let (id_early, stored_early) = register_via_registry(&fixture_at(LEDGER_TS_EARLY));
    let (id_late, stored_late) = register_via_registry(&fixture_at(LEDGER_TS_LATE));

    assert_eq!(
        id_early, id_late,
        "project_registry must derive the same ID regardless of the ledger the \
         registration lands in"
    );

    // The timestamp is still recorded — it is display metadata, it just no
    // longer feeds the hash.
    assert_eq!(stored_early, LEDGER_TS_EARLY);
    assert_eq!(stored_late, LEDGER_TS_LATE);
    assert_ne!(
        stored_early, stored_late,
        "the two registrations really did land in different ledgers"
    );
}

/// Same property through the real `credit_factory` entry point, which deploys
/// the compiled credit_token WASM as part of registration.
#[test]
fn test_factory_project_id_is_identical_across_ledger_timestamps() {
    let (id_early, stored_early) = register_via_factory(&fixture_at(LEDGER_TS_EARLY));
    let (id_late, stored_late) = register_via_factory(&fixture_at(LEDGER_TS_LATE));

    assert_eq!(
        id_early, id_late,
        "credit_factory must derive the same ID regardless of the ledger the \
         registration lands in"
    );

    assert_eq!(stored_early, LEDGER_TS_EARLY);
    assert_eq!(stored_late, LEDGER_TS_LATE);
}

/// The factory and the registry must still agree on the canonical ID, and that
/// agreement must survive the two registrations landing in different ledgers —
/// the mirrored-registration invariant `full_lifecycle.rs` relies on, which
/// previously only held when both landed in the *same* ledger.
#[test]
fn test_factory_and_registry_agree_across_different_ledgers() {
    let (factory_id, _) = register_via_factory(&fixture_at(LEDGER_TS_EARLY));
    let (registry_id, _) = register_via_registry(&fixture_at(LEDGER_TS_LATE));

    assert_eq!(
        factory_id, registry_id,
        "canonical ID scheme must agree across factory and project_registry \
         even when the two registrations land in different ledgers"
    );
}

/// The scenario the issue is actually about: an off-chain caller derives the
/// expected ID *before* submitting, using only inputs it already knows (the
/// current project count and the registration fields), then submits into an
/// unrelated, later ledger. The pre-computed ID must match what the contract
/// returns.
#[test]
fn test_offchain_precomputed_id_matches_onchain_registration() {
    // Off-chain: the backend knows the next project ordinal is 0 and knows the
    // registration fields. It has no idea which ledger the transaction will
    // land in, and no longer needs to.
    let offchain_env = Env::default();
    let expected_id = generate_project_id(
        &offchain_env,
        0,
        &String::from_str(&offchain_env, NAME),
        &String::from_str(&offchain_env, METHODOLOGY),
        LATITUDE,
        LONGITUDE,
        AREA_HECTARES,
    );
    let expected_bytes = expected_id.to_array();

    // On-chain: the transaction lands in a ledger the backend never saw.
    let (onchain_id, _) = register_via_registry(&fixture_at(LEDGER_TS_LATE));

    assert_eq!(
        onchain_id, expected_bytes,
        "pre-computed project ID must match the ID the contract assigns"
    );

    // Pin the exact digest for these inputs. This is the value an independent
    // implementation of the byte layout documented in README.md "Project ID
    // Derivation" / SPEC §2.5 produces, so it fails if the preimage ever drifts
    // away from the documented one.
    //
    //   SHA-256(
    //     u64be(0)
    //     | u32be(20)  | "Green Valley Wetland"
    //     | u32be(24)  | "Wetland_Restoration_v2.1"
    //     | i64be(38897700) | i64be(-77036500) | u64be(500)
    //   )
    const DOCUMENTED_DIGEST: [u8; 32] = [
        0x23, 0xb1, 0x4d, 0x8d, 0xd8, 0x60, 0xfa, 0x2a, 0x48, 0xd3, 0xc1, 0xed, 0xa0, 0xdc, 0x09,
        0x99, 0xaa, 0x73, 0xef, 0xa0, 0xd2, 0xe9, 0x0a, 0x56, 0x60, 0x41, 0x76, 0x7c, 0xa0, 0x17,
        0x0b, 0x2b,
    ];
    assert_eq!(
        onchain_id, DOCUMENTED_DIGEST,
        "on-chain ID must match the documented off-chain derivation"
    );
}

/// Guards the uniqueness argument for dropping the timestamp: `count` is now
/// the only field distinguishing two registrations of otherwise identical
/// project details, so consecutive registrations must still get distinct IDs.
#[test]
fn test_consecutive_registrations_of_identical_details_stay_unique() {
    let f = fixture_at(LEDGER_TS_EARLY);
    let registry_id = f.e.register_contract(None, ProjectRegistry);
    let registry = ProjectRegistryClient::new(&f.e, &registry_id);
    registry.initialize(&f.admin);

    let mut ids: std::vec::Vec<BytesN<32>> = std::vec::Vec::new();
    for _ in 0..5 {
        let id = registry.register(
            &f.admin,
            &String::from_str(&f.e, NAME),
            &LATITUDE,
            &LONGITUDE,
            &String::from_str(&f.e, METHODOLOGY),
            &f.owner,
            &AREA_HECTARES,
        );
        assert!(
            !ids.contains(&id),
            "duplicate project ID for re-registration of identical details"
        );
        ids.push(id);
    }
    assert_eq!(registry.count(), 5);
}
