//! End-to-end integration test covering the complete six-contract protocol
//! lifecycle (issue #40):
//!
//! ```text
//! register_project → add_oracle ×3 → submit_reading ×3 → mint_to (auto)
//!     → transfer → retire → record_retirement → verify all invariants
//! ```
//!
//! Contracts are deployed and initialized in the exact order of the README
//! deployment guide (governance → project_registry → credit_token reference
//! WASM → credit_factory → verification_oracle → retirement_registry), and
//! the full authorization chain (`set_minter`, `set_retirement_registry`,
//! `set_authorized_caller`, `set_project_config`) is wired the same way a
//! real deployment would be.
//!
//! The token minted by the factory is a *real* `credit_token.wasm` blob
//! (compiled by tests/build.rs and deployed through the Soroban deployer),
//! so this test also exercises the WASM upload/deploy path that
//! `register_project` uses on-chain — not just the native test shims.
//!
//! Supply-conservation invariant checked after every mutating operation:
//!
//! ```text
//! total_supply == Σ live balances
//! total_supply + total_retired + total_burned == ever_minted
//! registry.total_retired == token.total_retired
//! ```
//!
//! (The issue text phrases the invariant as
//! `total_supply == Σbalances + total_retired`; per the contract's actual
//! accounting — see SPEC §5 and `test_supply_conservation_invariant_*` in
//! credit_lifecycle.rs — `retire()` *reduces* `total_supply`, so the
//! conserved quantity is `ever_minted`, tracked locally by the test.)
//!
//! Known integration gap (documented as a follow-up in issue #40):
//! `project_registry` and `credit_factory` are independent contracts — the
//! factory does not write into `project_registry`, so a deployment must
//! register the project in both places. Both derive IDs with
//! `shared::generate_project_id(count, timestamp)`, so mirrored
//! registrations made in the same ledger with the same ordinal produce the
//! same project ID, which this test asserts.

use credit_factory::{CreditFactory, CreditFactoryClient};
use credit_token::{CreditTokenClient, RetirementCertificate};
use governance::{Governance, GovernanceClient};
use project_registry::{ProjectRegistry, ProjectRegistryClient};
use retirement_registry::{RetirementRegistry, RetirementRegistryClient};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events, Ledger},
    vec, Address, Bytes, BytesN, Env, String, Symbol, TryFromVal, Val,
};
use verification_oracle::{
    OracleConfig, VerificationOracle, VerificationOracleClient, VerificationResult,
};

/// Fixed ledger timestamp so certificate/record timestamps are non-zero and
/// deterministic.
const LEDGER_TIMESTAMP: u64 = 1_752_710_400;

/// Return the data payload of the most recent event published by `contract`
/// whose first topic equals `topic`. Panics if no such event is in the
/// environment's event buffer.
fn last_event_data(e: &Env, contract: &Address, topic: Symbol) -> Val {
    let events = e.events().all();
    let mut found: Option<Val> = None;
    for i in 0..events.len() {
        let (c, topics, data) = events.get(i).unwrap();
        if c == *contract {
            if let Ok(t) = Symbol::try_from_val(e, &topics.get(0).unwrap()) {
                if t == topic {
                    found = Some(data);
                }
            }
        }
    }
    found.unwrap_or_else(|| panic!("expected event with topic {topic:?} from {contract:?}"))
}

#[test]
fn test_full_six_contract_lifecycle() {
    let e = Env::default();
    e.mock_all_auths();
    // Uploading and executing the real credit_token WASM exceeds the default
    // test budget; this test verifies behavior, not metering.
    e.budget().reset_unlimited();
    e.ledger().with_mut(|l| l.timestamp = LEDGER_TIMESTAMP);

    let admin = Address::generate(&e);
    // Project owner doubles as the auto-mint beneficiary.
    let project_owner = Address::generate(&e);
    let buyer = Address::generate(&e);

    // ─────────────────────────────────────────────────────────────────────
    // Phase 1 — deploy all six contracts in README deployment-guide order
    // ─────────────────────────────────────────────────────────────────────

    // 1. governance (first, for multisig setup)
    let governance_id = e.register_contract(None, Governance);
    let governance = GovernanceClient::new(&e, &governance_id);
    let members = vec![
        &e,
        admin.clone(),
        Address::generate(&e),
        Address::generate(&e),
    ];
    governance.initialize(&admin, &members);
    assert_eq!(governance.member_count_fn(), 3);

    // 2. project_registry
    let project_registry_id = e.register_contract(None, ProjectRegistry);
    let project_registry = ProjectRegistryClient::new(&e, &project_registry_id);
    project_registry.initialize(&admin);

    // 3+4. credit_token reference WASM — upload the real compiled blob and
    // take its hash, exactly like `soroban contract install` would.
    // The path is produced by tests/build.rs at compile time.
    let wasm_bytes = std::fs::read(env!("CREDIT_TOKEN_WASM"))
        .expect("credit_token.wasm should have been built by tests/build.rs");
    let token_wasm_hash = e
        .deployer()
        .upload_contract_wasm(Bytes::from_slice(&e, &wasm_bytes));

    // 5. credit_factory
    let factory_id = e.register_contract(None, CreditFactory);
    let factory = CreditFactoryClient::new(&e, &factory_id);
    factory.initialize(&admin);
    assert_eq!(factory.admin(), admin);
    assert_eq!(factory.project_count(), 0);

    // 6. verification_oracle — staking disabled (min_stake = 0) because
    // integration tests have no live staking token; min_oracles stays 3 to
    // match the three-oracle submission flow below.
    let oracle_id = e.register_contract(None, VerificationOracle);
    let oracle = VerificationOracleClient::new(&e, &oracle_id);
    let staking_token = Address::generate(&e);
    let treasury = Address::generate(&e);
    oracle.initialize(&admin, &staking_token, &treasury);
    oracle.update_config(
        &admin,
        &OracleConfig {
            min_oracles: 3,
            max_oracles: 10,
            quality_threshold_ph: 600,
            quality_threshold_turbidity: 50,
            quality_threshold_do: 50,
            quality_threshold_temp: 300,
            credit_per_kg_n: 10,
            credit_per_kg_p: 20,
            staking_token,
            treasury,
            min_stake: 0,
            unstake_cooldown_secs: 86400,
            commit_phase_secs: 300,
            reveal_phase_secs: 300,
        },
    );

    // 7. retirement_registry
    let retirement_registry_id = e.register_contract(None, RetirementRegistry);
    let retirement_registry = RetirementRegistryClient::new(&e, &retirement_registry_id);
    retirement_registry.initialize(&admin);

    // ─────────────────────────────────────────────────────────────────────
    // Phase 2 — register the project via the factory (deploys the token)
    // ─────────────────────────────────────────────────────────────────────

    let name = String::from_str(&e, "Green Valley Wetland");
    let methodology = String::from_str(&e, "Wetland_Restoration_v2.1");
    let latitude = 38_897_700i64;
    let longitude = -77_036_500i64;
    let area_hectares = 500u64;

    let project_id = factory.register_project(
        &admin,
        &name,
        &latitude,
        &longitude,
        &methodology,
        &project_owner,
        &area_hectares,
        &token_wasm_hash,
    );

    // Event: proj_reg(project_id) from the factory.
    let proj_reg_data = last_event_data(&e, &factory_id, symbol_short!("proj_reg"));
    let (evt_project_id,) = <(BytesN<32>,)>::try_from_val(&e, &proj_reg_data).unwrap();
    assert_eq!(evt_project_id, project_id);

    let project = factory.get_project(&project_id).unwrap();
    assert_eq!(project.id, project_id);
    assert_eq!(project.name, name);
    assert_eq!(project.owner, project_owner);
    assert_eq!(project.status, String::from_str(&e, "registered"));
    assert_eq!(project.area_hectares, area_hectares);
    assert_eq!(factory.project_count(), 1);

    // The factory deployed and initialized a real credit_token WASM
    // instance; thread its address through the rest of the setup.
    let token_id = project.credit_token.clone();
    let token = CreditTokenClient::new(&e, &token_id);
    assert_eq!(token.name(), name);
    assert_eq!(token.symbol(), String::from_str(&e, "WC"));
    assert_eq!(token.decimals(), 7);
    assert_eq!(token.metadata().project_id, project_id);
    assert_eq!(token.metadata().methodology, methodology);
    assert_eq!(token.total_supply(), 0);

    // Mirror the registration in project_registry. The two contracts are
    // NOT integrated (the factory does not call the registry) — see the
    // module docs; this is the documented follow-up gap. Both use
    // shared::generate_project_id(count, timestamp), so the mirrored entry
    // gets the same canonical ID.
    let registry_project_id = project_registry.register(
        &admin,
        &name,
        &latitude,
        &longitude,
        &methodology,
        &project_owner,
        &area_hectares,
    );
    assert_eq!(
        registry_project_id, project_id,
        "canonical ID scheme must agree across factory and project_registry"
    );
    assert_eq!(project_registry.count(), 1);
    let entry = project_registry.get(&registry_project_id).unwrap();
    assert_eq!(entry.owner, project_owner);
    assert_eq!(entry.status, String::from_str(&e, "registered"));

    // ─────────────────────────────────────────────────────────────────────
    // Phase 3 — wire the authorization chain (post-deployment, since the
    // token address is only known after register_project)
    // ─────────────────────────────────────────────────────────────────────

    // Oracle is the only minter of the project token.
    token.set_minter(&admin, &oracle_id);
    // Retirements cross-call the retirement registry…
    token.set_retirement_registry(&admin, &retirement_registry_id);
    // …which must whitelist the token contract as a caller.
    retirement_registry.set_authorized_caller(&admin, &token_id, &true);
    // Auto-mint config: verified credits go to the project owner.
    oracle.set_project_config(
        &admin,
        &project_id,
        &token_id,
        &project_owner,
        &10,
        &2,
        &300,
    );
    // Governance tracks the token for emergency pause coverage.
    governance.register_token(&admin, &token_id);
    assert_eq!(
        governance.list_registered_tokens(),
        vec![&e, token_id.clone()]
    );

    // Supply-conservation invariant, asserted after every mutating op.
    let check_invariants = |ever_minted: i128| {
        let owner_bal = token.balance(&project_owner);
        let buyer_bal = token.balance(&buyer);
        let supply = token.total_supply();
        let retired = token.total_retired();
        let burned = token.total_burned();
        assert_eq!(
            supply,
            owner_bal + buyer_bal,
            "total_supply must equal the sum of live balances"
        );
        assert_eq!(
            supply + retired + burned,
            ever_minted,
            "supply conservation violated: {supply} + {retired} + {burned} != {ever_minted}"
        );
        assert_eq!(
            retirement_registry.total_retired(),
            retired,
            "registry and token must agree on the retired total"
        );
    };
    check_invariants(0);

    // ─────────────────────────────────────────────────────────────────────
    // Phase 4 — oracle setup and three sensor readings → auto-mint
    // ─────────────────────────────────────────────────────────────────────

    let o1 = Address::generate(&e);
    let o2 = Address::generate(&e);
    let o3 = Address::generate(&e);
    oracle.add_oracle(&admin, &o1);
    oracle.add_oracle(&admin, &o2);
    oracle.add_oracle(&admin, &o3);
    assert_eq!(oracle.oracle_count(), 3);
    assert!(oracle.is_oracle_active(&o1));
    assert!(oracle.is_oracle_active(&o2));
    assert!(oracle.is_oracle_active(&o3));

    // Sensor readings: ph 7.00, turbidity 10, DO 8.0, flow 500, temp 25.0°C,
    // N 8 mg/L, P 1 mg/L. With the config above the finalized window yields:
    //   n_removed  = (10 - 8) * 500 * 3600 / 1e6 = 3 kg  → 3 * 10 = 30
    //   p_removed  = (2 - 1)  * 500 * 3600 / 1e6 = 1 kg  → 1 * 20 = 20
    //   volumetric = 500 * 100 / 1000            = 50
    //   penalty    = 0 (all quality thresholds met)
    //   total      = 100 credits
    const EXPECTED_CREDITS: i128 = 100;
    let submit = |oracle_addr: &Address| {
        oracle.submit_reading(
            oracle_addr,
            &project_id,
            &1, // first submission for this (project, oracle) pair
            &700i64,
            &10i64,
            &80i64,
            &500i64,
            &250i64,
            &8i64,
            &1i64,
        )
    };

    // Window does not finalize below min_oracles = 3.
    assert_eq!(submit(&o1), None);
    assert_eq!(token.total_supply(), 0, "no mint before window finalizes");
    assert_eq!(submit(&o2), None);
    assert_eq!(token.total_supply(), 0, "no mint before window finalizes");

    // Third submission finalizes the window and auto-mints. Check the
    // emitted events first, before any further client calls touch the
    // event buffer.
    let result = submit(&o3).expect("third submission must finalize the window");

    // Event: rdng_vrfy(project_id, result) from the oracle.
    let vrfy_data = last_event_data(&e, &oracle_id, symbol_short!("rdng_vrfy"));
    let (evt_proj, evt_result) =
        <(BytesN<32>, VerificationResult)>::try_from_val(&e, &vrfy_data).unwrap();
    assert_eq!(evt_proj, project_id);
    assert_eq!(evt_result, result);

    // Event: minted(beneficiary, amount) from the (WASM) token contract,
    // emitted inside the same submit_reading invocation.
    let minted_data = last_event_data(&e, &token_id, symbol_short!("minted"));
    let (evt_to, evt_amount) = <(Address, i128)>::try_from_val(&e, &minted_data).unwrap();
    assert_eq!(evt_to, project_owner);
    assert_eq!(evt_amount, EXPECTED_CREDITS);

    assert_eq!(result.total_credits, EXPECTED_CREDITS);
    assert_eq!(result.credits_minted, EXPECTED_CREDITS);
    assert_eq!(result.oracle_count, 3);
    assert_eq!(result.quality_penalty, 0);
    assert_eq!(result.project_id, project_id);
    assert_eq!(oracle.get_last_result(&project_id).unwrap(), result);

    // Beneficiary balance equals the verified credit total.
    assert_eq!(token.balance(&project_owner), result.total_credits);
    check_invariants(EXPECTED_CREDITS);

    // ─────────────────────────────────────────────────────────────────────
    // Phase 5 — transfer half of the credits to a buyer
    // ─────────────────────────────────────────────────────────────────────

    let half = EXPECTED_CREDITS / 2; // 50
    token.transfer(&project_owner, &buyer, &half);

    // Event: xfer(from, to, amount).
    let xfer_data = last_event_data(&e, &token_id, symbol_short!("xfer"));
    let (evt_from, evt_to, evt_amount) =
        <(Address, Address, i128)>::try_from_val(&e, &xfer_data).unwrap();
    assert_eq!(evt_from, project_owner);
    assert_eq!(evt_to, buyer);
    assert_eq!(evt_amount, half);

    assert_eq!(token.balance(&project_owner), EXPECTED_CREDITS - half);
    assert_eq!(token.balance(&buyer), half);
    check_invariants(EXPECTED_CREDITS);

    // ─────────────────────────────────────────────────────────────────────
    // Phase 6 — buyer retires credits; token cross-calls the registry
    // ─────────────────────────────────────────────────────────────────────

    let retire_amount = 30i128;
    let purpose = String::from_str(&e, "voluntary offset");
    let uri = String::from_str(&e, "ipfs://QmFullLifecycle");
    let cert = token.retire(&buyer, &retire_amount, &purpose, &uri);

    // Event: retired(holder, amount, cert) — checked first, before other
    // client calls touch the event buffer.
    let retired_data = last_event_data(&e, &token_id, symbol_short!("retired"));
    let (evt_holder, evt_amount, evt_cert) =
        <(Address, i128, RetirementCertificate)>::try_from_val(&e, &retired_data).unwrap();
    assert_eq!(evt_holder, buyer);
    assert_eq!(evt_amount, retire_amount);
    assert_eq!(evt_cert, cert);

    // Certificate contents.
    assert_eq!(cert.retiree, buyer);
    assert_eq!(cert.project_id, project_id);
    assert_eq!(cert.amount, retire_amount);
    assert_eq!(cert.purpose, purpose);
    assert_eq!(cert.metadata_uri, uri);
    assert_eq!(cert.timestamp, LEDGER_TIMESTAMP);
    // Certificate links to the registry record created by the cross-call,
    // and that record is the registry's latest.
    assert_eq!(cert.registry_record_id, Some(1));
    assert_eq!(retirement_registry.record_count(), 1);
    assert_eq!(
        cert.registry_record_id,
        Some(retirement_registry.record_count())
    );

    // Registry record contents.
    assert_eq!(retirement_registry.total_retired(), retire_amount);
    let record = retirement_registry.get_record(&1).unwrap();
    assert_eq!(record.id, 1);
    assert_eq!(record.retiree, buyer);
    assert_eq!(record.project_id, project_id);
    assert_eq!(record.amount, retire_amount);
    assert_eq!(record.purpose, purpose);
    assert_eq!(record.metadata_uri, uri);
    assert_eq!(record.timestamp, LEDGER_TIMESTAMP);
    assert_eq!(retirement_registry.retiree_count(&buyer), 1);
    assert_eq!(retirement_registry.project_retirement_count(&project_id), 1);

    // Certificate is also stored on the token side (index 0).
    assert_eq!(token.get_certificate(&0), Some(cert.clone()));

    // ─────────────────────────────────────────────────────────────────────
    // Phase 7 — final invariants across the whole stack
    // ─────────────────────────────────────────────────────────────────────

    assert_eq!(token.balance(&project_owner), 50);
    assert_eq!(token.balance(&buyer), 20);
    assert_eq!(token.total_supply(), 70);
    assert_eq!(token.total_retired(), 30);
    assert_eq!(token.total_burned(), 0);
    check_invariants(EXPECTED_CREDITS);
}
