use credit_token::{CreditToken, CreditTokenClient};
use soroban_sdk::{testutils::Address as _, testutils::Ledger as _, Address, BytesN, Env, String};
use verification_oracle::{
    sha256_commitment, OracleConfig, RevealParams, VerificationOracle, VerificationOracleClient,
};

#[test]
fn test_staking_with_real_token() {
    let e = Env::default();
    e.mock_all_auths();
    e.budget().reset_unlimited();

    let admin = Address::generate(&e);
    let treasury = Address::generate(&e);
    let oracle_1 = Address::generate(&e);
    let oracle_2 = Address::generate(&e);
    let oracle_3 = Address::generate(&e);
    let oracle_4 = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[7u8; 32]);

    // 1. Deploy & initialize a real token for staking
    let token_id = e.register_contract(None, CreditToken);
    let token_client = CreditTokenClient::new(&e, &token_id);
    token_client.initialize(
        &admin,
        &String::from_str(&e, "Staking Token"),
        &String::from_str(&e, "STK"),
        &project_id,
        &String::from_str(&e, "Methodology"),
    );

    // Give oracle_4 some initial staking tokens
    token_client.set_minter(&admin, &admin);
    token_client.mint_to(&admin, &oracle_4, &5000);
    assert_eq!(token_client.balance(&oracle_4), 5000);

    // 2. Deploy & initialize VerificationOracle
    let oracle_id = e.register_contract(None, VerificationOracle);
    let oracle_client = VerificationOracleClient::new(&e, &oracle_id);
    oracle_client.initialize(&admin, &token_id, &treasury);

    // Add first 3 oracles (initially min_stake = 0)
    oracle_client.add_oracle(&admin, &oracle_1);
    oracle_client.add_oracle(&admin, &oracle_2);
    oracle_client.add_oracle(&admin, &oracle_3);

    // 3. Configure min_stake to 1000 and min_oracles to 3
    let config = OracleConfig {
        min_oracles: 3,
        max_oracles: 10,
        quality_threshold_ph: 600,
        quality_threshold_ph_max: 850,
        quality_threshold_turbidity: 100,
        quality_threshold_do: 80,
        quality_threshold_temp: 300,
        credit_per_kg_n: 100,
        credit_per_kg_p: 200,
        staking_token: token_id.clone(),
        treasury: treasury.clone(),
        min_stake: 1000,
        unstake_cooldown_secs: 100,
        commit_phase_secs: 10,
        min_reveal_ledgers: 1,
        max_reveal_ledgers: 20,
        slash_pct_bps: 1000,
        min_slash_amount: 10,
        max_slash_amount: 500,
        window_secs: 3600,
        max_open_windows: 20,
        fee_bps: 0,
    };
    oracle_client.update_config(&admin, &config);

    // Set project config
    token_client.set_minter(&admin, &oracle_id);
    oracle_client.set_project_config(&admin, &project_id, &token_id, &admin, &10, &2, &300);

    // Try adding oracle_4 without staking first -> should panic with "insufficient stake"
    let res = oracle_client.try_add_oracle(&admin, &oracle_4);
    assert!(res.is_err());

    // 4. Staking: The oracle directly stakes 2000 tokens
    oracle_client.stake(&oracle_4, &2000);

    // Verify stake is recorded in verification_oracle
    let stake_info = oracle_client.get_stake(&oracle_4);
    assert_eq!(stake_info.amount, 2000);

    // Verify token balance of oracle decreased, and contract increased
    assert_eq!(token_client.balance(&oracle_4), 3000);
    assert_eq!(token_client.balance(&oracle_id), 2000);

    // Now adding oracle_4 should succeed
    oracle_client.add_oracle(&admin, &oracle_4);

    // 5. Run a simple commit -> reveal cycle with oracle_4
    // Open a submission window
    oracle_client.open_window(&admin, &project_id);

    // Commit
    let nonce = 1u64;
    let reading = (700i64, 10i64, 80i64, 500i64, 250i64, 10i64, 2i64); // ph, turb, do, flow, temp, n, p
    let salt = BytesN::from_array(&e, &[0xB1u8; 32]);
    let commitment = sha256_commitment(
        &e, nonce, reading.0, reading.1, reading.2, reading.3, reading.4, reading.5, reading.6,
        &salt,
    );
    oracle_client.commit_reading(&oracle_4, &project_id, &nonce, &commitment);

    // Transition Commit -> Reveal
    let mut info = e.ledger().get();
    info.timestamp += 15;
    info.sequence_number += 3;
    e.ledger().set(info);
    oracle_client.begin_reveal_phase(&project_id);

    // Advance ledger by 1 sequence to satisfy min_reveal_ledgers check
    let mut info = e.ledger().get();
    info.timestamp += 5;
    info.sequence_number += 1;
    e.ledger().set(info);

    // Reveal
    let params = RevealParams {
        nonce,
        ph: reading.0,
        turbidity: reading.1,
        dissolved_oxygen: reading.2,
        flow_rate: reading.3,
        temperature: reading.4,
        total_nitrogen: reading.5,
        total_phosphorus: reading.6,
        salt,
    };
    oracle_client.reveal_reading(&oracle_4, &project_id, &params);
}
