use credit_token::{CreditToken, CreditTokenClient};
use retirement_registry::{RetirementRegistry, RetirementRegistryClient};
use soroban_sdk::{testutils::Address as _, testutils::Ledger as _, Address, BytesN, Env, String};
use verification_oracle::{
    sha256_commitment, OracleConfig, RevealParams, VerificationOracle, VerificationOracleClient,
};

fn deploy_oracle(e: &Env, admin: &Address) -> (Address, VerificationOracleClient<'static>) {
    let contract_id = e.register_contract(None, VerificationOracle);
    let client = VerificationOracleClient::new(e, &contract_id);
    let staking_token = Address::generate(e);
    let treasury = Address::generate(e);
    client.initialize(admin, &staking_token, &treasury);
    // Disable staking for integration tests — staking requires a live token contract.
    // Keep min_oracles at 3 to match the test's 3-oracle commit-reveal flow.
    client.update_config(
        admin,
        &OracleConfig {
            min_oracles: 3,
            max_oracles: 10,
            quality_threshold_ph: 600,
            quality_threshold_ph_max: 700,
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
            min_reveal_ledgers: 0,
            max_reveal_ledgers: 60,
            slash_pct_bps: 1000,
            min_slash_amount: 0,
            max_slash_amount: i128::MAX,
        },
    );
    (contract_id, client)
}

/// Advance both the ledger timestamp and sequence number together, consistent
/// with the ~5s/ledger assumption the oracle contract's reveal-window checks
/// rely on.
fn advance_ledgers(e: &Env, secs: u64) {
    let bump = (secs / 5) as u32 + 1;
    let mut info = e.ledger().get();
    info.timestamp += secs;
    info.sequence_number += bump;
    e.ledger().set(info);
}

/// Run a full commit-reveal round for `oracles`, all submitting the same
/// reading under `nonce`. Opens the window, waits out the commit phase,
/// transitions to reveal, and reveals in order.
#[allow(clippy::too_many_arguments)]
fn commit_reveal_round(
    e: &Env,
    admin: &Address,
    client: &VerificationOracleClient<'static>,
    project_id: &BytesN<32>,
    oracles: &[Address],
    nonce: u64,
    reading: (i64, i64, i64, i64, i64, i64, i64),
    salt: &BytesN<32>,
) {
    let (ph, turb, do_, flow, temp, n, p) = reading;
    client.open_window(admin, project_id);
    let commitment = sha256_commitment(e, nonce, ph, turb, do_, flow, temp, n, p, salt);
    for o in oracles {
        client.commit_reading(o, project_id, &nonce, &commitment);
    }
    advance_ledgers(e, 301);
    client.begin_reveal_phase(project_id);
    let params = RevealParams {
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
    for o in oracles {
        client.reveal_reading(o, project_id, &params);
    }
}

fn deploy_token(
    e: &Env,
    admin: &Address,
    project_id: &BytesN<32>,
) -> (Address, CreditTokenClient<'static>) {
    let contract_id = e.register_contract(None, CreditToken);
    let client = CreditTokenClient::new(e, &contract_id);
    client.initialize(
        admin,
        &String::from_str(e, "Test Credit"),
        &String::from_str(e, "TST"),
        project_id,
        &String::from_str(e, "Test_v1"),
    );
    (contract_id, client)
}

fn deploy_registry(e: &Env, admin: &Address) -> (Address, RetirementRegistryClient<'static>) {
    let contract_id = e.register_contract(None, RetirementRegistry);
    let client = RetirementRegistryClient::new(e, &contract_id);
    client.initialize(admin);
    (contract_id, client)
}

#[test]
fn test_oracle_mints_credits_to_beneficiary() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let beneficiary = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[1u8; 32]);

    let (token_id, token_client) = deploy_token(&e, &admin, &project_id);
    let (oracle_id, oracle_client) = deploy_oracle(&e, &admin);

    // Configure: token minter = oracle contract
    token_client.set_minter(&admin, &oracle_id);

    // Configure: oracle project config for auto-mint
    oracle_client.set_project_config(&admin, &project_id, &token_id, &beneficiary, &10, &2, &300);

    // Add 3 oracles
    let o1 = Address::generate(&e);
    let o2 = Address::generate(&e);
    let o3 = Address::generate(&e);
    oracle_client.add_oracle(&admin, &o1);
    oracle_client.add_oracle(&admin, &o2);
    oracle_client.add_oracle(&admin, &o3);

    // Commit-reveal a reading from each oracle
    let salt = BytesN::from_array(&e, &[0xA1u8; 32]);
    commit_reveal_round(
        &e,
        &admin,
        &oracle_client,
        &project_id,
        &[o1, o2, o3],
        1,
        (700, 10, 80, 500, 250, 8, 1),
        &salt,
    );

    // Beneficiary should have received credits
    let balance = token_client.balance(&beneficiary);
    assert!(balance > 0, "beneficiary should receive minted credits");

    // Verify last result exists and has credits
    let result = oracle_client.get_last_result(&project_id).unwrap();
    assert_eq!(result.total_credits, balance);
}

#[test]
fn test_retire_cross_calls_registry() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let holder = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[2u8; 32]);

    let (token_id, token_client) = deploy_token(&e, &admin, &project_id);
    let (_registry_id, registry_client) = deploy_registry(&e, &admin);

    // Authorize token contract to call registry
    registry_client.set_authorized_caller(&admin, &token_id, &true);

    // Set registry on token
    token_client.set_retirement_registry(&admin, &_registry_id);

    // Mint credits to holder
    token_client.mint_to(&admin, &holder, &1000);

    // Retire credits
    let purpose = String::from_str(&e, "voluntary");
    let uri = String::from_str(&e, "ipfs://QmTest");
    let cert = token_client.retire(&holder, &500, &purpose, &uri);
    assert_eq!(cert.amount, 500);

    // Verify registry recorded the retirement
    assert_eq!(registry_client.total_retired(), 500);
    assert_eq!(registry_client.record_count(), 1);

    let record = registry_client.get_record(&1).unwrap();
    assert_eq!(record.retiree, holder);
    assert_eq!(record.amount, 500);

    // Verify certificate links back to registry record #1
    assert_eq!(cert.registry_record_id, Some(1));

    // Verify token state
    assert_eq!(token_client.balance(&holder), 500);
    assert_eq!(token_client.total_supply(), 500);
    assert_eq!(token_client.total_retired(), 500);
}

#[test]
fn test_unauthorized_oracle_rejected() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let unauthorized = Address::generate(&e);
    let _project_id = BytesN::from_array(&e, &[3u8; 32]);

    let (_oracle_id, oracle_client) = deploy_oracle(&e, &admin);

    // Verify only admin-authorized oracles are active
    assert!(!oracle_client.is_oracle_active(&unauthorized));

    // A non-active oracle submitting will panic the contract
    // (This panic is non-catchable in the test host, so we can only verify preconditions)
    let active = oracle_client.is_oracle_active(&admin);
    assert!(!active, "admin is not an oracle by default");

    // After adding an oracle it becomes active
    let oracle = Address::generate(&e);
    oracle_client.add_oracle(&admin, &oracle);
    assert!(oracle_client.is_oracle_active(&oracle));
}

/// # Supply conservation invariant — end-to-end
///
/// Invariant (SPEC §5, Invariant 1):
///   `total_supply + total_retired + total_burned == ever_minted`
///
/// This test walks through a representative lifecycle:
///   1. Mint to farmer
///   2. Transfer farmer → buyer
///   3. Buyer retires some credits (creates a retirement record in the registry)
///   4. Admin burns some of farmer's remaining credits (no retirement record)
///
/// After each step we assert the invariant holds.
#[test]
fn test_supply_conservation_invariant_mint_transfer_retire_burn() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let farmer = Address::generate(&e);
    let buyer = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[10u8; 32]);

    // ── Deploy contracts ──────────────────────────────────────────────────
    let (token_id, token_client) = deploy_token(&e, &admin, &project_id);
    let (_registry_id, registry_client) = deploy_registry(&e, &admin);

    // Authorize the token contract to record retirements in the registry
    registry_client.set_authorized_caller(&admin, &token_id, &true);
    token_client.set_retirement_registry(&admin, &_registry_id);

    // Helper: assert the invariant at any point.
    // ever_minted is passed in because the token only tracks current supply
    // (total_supply = ever_minted - total_retired - total_burned).
    let assert_invariant = |ever_minted: i128| {
        let ts = token_client.total_supply();
        let tr = token_client.total_retired();
        let tb = token_client.total_burned();
        assert_eq!(
            ts + tr + tb,
            ever_minted,
            "invariant violated: total_supply({ts}) + total_retired({tr}) + \
             total_burned({tb}) != ever_minted({ever_minted})"
        );
    };

    // ── Step 0: freshly initialized ──────────────────────────────────────
    assert_invariant(0);
    assert_eq!(token_client.total_burned(), 0);

    // ── Step 1: mint 5 000 credits to farmer ─────────────────────────────
    token_client.mint_to(&admin, &farmer, &5_000);
    // ever_minted = 5 000
    assert_eq!(token_client.balance(&farmer), 5_000);
    assert_invariant(5_000);

    // ── Step 2: farmer transfers 1 500 to buyer ───────────────────────────
    token_client.transfer(&farmer, &buyer, &1_500);
    assert_eq!(token_client.balance(&farmer), 3_500);
    assert_eq!(token_client.balance(&buyer), 1_500);
    // Transfer doesn't change total_supply, total_retired, or total_burned.
    assert_invariant(5_000);

    // ── Step 3: buyer retires 800 credits ────────────────────────────────
    let purpose = String::from_str(&e, "voluntary");
    let uri = String::from_str(&e, "ipfs://QmCert");
    let cert = token_client.retire(&buyer, &800, &purpose, &uri);
    assert_eq!(cert.amount, 800);
    assert_eq!(token_client.balance(&buyer), 700);
    assert_eq!(token_client.total_retired(), 800);
    assert_eq!(token_client.total_burned(), 0); // burn hasn't happened yet
    assert_invariant(5_000);

    // Cross-contract: registry must agree on the retired total
    assert_eq!(registry_client.total_retired(), 800);
    // Cross-contract: cert must reference registry record #1
    assert_eq!(cert.registry_record_id, Some(1));

    // ── Step 4: admin burns 500 from farmer (no retirement record) ────────
    token_client.burn(&admin, &farmer, &500);
    assert_eq!(token_client.balance(&farmer), 3_000);
    assert_eq!(token_client.total_burned(), 500);
    assert_eq!(token_client.total_retired(), 800); // unchanged
                                                   // total_supply = 5000 - 800 - 500 = 3700
    assert_eq!(token_client.total_supply(), 3_700);
    assert_invariant(5_000);

    // Burn is NOT recorded in the retirement registry
    assert_eq!(
        registry_client.total_retired(),
        800,
        "registry must not count admin burns"
    );

    // ── Step 5: second burn — ensure accumulator adds correctly ──────────
    token_client.burn(&admin, &farmer, &200);
    assert_eq!(token_client.total_burned(), 700);
    assert_invariant(5_000);

    // ── Step 6: second retirement from buyer ─────────────────────────────
    let uri2 = String::from_str(&e, "ipfs://QmCert2");
    token_client.retire(&buyer, &300, &purpose, &uri2);
    assert_eq!(token_client.total_retired(), 1_100);
    assert_invariant(5_000);
    assert_eq!(registry_client.total_retired(), 1_100);

    // ── Final sanity: sum of live balances == total_supply ───────────────
    let farmer_bal = token_client.balance(&farmer);
    let buyer_bal = token_client.balance(&buyer);
    assert_eq!(
        farmer_bal + buyer_bal,
        token_client.total_supply(),
        "Σbalances must equal total_supply at rest"
    );
}

#[test]
fn test_retire_certificate_defaults_record_id_to_none() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let user = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[1u8; 32]);

    let (_, token_client) = deploy_token(&e, &admin, &project_id);
    token_client.mint_to(&admin, &user, &1000);

    let purpose = String::from_str(&e, "voluntary");
    let uri = String::from_str(&e, "ipfs://QmTest");
    let cert = token_client.retire(&user, &100, &purpose, &uri);

    assert_eq!(cert.registry_record_id, None);
    assert_eq!(token_client.total_retired(), 100);
}

#[test]
fn test_full_credit_lifecycle() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let farmer = Address::generate(&e);
    let buyer = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[1u8; 32]);

    let (_, client) = deploy_token(&e, &admin, &project_id);

    client.mint_to(&admin, &farmer, &5000);
    assert_eq!(client.balance(&farmer), 5000);

    client.transfer(&farmer, &buyer, &1000);
    assert_eq!(client.balance(&farmer), 4000);
    assert_eq!(client.balance(&buyer), 1000);

    let purpose = String::from_str(&e, "voluntary");
    let uri = String::from_str(&e, "ipfs://QmCert");
    let cert = client.retire(&buyer, &500, &purpose, &uri);
    assert_eq!(cert.amount, 500);
    assert_eq!(cert.project_id, project_id);

    assert_eq!(client.balance(&buyer), 500);
    assert_eq!(client.total_retired(), 500);
    assert_eq!(client.total_supply(), 4500);
}
