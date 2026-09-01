use credit_bridge::{CreditBridge, CreditBridgeClient, InboundTransfer};
use credit_token::{CreditToken, CreditTokenClient};
use ed25519_dalek::{Signer, SigningKey};
use governance::{Governance, GovernanceClient};
use soroban_sdk::xdr::ToXdr;
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, Bytes, BytesN, Env, String, Symbol, Vec,
};

// ── Helpers ──

fn deploy_token(e: &Env, admin: &Address) -> (Address, CreditTokenClient<'static>) {
    let contract_id = e.register_contract(None, CreditToken);
    let client = CreditTokenClient::new(e, &contract_id);
    let project_id = BytesN::from_array(e, &[1u8; 32]);
    client.initialize(
        admin,
        &String::from_str(e, "Test Credit"),
        &String::from_str(e, "TST"),
        &project_id,
        &String::from_str(e, "Methodology"),
    );
    (contract_id, client)
}

fn deploy_bridge(e: &Env) -> (Address, CreditBridgeClient<'static>) {
    let contract_id = e.register_contract(None, CreditBridge);
    let client = CreditBridgeClient::new(e, &contract_id);
    (contract_id, client)
}

fn deploy_governance(
    e: &Env,
    admin: &Address,
    members: Vec<Address>,
) -> (Address, GovernanceClient<'static>) {
    let contract_id = e.register_contract(None, Governance);
    let client = GovernanceClient::new(e, &contract_id);
    client.initialize(admin, &members);
    (contract_id, client)
}

fn create_relayer_key(e: &Env, seed: u8) -> (SigningKey, BytesN<32>) {
    let mut seed_bytes = [0u8; 32];
    seed_bytes[0] = seed;
    let signing_key = SigningKey::from_bytes(&seed_bytes);
    let pubkey_bytes = signing_key.verifying_key().to_bytes();
    (signing_key, BytesN::from_array(e, &pubkey_bytes))
}

#[test]
fn test_bridge_deposit_and_withdraw() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let user = Address::generate(&e);

    // Deploy contracts
    let (token_id, token_client) = deploy_token(&e, &admin);
    let (bridge_id, bridge_client) = deploy_bridge(&e);

    // Generate relayers (sorted by public key)
    let mut relayers_info = std::vec![
        create_relayer_key(&e, 1),
        create_relayer_key(&e, 2),
        create_relayer_key(&e, 3),
    ];
    relayers_info.sort_by(|a, b| a.1.cmp(&b.1));

    std::eprintln!("Relayer 0 key: {:?}", relayers_info[0].1);
    std::eprintln!("Relayer 1 key: {:?}", relayers_info[1].1);
    std::eprintln!("Relayer 2 key: {:?}", relayers_info[2].1);

    let relayers = Vec::from_array(
        &e,
        [
            relayers_info[0].1.clone(),
            relayers_info[1].1.clone(),
            relayers_info[2].1.clone(),
        ],
    );

    // Initialize bridge
    let governance_addr = Address::generate(&e);
    let evm_token = BytesN::from_array(&e, &[9u8; 32]);
    bridge_client.initialize(
        &admin,
        &token_id,
        &evm_token,
        &governance_addr,
        &relayers,
        &2,
    );

    // Set bridge contract on credit_token
    token_client.set_bridge(&admin, &bridge_id);
    // Set bridge as minter on credit_token
    token_client.set_minter(&admin, &bridge_id);

    // Mint some tokens to user
    token_client.mint_to(&admin, &user, &1000);
    assert_eq!(token_client.balance(&user), 1000);
    assert_eq!(token_client.total_supply(), 1000);
    assert_eq!(token_client.bridged_to_evm(), 0);

    // Deposit to EVM
    let evm_recipient = BytesN::from_array(&e, &[7u8; 32]);
    bridge_client.deposit(&user, &400, &evm_recipient);

    // Check balances
    assert_eq!(token_client.balance(&user), 600);
    assert_eq!(token_client.total_supply(), 600);
    assert_eq!(token_client.bridged_to_evm(), 400);

    // Withdraw back to Stellar
    let (recipient_type, recipient) = bridge_client.address_to_bytes(&user);
    let transfer = InboundTransfer {
        source_chain: 2,
        destination_chain: 1,
        nonce: 1,
        token_address: BytesN::from_array(&e, &[9u8; 32]),
        sender: BytesN::from_array(&e, &[7u8; 32]),
        recipient_type,
        recipient,
        amount: 250,
    };

    // Serialize transfer
    let msg_bytes = transfer.clone().to_xdr(&e);
    std::eprintln!("Test msg_bytes: {:?}", msg_bytes);
    let mut msg_vec = std::vec![0u8; msg_bytes.len() as usize];
    msg_bytes.copy_into_slice(&mut msg_vec);

    // Sign message using 2 relayers
    let sig1 = relayers_info[0].0.sign(&msg_vec).to_bytes();
    let sig2 = relayers_info[1].0.sign(&msg_vec).to_bytes();

    let signers = Vec::from_array(&e, [relayers_info[0].1.clone(), relayers_info[1].1.clone()]);
    let signatures = Vec::from_array(
        &e,
        [BytesN::from_array(&e, &sig1), BytesN::from_array(&e, &sig2)],
    );

    bridge_client.withdraw(&transfer, &signers, &signatures);

    // Check balances after withdraw
    assert_eq!(token_client.balance(&user), 850);
    assert_eq!(token_client.total_supply(), 850);
    assert_eq!(token_client.bridged_to_evm(), 150);
}

#[test]
#[should_panic(expected = "already processed")]
fn test_replay_protection() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let user = Address::generate(&e);

    let (token_id, token_client) = deploy_token(&e, &admin);
    let (bridge_id, bridge_client) = deploy_bridge(&e);

    let mut relayers_info = std::vec![create_relayer_key(&e, 1), create_relayer_key(&e, 2),];
    relayers_info.sort_by(|a, b| a.1.cmp(&b.1));

    let relayers = Vec::from_array(&e, [relayers_info[0].1.clone(), relayers_info[1].1.clone()]);
    let evm_token = BytesN::from_array(&e, &[9u8; 32]);
    bridge_client.initialize(
        &admin,
        &token_id,
        &evm_token,
        &Address::generate(&e),
        &relayers,
        &2,
    );
    token_client.set_bridge(&admin, &bridge_id);
    token_client.set_minter(&admin, &bridge_id);

    // Bridged amount must be initialized for withdraw to succeed
    // We can deposit 500 first
    token_client.mint_to(&admin, &user, &500);
    bridge_client.deposit(&user, &500, &BytesN::from_array(&e, &[0u8; 32]));

    let (recipient_type, recipient) = bridge_client.address_to_bytes(&user);
    let transfer = InboundTransfer {
        source_chain: 2,
        destination_chain: 1,
        nonce: 10,
        token_address: BytesN::from_array(&e, &[9u8; 32]),
        sender: BytesN::from_array(&e, &[7u8; 32]),
        recipient_type,
        recipient,
        amount: 200,
    };

    let msg_bytes = transfer.clone().to_xdr(&e);
    let mut msg_vec = std::vec![0u8; msg_bytes.len() as usize];
    msg_bytes.copy_into_slice(&mut msg_vec);

    let sig1 = relayers_info[0].0.sign(&msg_vec).to_bytes();
    let sig2 = relayers_info[1].0.sign(&msg_vec).to_bytes();

    let signers = Vec::from_array(&e, [relayers_info[0].1.clone(), relayers_info[1].1.clone()]);
    let signatures = Vec::from_array(
        &e,
        [BytesN::from_array(&e, &sig1), BytesN::from_array(&e, &sig2)],
    );

    // First withdraw succeeds
    bridge_client.withdraw(&transfer, &signers, &signatures);

    // Second withdraw with same transfer must fail (replay protection)
    bridge_client.withdraw(&transfer, &signers, &signatures);
}

#[test]
#[should_panic(expected = "unauthorized signer")]
fn test_invalid_signatures() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let user = Address::generate(&e);

    let (token_id, token_client) = deploy_token(&e, &admin);
    let (bridge_id, bridge_client) = deploy_bridge(&e);

    let mut relayers_info = std::vec![create_relayer_key(&e, 1), create_relayer_key(&e, 2),];
    relayers_info.sort_by(|a, b| a.1.cmp(&b.1));

    let relayers = Vec::from_array(&e, [relayers_info[0].1.clone(), relayers_info[1].1.clone()]);
    let evm_token = BytesN::from_array(&e, &[9u8; 32]);
    bridge_client.initialize(
        &admin,
        &token_id,
        &evm_token,
        &Address::generate(&e),
        &relayers,
        &2,
    );
    token_client.set_bridge(&admin, &bridge_id);
    token_client.set_minter(&admin, &bridge_id);

    token_client.mint_to(&admin, &user, &500);
    bridge_client.deposit(&user, &500, &BytesN::from_array(&e, &[0u8; 32]));

    // Generate non-relayer key
    let non_relayer = create_relayer_key(&e, 9);

    let (recipient_type, recipient) = bridge_client.address_to_bytes(&user);
    let transfer = InboundTransfer {
        source_chain: 2,
        destination_chain: 1,
        nonce: 10,
        token_address: BytesN::from_array(&e, &[9u8; 32]),
        sender: BytesN::from_array(&e, &[7u8; 32]),
        recipient_type,
        recipient,
        amount: 200,
    };

    let msg_bytes = transfer.clone().to_xdr(&e);
    let mut msg_vec = std::vec![0u8; msg_bytes.len() as usize];
    msg_bytes.copy_into_slice(&mut msg_vec);

    let sig1 = relayers_info[0].0.sign(&msg_vec).to_bytes();
    let sig2 = non_relayer.0.sign(&msg_vec).to_bytes();

    let mut signers_info = std::vec![
        (relayers_info[0].1.clone(), sig1),
        (non_relayer.1.clone(), sig2),
    ];
    signers_info.sort_by(|a, b| a.0.cmp(&b.0));

    let signers = Vec::from_array(&e, [signers_info[0].0.clone(), signers_info[1].0.clone()]);
    let signatures = Vec::from_array(
        &e,
        [
            BytesN::from_array(&e, &signers_info[0].1),
            BytesN::from_array(&e, &signers_info[1].1),
        ],
    );

    // Must panic because non_relayer is not authorized
    bridge_client.withdraw(&transfer, &signers, &signatures);
}

#[test]
fn test_governance_pause() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let gov = Address::generate(&e);
    let user = Address::generate(&e);

    let (token_id, token_client) = deploy_token(&e, &admin);
    let (bridge_id, bridge_client) = deploy_bridge(&e);

    let mut relayers_info = std::vec![create_relayer_key(&e, 1), create_relayer_key(&e, 2),];
    relayers_info.sort_by(|a, b| a.1.cmp(&b.1));

    let relayers = Vec::from_array(&e, [relayers_info[0].1.clone(), relayers_info[1].1.clone()]);
    let evm_token = BytesN::from_array(&e, &[9u8; 32]);
    bridge_client.initialize(&admin, &token_id, &evm_token, &gov, &relayers, &2);
    token_client.set_bridge(&admin, &bridge_id);
    token_client.set_minter(&admin, &bridge_id);

    token_client.mint_to(&admin, &user, &500);

    // Pause bridge via governance
    bridge_client.pause(&gov);
    assert!(bridge_client.paused());

    // Deposit must fail
    let res = bridge_client.try_deposit(&user, &100, &BytesN::from_array(&e, &[0u8; 32]));
    assert!(res.is_err());

    // Unpause bridge via governance
    bridge_client.unpause(&gov);
    assert!(!bridge_client.paused());

    // Deposit should now succeed
    bridge_client.deposit(&user, &100, &BytesN::from_array(&e, &[0u8; 32]));
    assert_eq!(token_client.balance(&user), 400);
}

#[test]
fn test_max_supply_enforcement() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let user = Address::generate(&e);

    let (token_id, token_client) = deploy_token(&e, &admin);
    let (bridge_id, bridge_client) = deploy_bridge(&e);

    let mut relayers_info = std::vec![create_relayer_key(&e, 1), create_relayer_key(&e, 2),];
    relayers_info.sort_by(|a, b| a.1.cmp(&b.1));

    let relayers = Vec::from_array(&e, [relayers_info[0].1.clone(), relayers_info[1].1.clone()]);
    let evm_token = BytesN::from_array(&e, &[9u8; 32]);
    bridge_client.initialize(
        &admin,
        &token_id,
        &evm_token,
        &Address::generate(&e),
        &relayers,
        &2,
    );
    token_client.set_bridge(&admin, &bridge_id);
    token_client.set_minter(&admin, &bridge_id);

    // Set max supply to 1000
    token_client.set_max_supply(&admin, &1000);

    // Mint 600 tokens
    token_client.mint_to(&admin, &user, &600);

    // Bridge 500 tokens to EVM
    bridge_client.deposit(&user, &500, &BytesN::from_array(&e, &[0u8; 32]));
    // Circulating supply is 100, bridged to EVM is 500. Total is 600.

    // Try to mint 500 new tokens (this would make total supply 600 + 500 = 1100 > 1000)
    let res = token_client.try_mint_to(&admin, &user, &500);
    assert!(res.is_err()); // Must fail because combined supply would exceed max supply!

    // Minting 400 tokens (making total supply 600 + 400 = 1000) should succeed
    token_client.mint_to(&admin, &user, &400);
    assert_eq!(token_client.balance(&user), 500); // 100 remaining + 400 minted
}

#[test]
fn test_ed25519_verify_sanity() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let user = Address::generate(&e);
    let (token_id, token_client) = deploy_token(&e, &admin);
    let (bridge_id, bridge_client) = deploy_bridge(&e);

    let mut relayers_info = std::vec![create_relayer_key(&e, 1), create_relayer_key(&e, 2),];
    relayers_info.sort_by(|a, b| a.1.cmp(&b.1));

    let relayers = Vec::from_array(&e, [relayers_info[0].1.clone(), relayers_info[1].1.clone()]);

    // Initialize bridge
    let governance_addr = Address::generate(&e);
    let evm_token = BytesN::from_array(&e, &[9u8; 32]);
    bridge_client.initialize(
        &admin,
        &token_id,
        &evm_token,
        &governance_addr,
        &relayers,
        &2,
    );
    token_client.set_bridge(&admin, &bridge_id);
    token_client.set_minter(&admin, &bridge_id);

    // Deposit 400
    token_client.mint_to(&admin, &user, &1000);
    let evm_recipient = BytesN::from_array(&e, &[7u8; 32]);
    bridge_client.deposit(&user, &400, &evm_recipient);

    let (recipient_type, recipient) = bridge_client.address_to_bytes(&user);
    let transfer = InboundTransfer {
        source_chain: 2,
        destination_chain: 1,
        nonce: 1,
        token_address: BytesN::from_array(&e, &[9u8; 32]),
        sender: BytesN::from_array(&e, &[7u8; 32]),
        recipient_type,
        recipient,
        amount: 250,
    };

    let msg_bytes = transfer.clone().to_xdr(&e);
    let mut msg_vec = std::vec![0u8; msg_bytes.len() as usize];
    msg_bytes.copy_into_slice(&mut msg_vec);

    let sig1 = relayers_info[0].0.sign(&msg_vec).to_bytes();
    let sig2 = relayers_info[1].0.sign(&msg_vec).to_bytes();

    let sig1_bytes_n = BytesN::from_array(&e, &sig1);
    let sig2_bytes_n = BytesN::from_array(&e, &sig2);

    let signers = Vec::from_array(&e, [relayers_info[0].1.clone(), relayers_info[1].1.clone()]);
    let signatures = Vec::from_array(&e, [sig1_bytes_n, sig2_bytes_n]);

    // Verify using contract call verifying both signatures in the same call
    assert!(bridge_client.verify_both_sig_test(&transfer, &signers, &signatures));

    // Address round-trip test
    let addr = Address::generate(&e);
    let xdr = addr.clone().to_xdr(&e);
    let mut xdr_vec = std::vec![0u8; xdr.len() as usize];
    xdr.copy_into_slice(&mut xdr_vec);
    std::eprintln!("Generated Address XDR (len {}): {:?}", xdr.len(), xdr_vec);

    let (t, bytes) = bridge_client.address_to_bytes(&addr);
    let mut bytes_vec = [0u8; 32];
    bytes.copy_into_slice(&mut bytes_vec);
    std::eprintln!(
        "address_to_bytes returned type {} and payload: {:?}",
        t,
        bytes_vec
    );
    let restored = bridge_client.bytes_to_address(&t, &bytes);
    assert_eq!(addr, restored);
}
