#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, vec, Address, Bytes, BytesN, Env, Symbol,
    Val, Vec, IntoVal,
};
use soroban_sdk::xdr::{FromXdr, ToXdr};

#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    Governance,
    Paused,
    Relayers,
    Threshold,
    ProcessedMessages(BytesN<32>),
    Nonce,
    EvmToken,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InboundTransfer {
    pub source_chain: u32,
    pub destination_chain: u32,
    pub nonce: u64,
    pub token_address: BytesN<32>, // EVM token address
    pub sender: BytesN<32>,        // EVM sender address (padded)
    pub recipient_type: u32,       // 0 = Account, 1 = Contract
    pub recipient: BytesN<32>,     // Stellar recipient address payload
    pub amount: i128,
}

#[contract]
pub struct CreditBridge;

#[contractimpl]
impl CreditBridge {
    pub fn initialize(
        e: Env,
        admin: Address,
        token: Address,
        evm_token: BytesN<32>,
        governance: Address,
        relayers: Vec<BytesN<32>>,
        threshold: u32,
    ) {
        if e.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        if threshold == 0 || threshold > relayers.len() {
            panic!("invalid threshold");
        }
        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::Token, &token);
        e.storage().instance().set(&DataKey::EvmToken, &evm_token);
        e.storage().instance().set(&DataKey::Governance, &governance);
        e.storage().instance().set(&DataKey::Paused, &false);
        e.storage().instance().set(&DataKey::Relayers, &relayers);
        e.storage().instance().set(&DataKey::Threshold, &threshold);
        e.storage().instance().set(&DataKey::Nonce, &0u64);
    }

    pub fn update_relayers(
        e: Env,
        admin: Address,
        new_relayers: Vec<BytesN<32>>,
        new_threshold: u32,
    ) {
        admin.require_auth();
        let stored_admin: Address = e.storage().instance().get(&DataKey::Admin).unwrap();
        if admin != stored_admin {
            panic!("unauthorized");
        }
        if new_threshold == 0 || new_threshold > new_relayers.len() {
            panic!("invalid threshold");
        }
        e.storage().instance().set(&DataKey::Relayers, &new_relayers);
        e.storage().instance().set(&DataKey::Threshold, &new_threshold);
    }

    pub fn set_token(e: Env, admin: Address, new_token: Address) {
        admin.require_auth();
        let stored_admin: Address = e.storage().instance().get(&DataKey::Admin).unwrap();
        if admin != stored_admin {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Token, &new_token);
    }

    // ── Governance Pause Hook ──
    pub fn pause(e: Env, caller: Address) {
        caller.require_auth();
        let admin = e.storage().instance().get::<_, Address>(&DataKey::Admin).unwrap();
        let gov = e.storage().instance().get::<_, Address>(&DataKey::Governance).unwrap();
        if caller != admin && caller != gov {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Paused, &true);
        e.events().publish((symbol_short!("paused"),), (caller,));
    }

    pub fn unpause(e: Env, caller: Address) {
        caller.require_auth();
        let admin = e.storage().instance().get::<_, Address>(&DataKey::Admin).unwrap();
        let gov = e.storage().instance().get::<_, Address>(&DataKey::Governance).unwrap();
        if caller != admin && caller != gov {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Paused, &false);
        e.events().publish((symbol_short!("unpaused"),), (caller,));
    }

    pub fn paused(e: Env) -> bool {
        e.storage().instance().get(&DataKey::Paused).unwrap_or(false)
    }

    // ── Bridge Deposit (Stellar -> EVM) ──
    pub fn deposit(e: Env, from: Address, amount: i128, evm_recipient: BytesN<32>) {
        if amount <= 0 {
            panic!("amount must be positive");
        }
        if Self::paused(e.clone()) {
            panic!("contract is paused");
        }
        from.require_auth();

        let token: Address = e.storage().instance().get(&DataKey::Token).unwrap();

        // 1. Transfer tokens from depositor to this bridge contract
        let transfer_args: Vec<Val> = vec![
            &e,
            from.into_val(&e),
            e.current_contract_address().into_val(&e),
            amount.into_val(&e),
        ];
        e.invoke_contract::<()>(&token, &Symbol::new(&e, "transfer"), transfer_args);

        // 2. Burn the tokens from this bridge contract's balance
        let burn_args: Vec<Val> = vec![
            &e,
            e.current_contract_address().into_val(&e),
            e.current_contract_address().into_val(&e),
            amount.into_val(&e),
        ];
        e.invoke_contract::<()>(&token, &Symbol::new(&e, "burn"), burn_args);

        // 3. Increment nonce and emit deposit event
        let nonce: u64 = e.storage().instance().get(&DataKey::Nonce).unwrap_or(0);
        e.storage().instance().set(&DataKey::Nonce, &(nonce + 1));

        e.events().publish(
            (Symbol::new(&e, "deposit"),),
            (from, evm_recipient, amount, nonce),
        );
    }

    // ── Bridge Withdraw (EVM -> Stellar) ──
    pub fn withdraw(
        e: Env,
        transfer: InboundTransfer,
        signers: Vec<BytesN<32>>,
        signatures: Vec<BytesN<64>>,
    ) {
        if Self::paused(e.clone()) {
            panic!("contract is paused");
        }
        let token: Address = e.storage().instance().get(&DataKey::Token).unwrap();

        // Verify destination matches this contract/chain
        if transfer.destination_chain != 1 {
            panic!("invalid destination chain");
        }

        let evm_token: BytesN<32> = e.storage().instance().get(&DataKey::EvmToken).unwrap();
        if transfer.token_address != evm_token {
            panic!("invalid token address");
        }

        let msg_bytes = transfer.clone().to_xdr(&e);
        let msg_hash = e.crypto().sha256(&msg_bytes);

        // Replay protection check
        let key = DataKey::ProcessedMessages(msg_hash.clone());
        if e.storage().persistent().has(&key) {
            panic!("already processed");
        }
        e.storage().persistent().set(&key, &true);

        // Verify signatures
        let threshold: u32 = e.storage().instance().get(&DataKey::Threshold).unwrap();
        let relayers: Vec<BytesN<32>> = e.storage().instance().get(&DataKey::Relayers).unwrap();

        if signers.len() != signatures.len() {
            panic!("length mismatch");
        }
        if signers.len() < threshold {
            panic!("insufficient signers");
        }

        let mut last_signer: Option<BytesN<32>> = None;
        let mut valid_sigs = 0;

        for i in 0..signers.len() {
            let signer = signers.get(i).unwrap();
            let sig = signatures.get(i).unwrap();

            // Verify strictly sorted and unique signers to prevent duplicates
            if let Some(ref last) = last_signer {
                if signer <= *last {
                    panic!("signers not sorted or not unique");
                }
            }
            last_signer = Some(signer.clone());

            // Check if signer is an authorized relayer
            let mut is_authorized = false;
            for j in 0..relayers.len() {
                if relayers.get(j).unwrap() == signer {
                    is_authorized = true;
                    break;
                }
            }
            if !is_authorized {
                panic!("unauthorized signer");
            }

            e.crypto().ed25519_verify(&signer, &msg_bytes, &sig);
            valid_sigs += 1;
        }

        if valid_sigs < threshold {
            panic!("insufficient valid signatures");
        }

        // Map recipient to Address deterministically and losslessly
        let recipient_address = bytes_to_address(&e, transfer.recipient_type, &transfer.recipient);

        // Mint tokens to recipient
        let mint_args: Vec<Val> = vec![
            &e,
            e.current_contract_address().into_val(&e),
            recipient_address.into_val(&e),
            transfer.amount.into_val(&e),
        ];
        e.invoke_contract::<()>(&token, &Symbol::new(&e, "mint_to"), mint_args);

        e.events().publish(
            (Symbol::new(&e, "withdraw"),),
            (recipient_address, transfer.amount, transfer.nonce),
        );
    }

    pub fn get_nonce(e: Env) -> u64 {
        e.storage().instance().get(&DataKey::Nonce).unwrap_or(0)
    }

    pub fn is_processed(e: Env, msg_hash: BytesN<32>) -> bool {
        e.storage().persistent().has(&DataKey::ProcessedMessages(msg_hash))
    }

    pub fn address_to_bytes(e: Env, address: Address) -> (u32, BytesN<32>) {
        address_to_bytes(&e, &address)
    }

    pub fn bytes_to_address(e: Env, address_type: u32, payload: BytesN<32>) -> Address {
        bytes_to_address(&e, address_type, &payload)
    }

    pub fn verify_sig_test(
        e: Env,
        transfer: InboundTransfer,
        signer: BytesN<32>,
        sig: BytesN<64>,
    ) -> bool {
        let msg_bytes = transfer.to_xdr(&e);
        e.events().publish((Symbol::new(&e, "verify_sig_test_msg_bytes"),), msg_bytes.clone());
        e.crypto().ed25519_verify(&signer, &msg_bytes, &sig);
        true
    }

    pub fn verify_both_sig_test(
        e: Env,
        transfer: InboundTransfer,
        signers: Vec<BytesN<32>>,
        sigs: Vec<BytesN<64>>,
    ) -> bool {
        let msg_bytes = transfer.to_xdr(&e);
        for i in 0..signers.len() {
            let signer = signers.get(i).unwrap();
            let sig = sigs.get(i).unwrap();
            e.crypto().ed25519_verify(&signer, &msg_bytes, &sig);
        }
        true
    }
}

// Deterministic and lossless address mapping functions
pub fn address_to_bytes(e: &Env, address: &Address) -> (u32, BytesN<32>) {
    let xdr = address.to_xdr(e);
    let len = xdr.len();
    if len == 40 {
        // Contract: 4 bytes ScVal discriminant (18) + 4 bytes ScAddress discriminant (1) + 32 bytes hash
        let mut payload = [0u8; 32];
        for i in 0..32 {
            payload[i] = xdr.get((i + 8) as u32).unwrap();
        }
        (1, BytesN::from_array(e, &payload))
    } else if len == 44 {
        // Account: 4 bytes ScVal discriminant (18) + 4 bytes ScAddress discriminant (0) + 4 bytes PublicKey discriminant (0) + 32 bytes pubkey
        let mut payload = [0u8; 32];
        for i in 0..32 {
            payload[i] = xdr.get((i + 12) as u32).unwrap();
        }
        (0, BytesN::from_array(e, &payload))
    } else {
        panic!("invalid address length");
    }
}

pub fn bytes_to_address(e: &Env, address_type: u32, payload: &BytesN<32>) -> Address {
    let mut payload_arr = [0u8; 32];
    payload.copy_into_slice(&mut payload_arr);

    if address_type == 1 {
        // Contract
        let mut xdr = Bytes::new(e);
        xdr.append(&Bytes::from_array(e, &[0, 0, 0, 18, 0, 0, 0, 1]));
        xdr.append(&Bytes::from_array(e, &payload_arr));
        Address::from_xdr(e, &xdr).unwrap()
    } else if address_type == 0 {
        // Account
        let mut xdr = Bytes::new(e);
        xdr.append(&Bytes::from_array(e, &[0, 0, 0, 18, 0, 0, 0, 0, 0, 0, 0, 0]));
        xdr.append(&Bytes::from_array(e, &payload_arr));
        Address::from_xdr(e, &xdr).unwrap()
    } else {
        panic!("invalid address type");
    }
}
