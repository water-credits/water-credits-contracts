//! Supply-conservation integration test (SPEC §5, Invariant 1):
//!
//! ```text
//! total_supply + total_retired + total_burned == ever_minted()
//! ```
//!
//! All six contracts are deployed and wired through their real authorization
//! chain (`set_minter` via the factory, `set_retirement_registry`,
//! `set_authorized_caller`, `set_project_config`), with the credit token
//! deployed from its compiled WASM blob exactly like a production deployment.
//!
//! The invariant is asserted after every mutating step using the token's own
//! on-chain `ever_minted()` read — the observable function that SPEC §5,
//! Invariant 1 names as the "ever minted" reference:
//!
//! ```text
//! commit-reveal round → auto-mint → manual mint_to → transfer → retire
//!     → admin burn → retire ×2 (different users) → final cross-check
//! ```
//!
//! Transfers are conservative (they never change any of the four counters);
//! `retire()` moves credits from `total_supply` into `total_retired`;
//! `burn()` moves credits from `total_supply` into `total_burned`; only
//! minting ever grows `ever_minted()`. The retirement registry's
//! `total_retired()` must track the token's `total_retired()` exactly at
//! every step and in the final state.

use credit_factory::{CreditFactory, CreditFactoryClient};
use credit_token::CreditTokenClient;
use governance::{Governance, GovernanceClient};
use project_registry::{ProjectRegistry, ProjectRegistryClient};
use retirement_registry::{RetirementRegistry, RetirementRegistryClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Address, Bytes, BytesN, Env, String,
};
use verification_oracle::{
    sha256_commitment, RevealParams, VerificationOracle, VerificationOracleClient,
};

/// Fixed ledger timestamp so certificate/record timestamps are deterministic.
const LEDGER_TIMESTAMP: u64 = 1_752_710_400;

#[test]
fn test_supply_conservation_across_full_lifecycle() {
    let e = Env::default();
    e.mock_all_auths();
    // Uploading and executing the real credit_token WASM exceeds the default
    // test budget; this test verifies behavior, not metering.
    e.budget().reset_unlimited();
    e.ledger().with_mut(|l| l.timestamp = LEDGER_TIMESTAMP);

    let admin = Address::generate(&e);
    let project_owner = Address::generate(&e);
    let buyer = Address::generate(&e);
    let holder2 = Address::generate(&e);

    // ─────────────────────────────────────────────────────────────────────
    // Deploy all six contracts (README deployment-guide order)
    // ─────────────────────────────────────────────────────────────────────

    // 1. governance
    let governance_id = e.register_contract(None, Governance);
    let governance = GovernanceClient::new(&e, &governance_id);
    let members = vec![
        &e,
        admin.clone(),
        Address::generate(&e),
        Address::generate(&e),
    ];
    governance.initialize(&admin, &members);

    // 2. project_registry
    let project_registry_id = e.register_contract(None, ProjectRegistry);
    let project_registry = ProjectRegistryClient::new(&e, &project_registry_id);
    project_registry.initialize(&admin);

    // 3. credit_token reference WASM — upload the real compiled blob (same
    // path the factory's `register_project` deployer uses on-chain).
    let wasm_bytes = std::fs::read(env!("CREDIT_TOKEN_WASM"))
        .expect("credit_token.wasm should have been built by tests/build.rs");
    let token_wasm_hash = e
        .deployer()
        .upload_contract_wasm(Bytes::from_slice(&e, &wasm_bytes));

    // 4. credit_factory
    let factory_id = e.register_contract(None, CreditFactory);
    let factory = CreditFactoryClient::new(&e, &factory_id);
    factory.initialize(&admin);

    // 5. verification_oracle (staking disabled by default; min_oracles=3)
    let oracle_id = e.register_contract(None, VerificationOracle);
    let oracle = VerificationOracleClient::new(&e, &oracle_id);
    let staking_token = Address::generate(&e);
    let treasury = Address::generate(&e);
    oracle.initialize(&admin, &staking_token, &treasury);

    // 6. retirement_registry
    let retirement_registry_id = e.register_contract(None, RetirementRegistry);
    let retirement_registry = RetirementRegistryClient::new(&e, &retirement_registry_id);
    retirement_registry.initialize(&admin);

    // ── Wire factory ↔ project_registry ──────────────────────────────────
    project_registry.set_authorized_caller(&admin, &factory_id, &true);
    factory.set_project_registry(&admin, &Some(project_registry_id.clone()));

    // ─────────────────────────────────────────────────────────────────────
    // Register the project via the factory (deploys a real token WASM
    // instance and sets the oracle as minter)
    // ─────────────────────────────────────────────────────────────────────

    let project_id = factory.register_project(
        &admin,
        &String::from_str(&e, "Conservation Wetland"),
        &38_897_700i64,
        &-77_036_500i64,
        &String::from_str(&e, "Wetland_Restoration_v2.1"),
        &project_owner,
        &500u64,
        &token_wasm_hash,
        &Some(oracle_id.clone()),
    );

    let token_id = factory.get_project(&project_id).unwrap().credit_token;
    let token = CreditTokenClient::new(&e, &token_id);
    assert_eq!(token.total_supply(), 0);

    // ─────────────────────────────────────────────────────────────────────
    // Wire the remaining authorization chain (token address is only known
    // after register_project)
    // ─────────────────────────────────────────────────────────────────────

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

    // Supply-conservation invariant, asserted after every mutating op. Uses
    // the token's own on-chain ever_minted() as the reference.
    let check_invariants = || {
        let supply = token.total_supply();
        let retired = token.total_retired();
        let burned = token.total_burned();
        let ever = token.ever_minted();
        assert_eq!(
            supply + retired + burned,
            ever,
            "supply conservation violated: total_supply({supply}) + \
             total_retired({retired}) + total_burned({burned}) != \
             ever_minted({ever})"
        );
        let live = token.balance(&project_owner) + token.balance(&buyer) + token.balance(&holder2);
        assert_eq!(
            supply, live,
            "total_supply must equal the sum of live balances"
        );
        assert_eq!(
            retirement_registry.total_retired(),
            retired,
            "registry.total_retired must equal token.total_retired"
        );
    };
    check_invariants();

    // ─────────────────────────────────────────────────────────────────────
    // Step 1 — commit-reveal round → auto-mint 100 credits to the owner
    // ─────────────────────────────────────────────────────────────────────

    const EXPECTED_CREDITS: i128 = 100;
    let salt = BytesN::from_array(&e, &[0xF1u8; 32]);
    let nonce = 1u64;
    let (ph, turb, do_, flow, temp, n, p) = (700i64, 10i64, 80i64, 500i64, 250i64, 8i64, 1i64);
    let commitment = sha256_commitment(&e, nonce, ph, turb, do_, flow, temp, n, p, &salt);
    let reveal_params = RevealParams {
        nonce,
        ph,
        turbidity: turb,
        dissolved_oxygen: do_,
        flow_rate: flow,
        temperature: temp,
        total_nitrogen: n,
        total_phosphorus: p,
        salt: salt.clone(),
    };

    let o1 = Address::generate(&e);
    let o2 = Address::generate(&e);
    let o3 = Address::generate(&e);
    oracle.add_oracle(&admin, &o1);
    oracle.add_oracle(&admin, &o2);
    oracle.add_oracle(&admin, &o3);

    oracle.open_window(&admin, &project_id);
    oracle.commit_reading(&o1, &project_id, &nonce, &commitment);
    oracle.commit_reading(&o2, &project_id, &nonce, &commitment);
    oracle.commit_reading(&o3, &project_id, &nonce, &commitment);

    e.ledger().with_mut(|l| {
        l.timestamp += 301;
        l.sequence_number += 61;
    });
    oracle.begin_reveal_phase(&project_id);

    assert_eq!(
        oracle.reveal_reading(&o1, &project_id, &reveal_params),
        None
    );
    assert_eq!(
        oracle.reveal_reading(&o2, &project_id, &reveal_params),
        None
    );
    let result = oracle
        .reveal_reading(&o3, &project_id, &reveal_params)
        .expect("third reveal must finalize the window");
    assert_eq!(result.total_credits, EXPECTED_CREDITS);
    assert_eq!(result.credits_minted, EXPECTED_CREDITS);

    assert_eq!(token.balance(&project_owner), EXPECTED_CREDITS);
    assert_eq!(token.total_supply(), EXPECTED_CREDITS);
    assert_eq!(token.ever_minted(), EXPECTED_CREDITS);
    check_invariants();

    // ─────────────────────────────────────────────────────────────────────
    // Step 2 — manual mint_to (oracle as minter) → ever_minted grows
    // ─────────────────────────────────────────────────────────────────────

    let manual_amount = 300i128;
    token.mint_to(&oracle_id, &holder2, &manual_amount);

    assert_eq!(token.balance(&holder2), manual_amount);
    assert_eq!(token.ever_minted(), EXPECTED_CREDITS + manual_amount);
    assert_eq!(
        token.total_supply(),
        EXPECTED_CREDITS + manual_amount,
        "manual mint must increase total_supply by the same amount"
    );
    check_invariants();

    // ─────────────────────────────────────────────────────────────────────
    // Step 3 — transfer is conservative: all four counters unchanged
    // ─────────────────────────────────────────────────────────────────────

    let transfer_amount = 40i128;
    let supply_before = token.total_supply();
    token.transfer(&project_owner, &buyer, &transfer_amount);

    assert_eq!(
        token.balance(&project_owner),
        EXPECTED_CREDITS - transfer_amount
    );
    assert_eq!(token.balance(&buyer), transfer_amount);
    assert_eq!(token.total_supply(), supply_before);
    check_invariants();

    // ─────────────────────────────────────────────────────────────────────
    // Step 4 — retire: total_supply ↓, total_retired ↑ by same amount,
    // ever_minted unchanged, registry agrees
    // ─────────────────────────────────────────────────────────────────────

    let retire_amount = 30i128;
    let supply_before = token.total_supply();
    let retired_before = token.total_retired();
    let cert = token.retire(
        &buyer,
        &retire_amount,
        &String::from_str(&e, "voluntary offset"),
        &String::from_str(&e, "ipfs://QmConservation1"),
    );
    assert_eq!(cert.amount, retire_amount);
    assert_eq!(cert.registry_record_id, Some(1));

    assert_eq!(token.total_supply(), supply_before - retire_amount);
    assert_eq!(token.total_retired(), retired_before + retire_amount);
    assert_eq!(retirement_registry.total_retired(), token.total_retired());
    check_invariants();

    // ─────────────────────────────────────────────────────────────────────
    // Step 5 — admin burn: total_supply ↓, total_burned ↑ by same amount,
    // ever_minted unchanged
    // ─────────────────────────────────────────────────────────────────────

    let burn_amount = 50i128;
    let supply_before = token.total_supply();
    let burned_before = token.total_burned();
    token.burn(&admin, &project_owner, &burn_amount);

    assert_eq!(token.total_supply(), supply_before - burn_amount);
    assert_eq!(token.total_burned(), burned_before + burn_amount);
    // Burn is NOT recorded in the retirement registry.
    assert_eq!(retirement_registry.total_retired(), token.total_retired());
    check_invariants();

    // ─────────────────────────────────────────────────────────────────────
    // Steps 6–7 — two more retire operations by different users
    // ─────────────────────────────────────────────────────────────────────

    let retire2_amount = 200i128;
    let supply_before = token.total_supply();
    let retired_before = token.total_retired();
    token.retire(
        &holder2,
        &retire2_amount,
        &String::from_str(&e, "community"),
        &String::from_str(&e, "ipfs://QmConservation2"),
    );
    assert_eq!(token.total_supply(), supply_before - retire2_amount);
    assert_eq!(token.total_retired(), retired_before + retire2_amount);
    check_invariants();

    let retire3_amount = 10i128;
    let supply_before = token.total_supply();
    let retired_before = token.total_retired();
    token.retire(
        &project_owner,
        &retire3_amount,
        &String::from_str(&e, "compliance"),
        &String::from_str(&e, "ipfs://QmConservation3"),
    );
    assert_eq!(token.total_supply(), supply_before - retire3_amount);
    assert_eq!(token.total_retired(), retired_before + retire3_amount);
    check_invariants();

    // ─────────────────────────────────────────────────────────────────────
    // Final state — token and registry agree; conservation holds
    // ─────────────────────────────────────────────────────────────────────

    // supply = 400 minted - 30 - 50 - 200 - 10 retired/burned = 110
    assert_eq!(token.total_supply(), 110);
    assert_eq!(token.total_retired(), 240); // 30 + 200 + 10
    assert_eq!(token.total_burned(), 50);
    assert_eq!(token.ever_minted(), 400); // 100 + 300
    assert_eq!(
        token.total_retired(),
        retirement_registry.total_retired(),
        "final retired totals must match across contracts"
    );
    check_invariants();
}
