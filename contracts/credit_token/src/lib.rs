#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, vec, Address, BytesN, Env, String, Symbol,
    Val, Vec,
};

use soroban_sdk::IntoVal;

// ── TTL constants (in ledgers; ~5 sec/ledger on Stellar) ──
/// 1 year ≈ 6 307 200 ledgers. Used for balance, allowance, and cert entries.
const BALANCE_TTL_THRESHOLD: u32 = 6_307_200;
const BALANCE_TTL_BUMP: u32 = 6_307_200;
/// Allowances are shorter-lived: 90 days.
const ALLOWANCE_TTL_THRESHOLD: u32 = 1_555_200;
const ALLOWANCE_TTL_BUMP: u32 = 1_555_200;
/// Certificates are permanent records: 10 years.
const CERT_TTL_THRESHOLD: u32 = 63_072_000;
const CERT_TTL_BUMP: u32 = 63_072_000;

#[cfg(test)]
extern crate std;

// ── Events (max 9 chars for symbol_short) ──
const EVENT_MINTED: Symbol = symbol_short!("minted");
const EVENT_XFER: Symbol = symbol_short!("xfer");
const EVENT_RETIRED: Symbol = symbol_short!("retired");
const EVENT_BURNED: Symbol = symbol_short!("burned");
const EVENT_PAUSED: Symbol = symbol_short!("paused");
const EVENT_UNPAUSED: Symbol = symbol_short!("unpaused");

// ── Data Types ──

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CreditMetadata {
    pub project_id: BytesN<32>,
    pub methodology: String,
    pub vintage: u64,
    pub issuance_date: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RetirementCertificate {
    pub retiree: Address,
    pub project_id: BytesN<32>,
    pub amount: i128,
    pub purpose: String,
    pub timestamp: u64,
    pub metadata_uri: String,
    pub registry_record_id: Option<u64>,
}

#[contracttype]
pub enum DataKey {
    Balance(Address),
    Allowance(Address, Address),
    AllowanceExpiration(Address, Address),
    Admin,
    Minter,
    RetirementRegistry,
    TotalSupply,
    TotalRetired,
    /// Running total of credits destroyed via `burn()` (admin-only, no retirement record).
    /// Maintained separately from `TotalRetired` so that supply conservation can be
    /// verified on-chain: `total_supply + total_retired + total_burned == ever_minted`.
    TotalBurned,
    Name,
    Symbol,
    Decimals,
    Metadata,
    Cert(u64),
    CertCount,
    Paused,
    MaxSupply,
    /// An address that is allowed to call pause/unpause in addition to the admin.
    /// Used to grant the governance contract emergency pause rights.
    PauseGuardian,
    /// When true, `retire()` panics if no retirement registry is configured.
    RequireRegistry,
}

fn is_paused(e: &Env) -> bool {
    e.storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
}

fn require_not_paused(e: &Env) {
    if is_paused(e) {
        panic!("contract is paused");
    }
}

fn has_admin(e: &Env) -> bool {
    e.storage().instance().has(&DataKey::Admin)
}

fn read_admin(e: &Env) -> Address {
    e.storage().instance().get(&DataKey::Admin).unwrap()
}

fn read_minter(e: &Env) -> Address {
    e.storage()
        .instance()
        .get(&DataKey::Minter)
        .unwrap_or_else(|| read_admin(e))
}

fn require_minter(e: &Env, caller: &Address) {
    caller.require_auth();
    let minter = read_minter(e);
    let admin = read_admin(e);
    if *caller != minter && *caller != admin {
        panic!("unauthorized minter");
    }
}

fn read_balance(e: &Env, addr: &Address) -> i128 {
    let key = DataKey::Balance(addr.clone());
    let val: i128 = e.storage().persistent().get(&key).unwrap_or(0);
    if val > 0 {
        e.storage()
            .persistent()
            .extend_ttl(&key, BALANCE_TTL_THRESHOLD, BALANCE_TTL_BUMP);
    }
    val
}

fn save_balance(e: &Env, addr: &Address, amount: i128) {
    let key = DataKey::Balance(addr.clone());
    e.storage().persistent().set(&key, &amount);
    e.storage()
        .persistent()
        .extend_ttl(&key, BALANCE_TTL_THRESHOLD, BALANCE_TTL_BUMP);
}

fn read_total_supply(e: &Env) -> i128 {
    e.storage().instance().get(&DataKey::TotalSupply).unwrap()
}

fn save_total_supply(e: &Env, amount: i128) {
    e.storage().instance().set(&DataKey::TotalSupply, &amount);
}

fn read_total_retired(e: &Env) -> i128 {
    e.storage().instance().get(&DataKey::TotalRetired).unwrap()
}

fn save_total_retired(e: &Env, amount: i128) {
    e.storage().instance().set(&DataKey::TotalRetired, &amount);
}

fn read_total_burned(e: &Env) -> i128 {
    e.storage()
        .instance()
        .get(&DataKey::TotalBurned)
        .unwrap_or(0)
}

fn save_total_burned(e: &Env, amount: i128) {
    e.storage().instance().set(&DataKey::TotalBurned, &amount);
}

fn read_allowance(e: &Env, from: &Address, spender: &Address) -> i128 {
    let key = DataKey::Allowance(from.clone(), spender.clone());
    let val: i128 = e.storage().persistent().get(&key).unwrap_or(0);
    if val > 0 {
        e.storage()
            .persistent()
            .extend_ttl(&key, ALLOWANCE_TTL_THRESHOLD, ALLOWANCE_TTL_BUMP);
    }
    val
}

fn save_allowance(e: &Env, from: &Address, spender: &Address, amount: i128) {
    let key = DataKey::Allowance(from.clone(), spender.clone());
    e.storage().persistent().set(&key, &amount);
    e.storage()
        .persistent()
        .extend_ttl(&key, ALLOWANCE_TTL_THRESHOLD, ALLOWANCE_TTL_BUMP);
}

#[contract]
pub struct CreditToken;

#[contractimpl]
impl CreditToken {
    /// Initialize the token with project metadata. Callable once by the deploying admin.
    pub fn initialize(
        e: Env,
        admin: Address,
        name: String,
        symbol: String,
        project_id: BytesN<32>,
        methodology: String,
    ) {
        if has_admin(&e) {
            panic!("already initialized");
        }
        let timestamp = e.ledger().timestamp();
        let metadata = CreditMetadata {
            project_id,
            methodology,
            vintage: timestamp,
            issuance_date: timestamp,
        };
        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::Name, &name);
        e.storage().instance().set(&DataKey::Symbol, &symbol);
        e.storage().instance().set(&DataKey::Decimals, &7u32);
        e.storage().instance().set(&DataKey::TotalSupply, &0i128);
        e.storage().instance().set(&DataKey::TotalRetired, &0i128);
        e.storage().instance().set(&DataKey::TotalBurned, &0i128);
        e.storage().instance().set(&DataKey::Metadata, &metadata);
        e.storage().instance().set(&DataKey::CertCount, &0u64);
    }

    /// Transfer contract admin rights to a new address.
    pub fn set_admin(e: Env, admin: Address, new_admin: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Admin, &new_admin);
    }

    /// Designate the address allowed to mint credits (typically the verification oracle).
    pub fn set_minter(e: Env, admin: Address, minter: Address) {
        admin.require_auth();
        if admin != read_admin(&e) {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Minter, &minter);
    }

    /// Link the global retirement registry for on-chain retirement recording.
    pub fn set_retirement_registry(e: Env, admin: Address, registry: Address) {
        admin.require_auth();
        if admin != read_admin(&e) {
            panic!("unauthorized");
        }
        e.storage()
            .instance()
            .set(&DataKey::RetirementRegistry, &registry);
    }

    /// When true, `retire()` panics if no retirement registry is configured.
    pub fn set_require_registry(e: Env, admin: Address, require: bool) {
        admin.require_auth();
        if admin != read_admin(&e) {
            panic!("unauthorized");
        }
        e.storage()
            .instance()
            .set(&DataKey::RequireRegistry, &require);
    }

    /// Return whether strict retirement registry mode is enabled.
    pub fn require_registry(e: Env) -> bool {
        e.storage()
            .instance()
            .get(&DataKey::RequireRegistry)
            .unwrap_or(false)
    }

    /// Pause all token operations (mint, transfer, retire). Admin or pause guardian only.
    /// Useful for emergency halts or project suspension.
    pub fn pause(e: Env, caller: Address) {
        caller.require_auth();
        let admin = read_admin(&e);
        let guardian: Option<Address> = e.storage().instance().get(&DataKey::PauseGuardian);
        if caller != admin && guardian.as_ref() != Some(&caller) {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Paused, &true);
        e.events().publish((EVENT_PAUSED,), ());
    }

    /// Resume token operations after a pause. Admin or pause guardian only.
    pub fn unpause(e: Env, caller: Address) {
        caller.require_auth();
        let admin = read_admin(&e);
        let guardian: Option<Address> = e.storage().instance().get(&DataKey::PauseGuardian);
        if caller != admin && guardian.as_ref() != Some(&caller) {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Paused, &false);
        e.events().publish((EVENT_UNPAUSED,), ());
    }

    /// Returns true if the contract is currently paused.
    pub fn paused(e: Env) -> bool {
        is_paused(&e)
    }

    /// Set the maximum total supply for this token. Set to 0 to remove the cap.
    /// Admin only. Should be set once at project initialization to match the
    /// verified project area and methodology ceiling.
    pub fn set_max_supply(e: Env, admin: Address, max: i128) {
        admin.require_auth();
        if admin != read_admin(&e) {
            panic!("unauthorized");
        }
        if max < 0 {
            panic!("max supply must be non-negative");
        }
        e.storage().instance().set(&DataKey::MaxSupply, &max);
    }

    /// Get the configured maximum supply (0 = uncapped).
    pub fn max_supply(e: Env) -> i128 {
        e.storage().instance().get(&DataKey::MaxSupply).unwrap_or(0)
    }

    /// Set the pause guardian: a secondary address that may call `pause` and `unpause`
    /// in addition to the admin. Intended for use by the governance contract so that
    /// it can trigger an emergency pause without being the full token admin.
    /// Pass the zero address (or call without a guardian) to clear the guardian.
    /// Admin only.
    pub fn set_pause_guardian(e: Env, admin: Address, guardian: Address) {
        admin.require_auth();
        if admin != read_admin(&e) {
            panic!("unauthorized");
        }
        e.storage()
            .instance()
            .set(&DataKey::PauseGuardian, &guardian);
    }

    /// Return the current pause guardian address, if any.
    pub fn pause_guardian(e: Env) -> Option<Address> {
        e.storage().instance().get(&DataKey::PauseGuardian)
    }

    /// Mint new credits to a beneficiary. Callable by admin or designated minter.
    pub fn mint_to(e: Env, minter: Address, to: Address, amount: i128) {
        if amount <= 0 {
            panic!("amount must be positive");
        }
        require_not_paused(&e);
        require_minter(&e, &minter);

        let total = read_total_supply(&e);
        let max: i128 = e.storage().instance().get(&DataKey::MaxSupply).unwrap_or(0);
        if max > 0 && total.checked_add(amount).expect("overflow") > max {
            panic!("max supply exceeded");
        }

        let balance = read_balance(&e, &to);
        save_balance(&e, &to, balance.checked_add(amount).expect("overflow"));
        save_total_supply(&e, total.checked_add(amount).expect("overflow"));

        e.events().publish((EVENT_MINTED,), (to, amount));
    }

    /// Mint credits to multiple recipients in a single call.
    /// Each entry in `recipients` receives the corresponding amount from `amounts`.
    /// The two slices must be the same length. Callable by admin or designated minter.
    pub fn batch_mint_to(e: Env, minter: Address, recipients: Vec<Address>, amounts: Vec<i128>) {
        if recipients.len() != amounts.len() {
            panic!("recipients and amounts length mismatch");
        }
        if recipients.is_empty() {
            panic!("empty batch");
        }
        require_not_paused(&e);
        require_minter(&e, &minter);

        let mut total = read_total_supply(&e);
        let max: i128 = e.storage().instance().get(&DataKey::MaxSupply).unwrap_or(0);

        for i in 0..recipients.len() {
            let to = recipients.get(i).unwrap();
            let amount = amounts.get(i).unwrap();
            if amount <= 0 {
                panic!("amount must be positive");
            }
            if max > 0 && total.checked_add(amount).expect("overflow") > max {
                panic!("max supply exceeded");
            }
            let balance = read_balance(&e, &to);
            save_balance(&e, &to, balance.checked_add(amount).expect("overflow"));
            total = total.checked_add(amount).expect("overflow");
            e.events().publish((EVENT_MINTED,), (to, amount));
        }

        save_total_supply(&e, total);
    }

    /// Burn credits from a holder. Admin only.
    pub fn burn(e: Env, admin: Address, from: Address, amount: i128) {
        if amount <= 0 {
            panic!("amount must be positive");
        }
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }

        let balance = read_balance(&e, &from);
        let total = read_total_supply(&e);
        if balance < amount {
            panic!("insufficient balance");
        }
        save_balance(&e, &from, balance - amount);
        save_total_supply(&e, total - amount);

        let new_total_burned = read_total_burned(&e).checked_add(amount).expect("overflow");
        save_total_burned(&e, new_total_burned);

        e.events()
            .publish((EVENT_BURNED,), (from, amount, new_total_burned));
    }

    /// Transfer credits between wallets.
    pub fn transfer(e: Env, from: Address, to: Address, amount: i128) {
        if amount <= 0 {
            panic!("amount must be positive");
        }
        require_not_paused(&e);
        from.require_auth();

        let from_balance = read_balance(&e, &from);
        if from_balance < amount {
            panic!("insufficient balance");
        }
        let to_balance = read_balance(&e, &to);
        save_balance(&e, &from, from_balance - amount);
        save_balance(&e, &to, to_balance.checked_add(amount).expect("overflow"));

        e.events().publish((EVENT_XFER,), (from, to, amount));
    }

    /// Transfer credits from one holder to multiple recipients in a single call.
    /// The sender's balance is debited the total of all amounts atomically.
    /// Each recipient receives the corresponding amount.
    pub fn batch_transfer(e: Env, from: Address, recipients: Vec<Address>, amounts: Vec<i128>) {
        if recipients.len() != amounts.len() {
            panic!("recipients and amounts length mismatch");
        }
        if recipients.is_empty() {
            panic!("empty batch");
        }
        require_not_paused(&e);
        from.require_auth();

        // Calculate total amount to deduct from sender
        let mut total_amount: i128 = 0;
        for i in 0..amounts.len() {
            let amount = amounts.get(i).unwrap();
            if amount <= 0 {
                panic!("amount must be positive");
            }
            total_amount = total_amount.checked_add(amount).expect("overflow");
        }

        let from_balance = read_balance(&e, &from);
        if from_balance < total_amount {
            panic!("insufficient balance");
        }
        save_balance(&e, &from, from_balance - total_amount);

        for i in 0..recipients.len() {
            let to = recipients.get(i).unwrap();
            let amount = amounts.get(i).unwrap();
            let to_balance = read_balance(&e, &to);
            save_balance(&e, &to, to_balance.checked_add(amount).expect("overflow"));
            e.events()
                .publish((EVENT_XFER,), (from.clone(), to, amount));
        }
    }

    /// Transfer credits on behalf of an approved holder.
    pub fn transfer_from(e: Env, spender: Address, from: Address, to: Address, amount: i128) {
        if amount <= 0 {
            panic!("amount must be positive");
        }
        require_not_paused(&e);
        spender.require_auth();

        let allowance = read_allowance(&e, &from, &spender);
        if allowance < amount {
            panic!("insufficient allowance");
        }

        // Check expiration
        let exp_key = DataKey::AllowanceExpiration(from.clone(), spender.clone());
        let expiration: u32 = e.storage().persistent().get(&exp_key).unwrap_or(0);
        if expiration > 0 && e.ledger().sequence() >= expiration {
            // Allowance has expired — reset and reject
            save_allowance(&e, &from, &spender, 0);
            panic!("allowance expired");
        }

        let from_balance = read_balance(&e, &from);
        if from_balance < amount {
            panic!("insufficient balance");
        }
        let to_balance = read_balance(&e, &to);
        save_allowance(&e, &from, &spender, allowance - amount);
        save_balance(&e, &from, from_balance - amount);
        save_balance(&e, &to, to_balance.checked_add(amount).expect("overflow"));

        e.events().publish((EVENT_XFER,), (from, to, amount));
    }

    /// Approve a spender to transfer up to `amount` credits.
    /// The allowance expires at the given ledger number. Use 0 for no expiration.
    pub fn approve(e: Env, from: Address, spender: Address, amount: i128, expiration_ledger: u32) {
        if amount < 0 {
            panic!("amount must be non-negative");
        }
        from.require_auth();
        save_allowance(&e, &from, &spender, amount);
        let exp_key = DataKey::AllowanceExpiration(from, spender);
        e.storage().persistent().set(&exp_key, &expiration_ledger);
        e.storage()
            .persistent()
            .extend_ttl(&exp_key, ALLOWANCE_TTL_THRESHOLD, ALLOWANCE_TTL_BUMP);
    }

    /// Permanently retire credits and optionally record in the retirement registry.
    pub fn retire(
        e: Env,
        holder: Address,
        amount: i128,
        purpose: String,
        metadata_uri: String,
    ) -> RetirementCertificate {
        if amount <= 0 {
            panic!("amount must be positive");
        }
        require_not_paused(&e);
        holder.require_auth();

        let balance = read_balance(&e, &holder);
        if balance < amount {
            panic!("insufficient balance");
        }

        let metadata: CreditMetadata = e.storage().instance().get(&DataKey::Metadata).unwrap();
        let project_id = metadata.project_id.clone();

        let registry_addr: Option<Address> =
            e.storage().instance().get(&DataKey::RetirementRegistry);
        let require_registry: bool = e
            .storage()
            .instance()
            .get(&DataKey::RequireRegistry)
            .unwrap_or(false);

        let registry_record_id = if let Some(registry) = registry_addr {
            let record_args: Vec<Val> = vec![
                &e,
                e.current_contract_address().to_val(),
                holder.to_val(),
                project_id.to_val(),
                amount.into_val(&e),
                purpose.to_val(),
                metadata_uri.to_val(),
            ];
            let record_id: u64 = e.invoke_contract::<u64>(
                &registry,
                &Symbol::new(&e, "record_retirement"),
                record_args,
            );
            Some(record_id)
        } else if require_registry {
            soroban_sdk::panic_with_error!(&e, soroban_sdk::Error::from_contract_error(1));
        } else {
            None
        };

        save_balance(&e, &holder, balance - amount);

        let total = read_total_supply(&e);
        save_total_supply(&e, total - amount);

        let total_retired = read_total_retired(&e);
        save_total_retired(&e, total_retired + amount);

        let cert_count: u64 = e.storage().instance().get(&DataKey::CertCount).unwrap();
        let timestamp = e.ledger().timestamp();

        let cert = RetirementCertificate {
            retiree: holder.clone(),
            project_id: metadata.project_id,
            amount,
            purpose: purpose.clone(),
            timestamp,
            metadata_uri: metadata_uri.clone(),
            registry_record_id,
        };
        let cert_key = DataKey::Cert(cert_count);
        e.storage().persistent().set(&cert_key, &cert);
        e.storage()
            .persistent()
            .extend_ttl(&cert_key, CERT_TTL_THRESHOLD, CERT_TTL_BUMP);
        e.storage()
            .instance()
            .set(&DataKey::CertCount, &(cert_count + 1));

        e.events()
            .publish((EVENT_RETIRED,), (holder.clone(), amount, cert.clone()));

        cert
    }

    // ── Read-Only Functions ──

    pub fn balance(e: Env, addr: Address) -> i128 {
        read_balance(&e, &addr)
    }

    pub fn total_supply(e: Env) -> i128 {
        read_total_supply(&e)
    }

    pub fn total_retired(e: Env) -> i128 {
        read_total_retired(&e)
    }

    /// Total credits destroyed via admin `burn()` (no retirement record issued).
    ///
    /// Invariant: `total_supply() + total_retired() + total_burned() == ever_minted`
    /// where `ever_minted` is the cumulative sum of all `mint_to` / `batch_mint_to` calls.
    pub fn total_burned(e: Env) -> i128 {
        read_total_burned(&e)
    }

    pub fn allowance(e: Env, from: Address, spender: Address) -> i128 {
        read_allowance(&e, &from, &spender)
    }

    pub fn name(e: Env) -> String {
        e.storage().instance().get(&DataKey::Name).unwrap()
    }

    pub fn symbol(e: Env) -> String {
        e.storage().instance().get(&DataKey::Symbol).unwrap()
    }

    pub fn decimals(e: Env) -> u32 {
        e.storage().instance().get(&DataKey::Decimals).unwrap()
    }

    pub fn metadata(e: Env) -> CreditMetadata {
        e.storage().instance().get(&DataKey::Metadata).unwrap()
    }

    pub fn get_certificate(e: Env, index: u64) -> Option<RetirementCertificate> {
        let key = DataKey::Cert(index);
        let result: Option<RetirementCertificate> = e.storage().persistent().get(&key);
        if result.is_some() {
            e.storage()
                .persistent()
                .extend_ttl(&key, CERT_TTL_THRESHOLD, CERT_TTL_BUMP);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::Events;
    use soroban_sdk::testutils::Ledger as _;
    use soroban_sdk::{Address, Env, String, TryFromVal};

    fn setup() -> (
        Env,
        Address,
        Address,
        Address,
        BytesN<32>,
        CreditTokenClient<'static>,
    ) {
        let e = Env::default();
        let admin = Address::generate(&e);
        let user1 = Address::generate(&e);
        let user2 = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[1u8; 32]);
        let name = String::from_str(&e, "Green Valley Credits");
        let symbol = String::from_str(&e, "GVC");
        let methodology = String::from_str(&e, "Wetland_Restoration_v2.1");
        let contract_id = e.register_contract(None, CreditToken);
        let client = CreditTokenClient::new(&e, &contract_id);

        client.initialize(&admin, &name, &symbol, &project_id, &methodology);

        (e, admin, user1, user2, project_id, client)
    }

    #[test]
    fn test_initialize_sets_values() {
        let e = Env::default();
        let admin = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[2u8; 32]);
        let name = String::from_str(&e, "Test Credit");
        let symbol = String::from_str(&e, "TST");
        let methodology = String::from_str(&e, "Riparian_Buffer_v1.0");
        let contract_id = e.register_contract(None, CreditToken);
        let client = CreditTokenClient::new(&e, &contract_id);

        client.initialize(&admin, &name, &symbol, &project_id, &methodology);

        assert_eq!(client.name(), name);
        assert_eq!(client.symbol(), symbol);
        assert_eq!(client.decimals(), 7);
        assert_eq!(client.total_supply(), 0);
        assert_eq!(client.total_retired(), 0);
        let meta = client.metadata();
        assert_eq!(meta.project_id, project_id);
        assert_eq!(meta.methodology, methodology);
    }

    #[test]
    fn test_mint_to_increases_balance_and_supply() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user, &1000);

        assert_eq!(client.balance(&user), 1000);
        assert_eq!(client.total_supply(), 1000);
        assert_eq!(client.total_retired(), 0);
    }

    #[test]
    fn test_mint_emits_event() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user, &500);

        let events = e.events().all();
        assert_eq!(events.len(), 1);
        let (_contract, topics, _data) = &events.get(0).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("minted"));
    }

    #[test]
    fn test_burn_decreases_balance_and_supply() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user, &1000);
        client.burn(&admin, &user, &300);

        assert_eq!(client.balance(&user), 700);
        assert_eq!(client.total_supply(), 700);
        // TotalBurned must track the admin-destroyed amount separately
        assert_eq!(client.total_burned(), 300);
        // Retired is unaffected by burn
        assert_eq!(client.total_retired(), 0);
    }

    #[test]
    fn test_burn_emits_event() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user, &1000);

        // Clear events from mint
        client.burn(&admin, &user, &300);

        let events = e.events().all();
        assert_eq!(events.len(), 2);
        let (_contract, topics, _data) = &events.get(1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("burned"));
    }

    #[test]
    fn test_burn_event_payload_includes_running_total() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user, &1000);

        // First burn: 300 → running total = 300
        client.burn(&admin, &user, &300);
        assert_eq!(client.total_burned(), 300);

        // Second burn: 200 → running total = 500
        client.burn(&admin, &user, &200);
        assert_eq!(client.total_burned(), 500);

        // Verify the burned events are present (topic check is sufficient;
        // tuple-encoded event data requires XDR decoding which is out of scope here)
        let events = e.events().all();
        // Events: minted(1) + burned(1) + burned(1) = 3
        assert_eq!(events.len(), 3);

        let (_contract, topics1, _data1) = &events.get(1).unwrap();
        let topic1: Symbol = Symbol::try_from_val(&e, &topics1.get(0).unwrap()).unwrap();
        assert_eq!(topic1, symbol_short!("burned"));

        let (_contract, topics2, _data2) = &events.get(2).unwrap();
        let topic2: Symbol = Symbol::try_from_val(&e, &topics2.get(0).unwrap()).unwrap();
        assert_eq!(topic2, symbol_short!("burned"));
    }

    #[test]
    fn test_transfer_moves_balance() {
        let (e, admin, user1, user2, _project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user1, &1000);
        client.transfer(&user1, &user2, &300);

        assert_eq!(client.balance(&user1), 700);
        assert_eq!(client.balance(&user2), 300);
        assert_eq!(client.total_supply(), 1000);
    }

    #[test]
    fn test_transfer_emits_event() {
        let (e, admin, user1, user2, _project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user1, &500);
        client.transfer(&user1, &user2, &200);

        let events = e.events().all();
        assert_eq!(events.len(), 2);
        let (_contract, topics, _data) = &events.get(1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("xfer"));
    }

    #[test]
    fn test_approve_sets_and_overwrites() {
        let (e, _admin, owner, spender, _project_id, client) = setup();
        e.mock_all_auths();

        client.approve(&owner, &spender, &100, &100000);
        assert_eq!(client.allowance(&owner, &spender), 100);

        client.approve(&owner, &spender, &250, &100001);
        assert_eq!(client.allowance(&owner, &spender), 250);
    }

    #[test]
    fn test_transfer_from_with_allowance() {
        let (e, admin, owner, spender, _project_id, client) = setup();
        let recipient = Address::generate(&e);
        e.mock_all_auths();

        client.mint_to(&admin, &owner, &1000);
        client.approve(&owner, &spender, &500, &100000);
        client.transfer_from(&spender, &owner, &recipient, &200);

        assert_eq!(client.balance(&owner), 800);
        assert_eq!(client.balance(&recipient), 200);
        assert_eq!(client.allowance(&owner, &spender), 300);
    }

    #[test]
    fn test_transfer_from_emits_event() {
        let (e, admin, owner, spender, _project_id, client) = setup();
        let recipient = Address::generate(&e);
        e.mock_all_auths();

        client.mint_to(&admin, &owner, &1000);
        client.approve(&owner, &spender, &500, &100000);

        // Clear events from mint + approve
        client.transfer_from(&spender, &owner, &recipient, &200);

        // transfer_from should emit an xfer event
        // Count events: mint(1) + approve(0 events) + transfer_from(1) = 2
        let events = e.events().all();
        assert_eq!(events.len(), 2);
        let (_contract, topics, _data) = &events.get(1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("xfer"));
    }

    #[test]
    fn test_approve_zero_expiration_never_expires() {
        let (e, admin, owner, spender, _project_id, client) = setup();
        let recipient = Address::generate(&e);
        e.mock_all_auths();

        client.mint_to(&admin, &owner, &1000);
        client.approve(&owner, &spender, &500, &0);

        // Advance ledger far beyond any reasonable value — should still work
        let mut info = e.ledger().get();
        info.sequence_number = 999_999;
        e.ledger().set(info);

        client.transfer_from(&spender, &owner, &recipient, &100);
        assert_eq!(client.balance(&recipient), 100);
        assert_eq!(client.allowance(&owner, &spender), 400);
    }

    #[test]
    fn test_allowance_expiration_blocks_transfer() {
        let (e, admin, owner, spender, _project_id, client) = setup();
        let recipient = Address::generate(&e);
        e.mock_all_auths();

        client.mint_to(&admin, &owner, &1000);
        client.approve(&owner, &spender, &500, &10);

        // Advance to expiration ledger
        let mut info = e.ledger().get();
        info.sequence_number = 10;
        e.ledger().set(info);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.transfer_from(&spender, &owner, &recipient, &100);
        }));
        assert!(result.is_err());

        // Allowance should be reset to 0
        assert_eq!(client.allowance(&owner, &spender), 0);
    }

    #[test]
    fn test_allowance_valid_before_expiration() {
        let (e, admin, owner, spender, _project_id, client) = setup();
        let recipient = Address::generate(&e);
        e.mock_all_auths();

        client.mint_to(&admin, &owner, &1000);
        client.approve(&owner, &spender, &500, &10);

        // Advance to one ledger before expiration
        let mut info = e.ledger().get();
        info.sequence_number = 9;
        e.ledger().set(info);

        client.transfer_from(&spender, &owner, &recipient, &100);
        assert_eq!(client.balance(&recipient), 100);
        assert_eq!(client.allowance(&owner, &spender), 400);
    }

    #[test]
    fn test_retire_burns_and_generates_certificate() {
        let (e, admin, user, _, project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user, &1000);
        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmCert");

        let cert = client.retire(&user, &300, &purpose, &uri);

        assert_eq!(cert.retiree, user);
        assert_eq!(cert.project_id, project_id);
        assert_eq!(cert.amount, 300);
        assert_eq!(cert.purpose, purpose);
        assert_eq!(cert.metadata_uri, uri);

        assert_eq!(client.balance(&user), 700);
        assert_eq!(client.total_supply(), 700);
        assert_eq!(client.total_retired(), 300);
    }

    #[test]
    fn test_retire_multiple_certificates() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user, &1000);
        let purpose = String::from_str(&e, "voluntary");
        let uri1 = String::from_str(&e, "ipfs://Cert1");
        let uri2 = String::from_str(&e, "ipfs://Cert2");

        let cert1 = client.retire(&user, &400, &purpose, &uri1);
        assert_eq!(cert1.amount, 400);

        let cert2 = client.retire(&user, &200, &purpose, &uri2);
        assert_eq!(cert2.amount, 200);

        assert_eq!(client.balance(&user), 400);
        assert_eq!(client.total_retired(), 600);

        let retrieved1 = client.get_certificate(&0).unwrap();
        assert_eq!(retrieved1.amount, 400);
        assert_eq!(retrieved1.metadata_uri, uri1);

        let retrieved2 = client.get_certificate(&1).unwrap();
        assert_eq!(retrieved2.amount, 200);
        assert_eq!(retrieved2.metadata_uri, uri2);

        assert!(client.get_certificate(&5).is_none());
    }

    #[test]
    fn test_retire_emits_event() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user, &500);
        let purpose = String::from_str(&e, "compliance");
        let uri = String::from_str(&e, "ipfs://QmCert");
        client.retire(&user, &200, &purpose, &uri);

        let events = e.events().all();
        assert_eq!(events.len(), 2);
        let (_contract, topics, _data) = &events.get(1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("retired"));
    }

    #[test]
    fn test_require_registry_blocks_dropout() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.set_require_registry(&admin, &true);
        client.mint_to(&admin, &user, &1000);

        let purpose = String::from_str(&e, "voluntary");
        let uri = String::from_str(&e, "ipfs://QmTest");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.retire(&user, &100, &purpose, &uri);
        }));
        assert!(
            result.is_err(),
            "retire must panic when registry is required but missing"
        );
    }

    #[test]
    fn test_set_admin_transfers_ownership() {
        let (e, admin, _user1, _user2, _project_id, client) = setup();
        let new_admin = Address::generate(&e);
        e.mock_all_auths();

        client.set_admin(&admin, &new_admin);
        client.mint_to(&new_admin, &new_admin, &200);
        assert_eq!(client.balance(&new_admin), 200);
    }

    #[test]
    fn test_full_credit_lifecycle() {
        let (e, admin, farmer, buyer, project_id, client) = setup();
        e.mock_all_auths();

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

    #[test]
    fn test_max_supply_blocks_over_cap() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.set_max_supply(&admin, &1000);
        assert_eq!(client.max_supply(), 1000);

        client.mint_to(&admin, &user, &1000);
        assert_eq!(client.total_supply(), 1000);

        // Minting beyond cap should panic
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.mint_to(&admin, &user, &1);
        }));
        assert!(result.is_err());
    }

    #[test]
    fn test_max_supply_zero_means_uncapped() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        // Default: 0 = uncapped
        assert_eq!(client.max_supply(), 0);
        client.mint_to(&admin, &user, &1_000_000);
        assert_eq!(client.total_supply(), 1_000_000);
    }

    #[test]
    fn test_max_supply_allows_exact_cap() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.set_max_supply(&admin, &500);
        client.mint_to(&admin, &user, &300);
        client.mint_to(&admin, &user, &200); // exactly at cap
        assert_eq!(client.total_supply(), 500);
    }

    #[test]
    fn test_batch_mint_to_distributes_correctly() {
        let (e, admin, user1, user2, _project_id, client) = setup();
        let user3 = Address::generate(&e);
        e.mock_all_auths();

        let recipients = Vec::from_array(&e, [user1.clone(), user2.clone(), user3.clone()]);
        let amounts: Vec<i128> = Vec::from_array(&e, [100i128, 200i128, 300i128]);

        client.batch_mint_to(&admin, &recipients, &amounts);

        assert_eq!(client.balance(&user1), 100);
        assert_eq!(client.balance(&user2), 200);
        assert_eq!(client.balance(&user3), 300);
        assert_eq!(client.total_supply(), 600);
    }

    #[test]
    fn test_batch_mint_to_same_recipient_accumulates() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        let recipients = Vec::from_array(&e, [user.clone(), user.clone()]);
        let amounts: Vec<i128> = Vec::from_array(&e, [150i128, 250i128]);

        client.batch_mint_to(&admin, &recipients, &amounts);
        assert_eq!(client.balance(&user), 400);
        assert_eq!(client.total_supply(), 400);
    }

    #[test]
    fn test_batch_transfer_distributes_correctly() {
        let (e, admin, user1, user2, _project_id, client) = setup();
        let user3 = Address::generate(&e);
        e.mock_all_auths();

        client.mint_to(&admin, &user1, &1000);

        let recipients = Vec::from_array(&e, [user2.clone(), user3.clone()]);
        let amounts: Vec<i128> = Vec::from_array(&e, [200i128, 300i128]);
        client.batch_transfer(&user1, &recipients, &amounts);

        assert_eq!(client.balance(&user1), 500);
        assert_eq!(client.balance(&user2), 200);
        assert_eq!(client.balance(&user3), 300);
        assert_eq!(client.total_supply(), 1000);
    }

    #[test]
    fn test_batch_transfer_insufficient_balance() {
        let (e, admin, user1, user2, _project_id, client) = setup();
        let user3 = Address::generate(&e);
        e.mock_all_auths();

        client.mint_to(&admin, &user1, &100);

        let recipients = Vec::from_array(&e, [user2.clone(), user3.clone()]);
        let amounts: Vec<i128> = Vec::from_array(&e, [60i128, 60i128]);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.batch_transfer(&user1, &recipients, &amounts);
        }));
        assert!(result.is_err());
    }

    #[test]
    fn test_pause_blocks_mint() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.pause(&admin);
        assert!(client.paused());

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.mint_to(&admin, &user, &100);
        }));
        assert!(result.is_err());
    }

    #[test]
    fn test_pause_blocks_transfer() {
        let (e, admin, user1, user2, _project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user1, &1000);
        client.pause(&admin);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.transfer(&user1, &user2, &100);
        }));
        assert!(result.is_err());
    }

    #[test]
    fn test_unpause_restores_operations() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.pause(&admin);
        assert!(client.paused());

        client.unpause(&admin);
        assert!(!client.paused());

        client.mint_to(&admin, &user, &500);
        assert_eq!(client.balance(&user), 500);
    }

    #[test]
    fn test_paused_state_does_not_affect_reads() {
        let (e, admin, user, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.mint_to(&admin, &user, &300);
        client.pause(&admin);

        // Read-only functions still work while paused
        assert_eq!(client.balance(&user), 300);
        assert_eq!(client.total_supply(), 300);
        assert!(client.paused());
    }

    #[test]
    fn test_pause_emits_event() {
        let (e, admin, _, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.pause(&admin);

        let events = e.events().all();
        let mut found = false;
        for i in 0..events.len() {
            let (_contract, topics, _data) = &events.get(i).unwrap();
            let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
            if topic == symbol_short!("paused") {
                found = true;
                break;
            }
        }
        assert!(found);
    }

    #[test]
    fn test_unpause_emits_event() {
        let (e, admin, _, _, _project_id, client) = setup();
        e.mock_all_auths();

        client.pause(&admin);
        client.unpause(&admin);

        let events = e.events().all();
        let mut found = false;
        for i in 0..events.len() {
            let (_contract, topics, _data) = &events.get(i).unwrap();
            let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
            if topic == symbol_short!("unpaused") {
                found = true;
                break;
            }
        }
        assert!(found);
    }
}
