#![no_std]

use soroban_sdk::{contract, contractimpl, BytesN, Env, Address};

#[contract]
pub struct VerificationOracleContract;

#[contractimpl]
impl VerificationOracleContract {
    /// Initialize the verification oracle contract
    pub fn initialize(env: Env, admin: Address, credit_factory: Address) {
        env.storage().instance().set(&Symbol::new(&env, "admin"), &admin);
        env.storage().instance().set(&Symbol::new(&env, "factory"), &credit_factory);
    }

    /// Commit a sensor reading hash to prevent frontrunning
    pub fn commit_reading(env: Env, oracle: Address, project_id: BytesN<32>, nonce: u64, commitment: BytesN<32>) {
        oracle.require_auth();
        
        let key = (BytesN::from_array(&env, b"commit"), project_id, oracle);
        env.storage().persistent().set(&key, &commitment);
        
        env.events().publish(("reading_committed",), (oracle, nonce));
    }
}
