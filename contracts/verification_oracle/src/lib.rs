#![no_std]
#![allow(clippy::too_many_arguments)]
#![allow(unknown_lints, clippy::manual_is_multiple_of)]
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, vec, Address, Bytes, BytesN, Env, IntoVal,
    Symbol, Val, Vec,
};

#[cfg(test)]
extern crate std;

const EVENT_READING_VERIFIED: Symbol = symbol_short!("rdng_vrfy");
const EVENT_ORACLE_STAKED: Symbol = symbol_short!("orc_stk");
const EVENT_ORACLE_UNSTAKED: Symbol = symbol_short!("orc_unst");
const EVENT_ORACLE_SLASHED: Symbol = symbol_short!("orc_slsh");
const EVENT_ORACLE_COMMITTED: Symbol = symbol_short!("orc_cmt");
const EVENT_ORACLE_REVEALED: Symbol = symbol_short!("orc_rvl");
const EVENT_ORACLE_MISSED_REVEAL: Symbol = symbol_short!("orc_mr");
const EVENT_WINDOW_OPENED: Symbol = symbol_short!("wnd_opn");

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ReadingSubmission {
    pub oracle: Address,
    pub nonce: u64,
    pub timestamp: u64,
    pub ph: i64,
    pub turbidity: i64,
    pub dissolved_oxygen: i64,
    pub flow_rate: i64,
    pub temperature: i64,
    pub total_nitrogen: i64,
    pub total_phosphorus: i64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectConfig {
    pub token_contract: Address,
    pub beneficiary: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VerificationResult {
    pub project_id: BytesN<32>,
    pub n_removal_kg: i128,
    pub p_removal_kg: i128,
    pub quality_penalty: i64,
    pub volumetric_credit: i128,
    pub total_credits: i128,
    /// Amount of `total_credits` that was actually minted to the beneficiary.
    /// May be less than `total_credits` when the token's `max_supply` cap is
    /// reached (partial mint), or `0` when the cap is already exhausted or no
    /// project token is configured. Distinguishes "credits earned" from
    /// "credits actually minted" (Issue #36).
    pub credits_minted: i128,
    pub oracle_count: u32,
    pub finalized_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleConfig {
    pub min_oracles: u32,
    pub max_oracles: u32,
    pub quality_threshold_ph: i64,
    pub quality_threshold_turbidity: i64,
    pub quality_threshold_do: i64,
    pub quality_threshold_temp: i64,
    pub credit_per_kg_n: i128,
    pub credit_per_kg_p: i128,
    pub staking_token: Address,
    pub treasury: Address,
    pub min_stake: i128,
    pub unstake_cooldown_secs: u64,
    pub commit_phase_secs: u64,
    pub reveal_phase_secs: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum WindowPhase {
    Commit,
    Reveal,
    Finalized,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct WindowState {
    pub phase: WindowPhase,
    pub opened_at: u64,
    pub submissions: Vec<ReadingSubmission>,
    pub finalized: bool,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SlashReason {
    pub reason: u32,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct StakeInfo {
    pub amount: i128,
    pub unstake_request: Option<u64>,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CommitInfo {
    pub commitment: BytesN<32>,
    pub nonce: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RevealParams {
    pub nonce: u64,
    pub ph: i64,
    pub turbidity: i64,
    pub dissolved_oxygen: i64,
    pub flow_rate: i64,
    pub temperature: i64,
    pub total_nitrogen: i64,
    pub total_phosphorus: i64,
    pub salt: BytesN<32>,
}

#[contracttype]
pub enum DataKey {
    // ── Instance (loaded on every call) ──
    Admin,
    OracleCount,
    OracleList, // bounded by max_oracles (≤10); safe in instance
    Config,
    TotalSubmissions,
    // ── Persistent (loaded on explicit access, survives with rent) ──
    OracleActive(Address),
    OracleNonce((BytesN<32>, Address)),
    LastResult(BytesN<32>),
    /// Paginated history: ResultAt(project_id, position) → VerificationResult
    ResultAt(BytesN<32>, u64),
    /// Per-project result count, used for paginated history
    ResultCount(BytesN<32>),
    ProjectConfig(BytesN<32>),
    OracleSubmitCount(Address),
    OracleStake(Address),
    OracleSlashed(Address),
    OracleMissedReveals(Address),
    /// Index of project IDs with open (non-finalized) windows
    OpenProjects,
    // ── Temporary (window-scoped, can expire after finalization) ──
    WindowState(BytesN<32>),
    OracleSubmitted(BytesN<32>, Address),
    OracleCommitted((BytesN<32>, Address)),
    OracleRevealed((BytesN<32>, Address)),
}

// ── TTL constants ──
/// Oracle operational data: 1 year.
const ORACLE_TTL_THRESHOLD: u32 = 6_307_200;
const ORACLE_TTL_BUMP: u32 = 6_307_200;
/// Verification results and history: 10 years (audit trail).
const RESULT_TTL_THRESHOLD: u32 = 63_072_000;
const RESULT_TTL_BUMP: u32 = 63_072_000;
/// Project config: 1 year.
const PROJ_CFG_TTL_THRESHOLD: u32 = 6_307_200;
const PROJ_CFG_TTL_BUMP: u32 = 6_307_200;
/// Temporary window entries: 7 days (2 × commit + reveal phases, with buffer).
/// 7 days ≈ 120_960 ledgers at 5 s/ledger.
const WINDOW_TTL_THRESHOLD: u32 = 120_960;
const WINDOW_TTL_BUMP: u32 = 120_960;

fn has_admin(e: &Env) -> bool {
    e.storage().instance().has(&DataKey::Admin)
}

fn read_admin(e: &Env) -> Address {
    e.storage().instance().get(&DataKey::Admin).unwrap()
}

fn read_config(e: &Env) -> OracleConfig {
    e.storage().instance().get(&DataKey::Config).unwrap()
}

/// Compute SHA-256(reading || salt) for commit-reveal scheme.
/// Hashes: nonce(8B) || ph(8B) || turbidity(8B) || dissolved_oxygen(8B) || flow_rate(8B) || temperature(8B) || total_nitrogen(8B) || total_phosphorus(8B) || salt(32B)
fn sha256_commitment(
    e: &Env,
    nonce: u64,
    ph: i64,
    turbidity: i64,
    dissolved_oxygen: i64,
    flow_rate: i64,
    temperature: i64,
    total_nitrogen: i64,
    total_phosphorus: i64,
    salt: &BytesN<32>,
) -> BytesN<32> {
    let mut data: Bytes = Bytes::new(e);
    data.append(&Bytes::from_array(e, &nonce.to_be_bytes()));
    data.append(&Bytes::from_array(e, &ph.to_be_bytes()));
    data.append(&Bytes::from_array(e, &turbidity.to_be_bytes()));
    data.append(&Bytes::from_array(e, &dissolved_oxygen.to_be_bytes()));
    data.append(&Bytes::from_array(e, &flow_rate.to_be_bytes()));
    data.append(&Bytes::from_array(e, &temperature.to_be_bytes()));
    data.append(&Bytes::from_array(e, &total_nitrogen.to_be_bytes()));
    data.append(&Bytes::from_array(e, &total_phosphorus.to_be_bytes()));
    let salt_buf: [u8; 32] = salt.to_array();
    data.append(&Bytes::from_array(e, &salt_buf));
    e.crypto().sha256(&data)
}

fn median_i64(e: &Env, values: &Vec<i64>) -> i64 {
    let mut sorted: Vec<i64> = Vec::new(e);
    for i in 0..values.len() {
        let val = values.get(i).unwrap();
        let mut inserted = false;
        for j in 0..sorted.len() {
            if val < sorted.get(j).unwrap() {
                sorted.insert(j, val);
                inserted = true;
                break;
            }
        }
        if !inserted {
            sorted.push_back(val);
        }
    }
    let len = sorted.len();
    #[allow(unknown_lints, clippy::manual_is_multiple_of)]
    if len % 2 == 0 {
        (sorted.get(len / 2 - 1).unwrap() + sorted.get(len / 2).unwrap()) / 2
    } else {
        sorted.get(len / 2).unwrap()
    }
}

#[allow(unused)]
fn median_i128(e: &Env, values: &Vec<i128>) -> i128 {
    let mut sorted: Vec<i128> = Vec::new(e);
    for i in 0..values.len() {
        let val = values.get(i).unwrap();
        let mut inserted = false;
        for j in 0..sorted.len() {
            if val < sorted.get(j).unwrap() {
                sorted.insert(j, val);
                inserted = true;
                break;
            }
        }
        if !inserted {
            sorted.push_back(val);
        }
    }
    let len = sorted.len();
    #[allow(unknown_lints, clippy::manual_is_multiple_of)]
    if len % 2 == 0 {
        (sorted.get(len / 2 - 1).unwrap() + sorted.get(len / 2).unwrap()) / 2
    } else {
        sorted.get(len / 2).unwrap()
    }
}

fn add_open_project(e: &Env, project_id: &BytesN<32>) {
    let mut open: Vec<BytesN<32>> = e
        .storage()
        .instance()
        .get(&DataKey::OpenProjects)
        .unwrap_or(Vec::new(e));
    for i in 0..open.len() {
        if open.get(i).unwrap() == *project_id {
            return;
        }
    }
    open.push_back(project_id.clone());
    e.storage().instance().set(&DataKey::OpenProjects, &open);
}

fn remove_open_project(e: &Env, project_id: &BytesN<32>) {
    let open: Vec<BytesN<32>> = e
        .storage()
        .instance()
        .get(&DataKey::OpenProjects)
        .unwrap_or(Vec::new(e));
    let mut filtered: Vec<BytesN<32>> = Vec::new(e);
    for i in 0..open.len() {
        let p = open.get(i).unwrap();
        if p != *project_id {
            filtered.push_back(p);
        }
    }
    e.storage()
        .instance()
        .set(&DataKey::OpenProjects, &filtered);
}

fn oracle_has_open_submissions(e: &Env, oracle: &Address) -> bool {
    let open: Vec<BytesN<32>> = e
        .storage()
        .instance()
        .get(&DataKey::OpenProjects)
        .unwrap_or(Vec::new(e));
    for i in 0..open.len() {
        let pid = open.get(i).unwrap();
        if e.storage()
            .temporary()
            .has(&DataKey::OracleSubmitted(pid.clone(), oracle.clone()))
            || e.storage()
                .temporary()
                .has(&DataKey::OracleCommitted((pid.clone(), oracle.clone())))
            || e.storage()
                .temporary()
                .has(&DataKey::OracleRevealed((pid.clone(), oracle.clone())))
        {
            return true;
        }
    }
    false
}

/// Read the token's `total_supply` and `max_supply` and mint at most
/// `total_credits` to `beneficiary`, never exceeding the remaining supply
/// allowance. Returns the amount actually minted.
///
/// This prevents the partial-rollback failure mode described in Issue #36:
/// calling `mint_to` with `total_credits` when
/// `total_supply + total_credits > max_supply` panics inside the token and
/// rolls back the entire oracle finalization (leaving the window in a broken
/// state). By clamping to `max_supply - total_supply` up front, the mint call
/// can never breach the cap.
///
/// Semantics:
/// - `total_credits <= 0` → no mint is attempted, returns `0`.
/// - `max_supply == 0` → token is uncapped; the full `total_credits` is minted.
/// - `total_supply >= max_supply` → nothing remains; returns `0`, no mint.
/// - otherwise → mints `min(total_credits, max_supply - total_supply)`.
fn mint_credits_respecting_cap(
    e: &Env,
    token: &Address,
    beneficiary: &Address,
    total_credits: i128,
) -> i128 {
    if total_credits <= 0 {
        return 0;
    }

    let total_supply: i128 = e.invoke_contract(token, &Symbol::new(e, "total_supply"), vec![e]);
    let max_supply: i128 = e.invoke_contract(token, &Symbol::new(e, "max_supply"), vec![e]);

    let mintable = if max_supply > 0 {
        let remaining = max_supply - total_supply;
        if remaining <= 0 {
            0
        } else {
            remaining.min(total_credits)
        }
    } else {
        total_credits
    };

    if mintable <= 0 {
        return 0;
    }

    let mint_args: Vec<Val> = vec![
        e,
        e.current_contract_address().to_val(),
        beneficiary.to_val(),
        mintable.into_val(e),
    ];
    e.invoke_contract::<()>(token, &Symbol::new(e, "mint_to"), mint_args);
    mintable
}

#[contract]
pub struct VerificationOracle;

#[contractimpl]
#[allow(clippy::too_many_arguments)]
impl VerificationOracle {
    /// Initialize the oracle contract with an admin and default config. Callable once.
    pub fn initialize(e: Env, admin: Address, staking_token: Address, treasury: Address) {
        if has_admin(&e) {
            panic!("already initialized");
        }
        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::OracleCount, &0u32);
        e.storage()
            .instance()
            .set(&DataKey::OracleList, &Vec::<Address>::new(&e));

        let config = OracleConfig {
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
            min_stake: 1000,
            unstake_cooldown_secs: 86400,
            commit_phase_secs: 300,
            reveal_phase_secs: 300,
        };
        e.storage().instance().set(&DataKey::Config, &config);
    }

    /// Transfer admin rights to a new address. Admin only.
    ///
    /// This is the delegation mechanism that lets a `governance` contract take over
    /// admin authority: transfer admin to the governance contract's own address, and
    /// subsequent `execute()` dispatches from governance will auto-authorize the
    /// `admin.require_auth()` check here (a contract always authorizes its own address
    /// for calls it makes), with no separate signature required.
    pub fn transfer_admin(e: Env, admin: Address, new_admin: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Admin, &new_admin);
    }

    /// Add an oracle address to the whitelist. Only admin can call.
    /// If min_stake > 0, the oracle must have at least min_stake tokens staked.
    pub fn add_oracle(e: Env, admin: Address, oracle: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        if e.storage()
            .persistent()
            .has(&DataKey::OracleActive(oracle.clone()))
        {
            panic!("oracle already active");
        }
        let count: u32 = e.storage().instance().get(&DataKey::OracleCount).unwrap();
        let config: OracleConfig = read_config(&e);
        if count >= config.max_oracles {
            panic!("max oracles reached");
        }
        if config.min_stake > 0 {
            let stake_info: StakeInfo = e
                .storage()
                .persistent()
                .get(&DataKey::OracleStake(oracle.clone()))
                .unwrap_or(StakeInfo {
                    amount: 0,
                    unstake_request: None,
                });
            if stake_info.amount < config.min_stake {
                panic!("insufficient stake");
            }
        }
        e.storage()
            .persistent()
            .set(&DataKey::OracleActive(oracle.clone()), &true);
        e.storage().persistent().extend_ttl(
            &DataKey::OracleActive(oracle.clone()),
            ORACLE_TTL_THRESHOLD,
            ORACLE_TTL_BUMP,
        );
        e.storage()
            .instance()
            .set(&DataKey::OracleCount, &(count + 1));

        let mut list: Vec<Address> = e.storage().instance().get(&DataKey::OracleList).unwrap();
        list.push_back(oracle);
        e.storage().instance().set(&DataKey::OracleList, &list);
    }

    /// Remove an oracle from the whitelist. Must maintain at least min_oracles.
    /// The oracle must have zero stake (fully unstaked) before removal.
    pub fn remove_oracle(e: Env, admin: Address, oracle: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        if !e
            .storage()
            .persistent()
            .has(&DataKey::OracleActive(oracle.clone()))
        {
            panic!("oracle not active");
        }
        let stake_info: StakeInfo = e
            .storage()
            .persistent()
            .get(&DataKey::OracleStake(oracle.clone()))
            .unwrap_or(StakeInfo {
                amount: 0,
                unstake_request: None,
            });
        if stake_info.amount > 0 {
            panic!("oracle must unstake before removal");
        }
        let count: u32 = e.storage().instance().get(&DataKey::OracleCount).unwrap();
        let config: OracleConfig = read_config(&e);
        if count <= config.min_oracles {
            panic!("minimum oracles required");
        }
        if oracle_has_open_submissions(&e, &oracle) {
            panic!("oracle has open window submissions");
        }
        e.storage()
            .persistent()
            .remove(&DataKey::OracleActive(oracle.clone()));
        e.storage()
            .instance()
            .set(&DataKey::OracleCount, &(count - 1));

        // Filter the oracle out of the list
        let list: Vec<Address> = e.storage().instance().get(&DataKey::OracleList).unwrap();
        let mut filtered: Vec<Address> = Vec::new(&e);
        for i in 0..list.len() {
            let addr = list.get(i).unwrap();
            if addr != oracle {
                filtered.push_back(addr);
            }
        }
        e.storage().instance().set(&DataKey::OracleList, &filtered);
    }

    /// Check if an oracle address is whitelisted and active.
    pub fn is_oracle_active(e: Env, oracle: Address) -> bool {
        e.storage()
            .persistent()
            .get(&DataKey::OracleActive(oracle))
            .unwrap_or(false)
    }

    /// Get the list of all currently active oracle addresses.
    pub fn get_oracles(e: Env) -> Vec<Address> {
        e.storage()
            .instance()
            .get(&DataKey::OracleList)
            .unwrap_or_else(|| Vec::new(&e))
    }

    /// Submit a sensor reading for a project. Uses nonce-based replay protection.
    /// When min_oracles submissions are collected, computes median values, calculates
    /// nutrient removal, quality penalty, and volumetric credits. If a ProjectConfig
    /// is set, automatically mints credits to the configured beneficiary.
    pub fn submit_reading(
        e: Env,
        oracle: Address,
        project_id: BytesN<32>,
        nonce: u64,
        ph: i64,
        turbidity: i64,
        dissolved_oxygen: i64,
        flow_rate: i64,
        temperature: i64,
        total_nitrogen: i64,
        total_phosphorus: i64,
    ) -> Option<VerificationResult> {
        Self::submit_reading_impl(
            e,
            oracle,
            project_id,
            nonce,
            ph,
            turbidity,
            dissolved_oxygen,
            flow_rate,
            temperature,
            total_nitrogen,
            total_phosphorus,
        )
    }

    fn submit_reading_impl(
        e: Env,
        oracle: Address,
        project_id: BytesN<32>,
        nonce: u64,
        ph: i64,
        turbidity: i64,
        dissolved_oxygen: i64,
        flow_rate: i64,
        temperature: i64,
        total_nitrogen: i64,
        total_phosphorus: i64,
    ) -> Option<VerificationResult> {
        oracle.require_auth();

        if !e
            .storage()
            .persistent()
            .get(&DataKey::OracleActive(oracle.clone()))
            .unwrap_or(false)
        {
            panic!("oracle not active");
        }

        let config: OracleConfig = read_config(&e);
        if config.min_stake > 0 {
            let stake_info: StakeInfo = e
                .storage()
                .persistent()
                .get(&DataKey::OracleStake(oracle.clone()))
                .unwrap_or(StakeInfo {
                    amount: 0,
                    unstake_request: None,
                });
            if stake_info.amount < config.min_stake {
                panic!("insufficient stake");
            }
        }

        let nonce_key = DataKey::OracleNonce((project_id.clone(), oracle.clone()));
        let expected_nonce: u64 = e.storage().persistent().get(&nonce_key).unwrap_or(0) + 1;
        if nonce != expected_nonce {
            panic!("invalid nonce");
        }
        e.storage().persistent().set(&nonce_key, &nonce);
        e.storage()
            .persistent()
            .extend_ttl(&nonce_key, ORACLE_TTL_THRESHOLD, ORACLE_TTL_BUMP);

        // Track per-oracle and global submission counts
        let submit_count_key = DataKey::OracleSubmitCount(oracle.clone());
        let oracle_count: u64 = e.storage().persistent().get(&submit_count_key).unwrap_or(0);
        e.storage()
            .persistent()
            .set(&submit_count_key, &(oracle_count + 1));
        e.storage().persistent().extend_ttl(
            &submit_count_key,
            ORACLE_TTL_THRESHOLD,
            ORACLE_TTL_BUMP,
        );

        let total: u64 = e
            .storage()
            .instance()
            .get(&DataKey::TotalSubmissions)
            .unwrap_or(0);
        e.storage()
            .instance()
            .set(&DataKey::TotalSubmissions, &(total + 1));

        // Prevent duplicate oracle per window (temporary storage)
        let submitted_key = DataKey::OracleSubmitted(project_id.clone(), oracle.clone());
        if e.storage().temporary().has(&submitted_key) {
            panic!("oracle already submitted for this window");
        }

        let window_key = DataKey::WindowState(project_id.clone());
        let mut window: WindowState =
            e.storage()
                .temporary()
                .get(&window_key)
                .unwrap_or(WindowState {
                    phase: WindowPhase::Reveal,
                    opened_at: e.ledger().timestamp(),
                    submissions: Vec::new(&e),
                    finalized: false,
                });

        if window.finalized {
            panic!("window already finalized");
        }

        add_open_project(&e, &project_id);

        let timestamp = e.ledger().timestamp();

        let submission = ReadingSubmission {
            oracle: oracle.clone(),
            nonce,
            timestamp,
            ph,
            turbidity,
            dissolved_oxygen,
            flow_rate,
            temperature,
            total_nitrogen,
            total_phosphorus,
        };

        window.submissions.push_back(submission);
        e.storage().temporary().set(&window_key, &window);
        e.storage()
            .temporary()
            .extend_ttl(&window_key, WINDOW_TTL_THRESHOLD, WINDOW_TTL_BUMP);

        e.storage().temporary().set(&submitted_key, &true);
        e.storage()
            .temporary()
            .extend_ttl(&submitted_key, WINDOW_TTL_THRESHOLD, WINDOW_TTL_BUMP);

        if window.submissions.len() >= config.min_oracles {
            let subs = &window.submissions;
            let n_subs = subs.len();

            let mut ph_vals: Vec<i64> = Vec::new(&e);
            let mut turb_vals: Vec<i64> = Vec::new(&e);
            let mut do_vals: Vec<i64> = Vec::new(&e);
            let mut temp_vals: Vec<i64> = Vec::new(&e);
            let mut flow_vals: Vec<i64> = Vec::new(&e);
            let mut n_vals: Vec<i64> = Vec::new(&e);
            let mut p_vals: Vec<i64> = Vec::new(&e);
            for k in 0..n_subs {
                let s = subs.get(k).unwrap();
                ph_vals.push_back(s.ph);
                turb_vals.push_back(s.turbidity);
                do_vals.push_back(s.dissolved_oxygen);
                temp_vals.push_back(s.temperature);
                flow_vals.push_back(s.flow_rate);
                n_vals.push_back(s.total_nitrogen);
                p_vals.push_back(s.total_phosphorus);
            }

            let med_ph = median_i64(&e, &ph_vals);
            let med_turb = median_i64(&e, &turb_vals);
            let med_do = median_i64(&e, &do_vals);
            let med_temp = median_i64(&e, &temp_vals);
            let med_flow = median_i64(&e, &flow_vals);
            let med_n = median_i64(&e, &n_vals);
            let med_p = median_i64(&e, &p_vals);

            // N removal: baseline 10 mg/L
            let baseline_n: i128 = 10;
            let n_removed: i128 = if (med_n as i128) < baseline_n {
                (baseline_n - med_n as i128) * med_flow as i128 * 3600 / 1000000
            } else {
                0
            };

            // P removal: baseline 2 mg/L
            let baseline_p: i128 = 2;
            let p_removed: i128 = if (med_p as i128) < baseline_p {
                (baseline_p - med_p as i128) * med_flow as i128 * 3600 / 1000000
            } else {
                0
            };

            // Quality penalty (basis points: 0-10000)
            let mut penalty: i64 = 0;
            if med_ph < config.quality_threshold_ph || med_ph > (config.quality_threshold_ph + 100)
            {
                penalty += 2000;
            }
            if med_turb > config.quality_threshold_turbidity {
                penalty += 2000;
            }
            if med_do < config.quality_threshold_do {
                penalty += 2000;
            }
            if med_temp > config.quality_threshold_temp {
                penalty += 1000;
            }
            if penalty > 8000 {
                penalty = 8000;
            }

            // Volumetric credit based on flow
            let volumetric_credit: i128 = if med_flow > 0 {
                med_flow as i128 * 100 / 1000
            } else {
                0
            };

            // Gross credit
            let n_credit: i128 = n_removed * config.credit_per_kg_n;
            let p_credit: i128 = p_removed * config.credit_per_kg_p;
            let gross = n_credit + p_credit + volumetric_credit;

            // Apply quality penalty
            let total: i128 = gross * (10000 - penalty as i128) / 10000;

            let mut result = VerificationResult {
                project_id: project_id.clone(),
                n_removal_kg: n_removed,
                p_removal_kg: p_removed,
                quality_penalty: penalty,
                volumetric_credit,
                total_credits: total,
                credits_minted: 0,
                oracle_count: window.submissions.len(),
                finalized_at: e.ledger().timestamp(),
            };

            // Mint credits to the beneficiary, clamped to the token's
            // max_supply cap. This runs BEFORE the result is persisted so that
            // `credits_minted` is recorded accurately and so a cap breach can
            // never roll back finalization (Issue #36). If no project token is
            // configured, or the cap is already exhausted, no mint occurs and
            // the window still finalizes cleanly.
            let cfg_key = DataKey::ProjectConfig(project_id.clone());
            if let Some(config) = e.storage().persistent().get::<_, ProjectConfig>(&cfg_key) {
                result.credits_minted = mint_credits_respecting_cap(
                    &e,
                    &config.token_contract,
                    &config.beneficiary,
                    result.total_credits,
                );
            }

            // Persist last result
            let last_key = DataKey::LastResult(project_id.clone());
            e.storage().persistent().set(&last_key, &result);
            e.storage()
                .persistent()
                .extend_ttl(&last_key, RESULT_TTL_THRESHOLD, RESULT_TTL_BUMP);

            // Append to paginated history
            let count_key = DataKey::ResultCount(project_id.clone());
            let hist_pos: u64 = e.storage().persistent().get(&count_key).unwrap_or(0);
            let hist_key = DataKey::ResultAt(project_id.clone(), hist_pos);
            e.storage().persistent().set(&hist_key, &result);
            e.storage()
                .persistent()
                .extend_ttl(&hist_key, RESULT_TTL_THRESHOLD, RESULT_TTL_BUMP);
            e.storage().persistent().set(&count_key, &(hist_pos + 1));
            e.storage()
                .persistent()
                .extend_ttl(&count_key, RESULT_TTL_THRESHOLD, RESULT_TTL_BUMP);

            window.finalized = true;
            e.storage().temporary().set(&window_key, &window);
            // no extend needed — finalized windows can expire

            remove_open_project(&e, &project_id);

            e.events()
                .publish((EVENT_READING_VERIFIED,), (project_id, result.clone()));

            Some(result)
        } else {
            None
        }
    }

    /// Configure the credit token contract and beneficiary for a project.
    /// When enabled, the oracle will auto-mint credits to the beneficiary upon verification finalization.
    pub fn set_project_config(
        e: Env,
        admin: Address,
        project_id: BytesN<32>,
        token_contract: Address,
        beneficiary: Address,
    ) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        let config = ProjectConfig {
            token_contract,
            beneficiary,
        };
        let key = DataKey::ProjectConfig(project_id);
        e.storage().persistent().set(&key, &config);
        e.storage()
            .persistent()
            .extend_ttl(&key, PROJ_CFG_TTL_THRESHOLD, PROJ_CFG_TTL_BUMP);
    }

    /// Get the project config (token contract and beneficiary) for a project.
    pub fn get_project_config(e: Env, project_id: BytesN<32>) -> Option<ProjectConfig> {
        let key = DataKey::ProjectConfig(project_id);
        let result: Option<ProjectConfig> = e.storage().persistent().get(&key);
        if result.is_some() {
            e.storage()
                .persistent()
                .extend_ttl(&key, PROJ_CFG_TTL_THRESHOLD, PROJ_CFG_TTL_BUMP);
        }
        result
    }

    /// Get the last verification result for a project. Returns None if no window has been finalized.
    pub fn get_last_result(e: Env, project_id: BytesN<32>) -> Option<VerificationResult> {
        let key = DataKey::LastResult(project_id);
        let result: Option<VerificationResult> = e.storage().persistent().get(&key);
        if result.is_some() {
            e.storage()
                .persistent()
                .extend_ttl(&key, RESULT_TTL_THRESHOLD, RESULT_TTL_BUMP);
        }
        result
    }

    /// Get paginated history of verification results for a project.
    /// `offset` is the zero-based start position; `limit` is the max entries to return.
    pub fn get_result_history(
        e: Env,
        project_id: BytesN<32>,
        offset: u64,
        limit: u32,
    ) -> Vec<VerificationResult> {
        let count_key = DataKey::ResultCount(project_id.clone());
        let total: u64 = e.storage().persistent().get(&count_key).unwrap_or(0);
        let end = (offset + limit as u64).min(total);
        let mut results: Vec<VerificationResult> = Vec::new(&e);
        for pos in offset..end {
            let key = DataKey::ResultAt(project_id.clone(), pos);
            if let Some(r) = e.storage().persistent().get::<_, VerificationResult>(&key) {
                e.storage()
                    .persistent()
                    .extend_ttl(&key, RESULT_TTL_THRESHOLD, RESULT_TTL_BUMP);
                results.push_back(r);
            }
        }
        results
    }

    /// Get the total number of stored results for a project.
    pub fn result_count(e: Env, project_id: BytesN<32>) -> u64 {
        e.storage()
            .persistent()
            .get(&DataKey::ResultCount(project_id))
            .unwrap_or(0)
    }

    /// Get the current oracle configuration parameters.
    pub fn get_config(e: Env) -> OracleConfig {
        read_config(&e)
    }

    /// Get the total number of readings an oracle has submitted across all projects and windows.
    pub fn oracle_submit_count(e: Env, oracle: Address) -> u64 {
        e.storage()
            .persistent()
            .get(&DataKey::OracleSubmitCount(oracle))
            .unwrap_or(0)
    }

    /// Get the total number of readings submitted by all oracles across all time.
    pub fn total_submissions(e: Env) -> u64 {
        e.storage()
            .instance()
            .get(&DataKey::TotalSubmissions)
            .unwrap_or(0)
    }

    /// Get the current number of active whitelisted oracles.
    pub fn oracle_count(e: Env) -> u32 {
        e.storage()
            .instance()
            .get(&DataKey::OracleCount)
            .unwrap_or(0)
    }

    /// Update the oracle configuration (min/max oracles, quality thresholds, credit rates). Admin only.
    pub fn update_config(e: Env, admin: Address, config: OracleConfig) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Config, &config);
    }

    /// Reset the open submission window for a project, clearing all pending oracle submissions.
    /// This allows oracles to resubmit for the same project in a new window, e.g. after a
    /// sensor error or stale data invalidation. Only callable by admin.
    /// Does not affect already-finalized results or oracle nonces.
    pub fn reset_window(e: Env, admin: Address, project_id: BytesN<32>) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }

        let window_key = DataKey::WindowState(project_id.clone());
        let window: Option<WindowState> = e.storage().temporary().get(&window_key);

        match window {
            None => panic!("no window found for project"),
            Some(ref w) if w.finalized => panic!("window already finalized"),
            _ => {}
        }

        let window = window.unwrap();

        // Remove OracleSubmitted markers for all submissions in this window
        for i in 0..window.submissions.len() {
            let sub = window.submissions.get(i).unwrap();
            e.storage()
                .temporary()
                .remove(&DataKey::OracleSubmitted(project_id.clone(), sub.oracle));
        }

        // Remove OracleCommitted and OracleRevealed markers for all active oracles.
        // Oracles that committed but haven't revealed yet have no entry in
        // window.submissions, so iterating only over submissions would leave
        // stale committed markers that block oracle removal (issue #38).
        let oracles: Vec<Address> = e
            .storage()
            .instance()
            .get(&DataKey::OracleList)
            .unwrap_or_else(|| Vec::new(&e));
        for i in 0..oracles.len() {
            let oracle = oracles.get(i).unwrap();
            e.storage().temporary().remove(&DataKey::OracleCommitted((
                project_id.clone(),
                oracle.clone(),
            )));
            e.storage().temporary().remove(&DataKey::OracleRevealed((
                project_id.clone(),
                oracle.clone(),
            )));
        }

        // Replace with a fresh empty window in Reveal phase (for direct submissions)
        let fresh = WindowState {
            phase: WindowPhase::Reveal,
            opened_at: e.ledger().timestamp(),
            submissions: Vec::new(&e),
            finalized: false,
        };
        e.storage().temporary().set(&window_key, &fresh);
        e.storage()
            .temporary()
            .extend_ttl(&window_key, WINDOW_TTL_THRESHOLD, WINDOW_TTL_BUMP);
    }

    /// Get the number of submissions in the current open window for a project.
    /// Returns 0 if no window exists or the window was already finalized.
    pub fn window_submission_count(e: Env, project_id: BytesN<32>) -> u32 {
        let window: Option<WindowState> = e
            .storage()
            .temporary()
            .get(&DataKey::WindowState(project_id));
        match window {
            None => 0,
            Some(w) if w.finalized => 0,
            Some(w) => w.submissions.len(),
        }
    }

    /// Stake tokens as collateral. The oracle must first approve this contract
    /// to spend `amount` of the configured staking token. Staked tokens are
    /// locked and can be slashed by admin or governance.
    pub fn stake(e: Env, oracle: Address, amount: i128) {
        oracle.require_auth();
        if amount <= 0 {
            soroban_sdk::panic_with_error!(&e, soroban_sdk::Error::from_contract_error(1));
        }
        let config: OracleConfig = read_config(&e);

        let transfer_args: Vec<Val> = vec![
            &e,
            oracle.to_val(),
            e.current_contract_address().to_val(),
            amount.into_val(&e),
        ];
        e.invoke_contract::<()>(
            &config.staking_token,
            &Symbol::new(&e, "transfer_from"),
            transfer_args,
        );

        let stake_key = DataKey::OracleStake(oracle.clone());
        let mut stake_info: StakeInfo =
            e.storage()
                .persistent()
                .get(&stake_key)
                .unwrap_or(StakeInfo {
                    amount: 0,
                    unstake_request: None,
                });
        stake_info.amount += amount;
        stake_info.unstake_request = None;
        e.storage().persistent().set(&stake_key, &stake_info);
        e.storage()
            .persistent()
            .extend_ttl(&stake_key, ORACLE_TTL_THRESHOLD, ORACLE_TTL_BUMP);

        e.events().publish((EVENT_ORACLE_STAKED,), (oracle, amount));
    }

    /// Request to unstake tokens. The unstaked tokens become available after
    /// `unstake_cooldown_secs` have elapsed. Only callable when the oracle
    /// is not active or has no pending unstake request.
    pub fn unstake(e: Env, oracle: Address, amount: i128) {
        oracle.require_auth();
        if amount <= 0 {
            panic!("unstake amount must be positive");
        }
        let config: OracleConfig = read_config(&e);
        let stake_key = DataKey::OracleStake(oracle.clone());
        let mut stake_info: StakeInfo =
            e.storage()
                .persistent()
                .get(&stake_key)
                .unwrap_or(StakeInfo {
                    amount: 0,
                    unstake_request: None,
                });
        if stake_info.amount < amount {
            panic!("insufficient staked balance");
        }
        if e.storage()
            .persistent()
            .get(&DataKey::OracleActive(oracle.clone()))
            .unwrap_or(false)
        {
            let remaining = stake_info.amount - amount;
            if remaining < config.min_stake {
                panic!("would fall below minimum stake");
            }
        }
        let now = e.ledger().timestamp();
        stake_info.amount -= amount;
        stake_info.unstake_request = Some(now + config.unstake_cooldown_secs);
        e.storage().persistent().set(&stake_key, &stake_info);
        e.storage()
            .persistent()
            .extend_ttl(&stake_key, ORACLE_TTL_THRESHOLD, ORACLE_TTL_BUMP);

        e.events()
            .publish((EVENT_ORACLE_UNSTAKED,), (oracle, amount));
    }

    /// Claim unstaked tokens after the cooldown period has elapsed.
    pub fn claim_unstake(e: Env, oracle: Address) {
        oracle.require_auth();
        let stake_key = DataKey::OracleStake(oracle.clone());
        let stake_info: StakeInfo = e
            .storage()
            .persistent()
            .get(&stake_key)
            .unwrap_or(StakeInfo {
                amount: 0,
                unstake_request: None,
            });
        let cooldown_end = stake_info.unstake_request.unwrap_or(0);
        let now = e.ledger().timestamp();
        if cooldown_end == 0 || now < cooldown_end {
            panic!("cooldown not elapsed");
        }
        let config: OracleConfig = read_config(&e);
        let unstaked_amount = stake_info.amount;

        let transfer_args: Vec<Val> = vec![
            &e,
            e.current_contract_address().to_val(),
            oracle.to_val(),
            unstaked_amount.into_val(&e),
        ];
        e.invoke_contract::<()>(
            &config.staking_token,
            &Symbol::new(&e, "transfer"),
            transfer_args,
        );

        e.storage().persistent().set(
            &stake_key,
            &StakeInfo {
                amount: 0,
                unstake_request: None,
            },
        );
        e.storage()
            .persistent()
            .extend_ttl(&stake_key, ORACLE_TTL_THRESHOLD, ORACLE_TTL_BUMP);
    }

    /// Slash an oracle's stake. Callable by admin or governance.
    /// Reason codes: 1 = admin_flag, 2 = fraud_proof.
    /// Slashed funds go to the treasury address.
    pub fn slash(e: Env, caller: Address, oracle: Address, amount: i128, reason: u32) {
        caller.require_auth();
        let stored: Address = read_admin(&e);
        if caller != stored {
            panic!("unauthorized");
        }
        if amount <= 0 {
            panic!("slash amount must be positive");
        }
        let stake_key = DataKey::OracleStake(oracle.clone());
        let mut stake_info: StakeInfo =
            e.storage()
                .persistent()
                .get(&stake_key)
                .unwrap_or(StakeInfo {
                    amount: 0,
                    unstake_request: None,
                });
        if stake_info.amount < amount {
            panic!("slash exceeds staked balance");
        }
        stake_info.amount -= amount;
        e.storage().persistent().set(&stake_key, &stake_info);
        e.storage()
            .persistent()
            .extend_ttl(&stake_key, ORACLE_TTL_THRESHOLD, ORACLE_TTL_BUMP);

        let config: OracleConfig = read_config(&e);
        let transfer_args: Vec<Val> = vec![
            &e,
            e.current_contract_address().to_val(),
            config.treasury.to_val(),
            amount.into_val(&e),
        ];
        e.invoke_contract::<()>(
            &config.staking_token,
            &Symbol::new(&e, "transfer"),
            transfer_args,
        );

        let slash_record = SlashReason {
            reason,
            timestamp: e.ledger().timestamp(),
        };
        let slash_key = DataKey::OracleSlashed(oracle.clone());
        e.storage().persistent().set(&slash_key, &slash_record);
        e.storage()
            .persistent()
            .extend_ttl(&slash_key, ORACLE_TTL_THRESHOLD, ORACLE_TTL_BUMP);

        e.events()
            .publish((EVENT_ORACLE_SLASHED,), (oracle, amount, reason));
    }

    /// Get the current staked balance and unstake request for an oracle.
    pub fn get_stake(e: Env, oracle: Address) -> StakeInfo {
        e.storage()
            .persistent()
            .get(&DataKey::OracleStake(oracle))
            .unwrap_or(StakeInfo {
                amount: 0,
                unstake_request: None,
            })
    }

    /// Get the slash record for an oracle (most recent slash).
    pub fn get_slash_record(e: Env, oracle: Address) -> Option<SlashReason> {
        e.storage()
            .persistent()
            .get(&DataKey::OracleSlashed(oracle))
    }

    /// Get the unstake cooldown period in seconds.
    pub fn get_unstake_cooldown(e: Env) -> u64 {
        let config: OracleConfig = read_config(&e);
        config.unstake_cooldown_secs
    }

    /// Get the treasury address where slashed funds are sent.
    pub fn get_treasury(e: Env) -> Address {
        let config: OracleConfig = read_config(&e);
        config.treasury
    }

    /// Get the staking token contract address.
    pub fn get_staking_token(e: Env) -> Address {
        let config: OracleConfig = read_config(&e);
        config.staking_token
    }

    // ── Commit-Reveal Scheme ──

    /// Open a new commit-reveal window for a project. Starts the commit phase.
    /// Only callable by admin. Cannot open a new window if one is already active.
    pub fn open_window(e: Env, admin: Address, project_id: BytesN<32>) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }

        let window_key = DataKey::WindowState(project_id.clone());
        let existing: Option<WindowState> = e.storage().temporary().get(&window_key);
        match existing {
            Some(ref w) if !w.finalized => panic!("window already active"),
            _ => {}
        }

        let window = WindowState {
            phase: WindowPhase::Commit,
            opened_at: e.ledger().timestamp(),
            submissions: Vec::new(&e),
            finalized: false,
        };
        e.storage().temporary().set(&window_key, &window);
        e.storage()
            .temporary()
            .extend_ttl(&window_key, WINDOW_TTL_THRESHOLD, WINDOW_TTL_BUMP);

        add_open_project(&e, &project_id);

        e.events().publish((EVENT_WINDOW_OPENED,), (project_id,));
    }

    /// Get the current phase of a project's window.
    pub fn get_window_phase(e: Env, project_id: BytesN<32>) -> Option<WindowPhase> {
        let window: Option<WindowState> = e
            .storage()
            .temporary()
            .get(&DataKey::WindowState(project_id));
        window.map(|w| w.phase)
    }

    /// Commit a SHA-256 hash of (reading + salt) during the commit phase.
    /// The oracle computes the hash off-chain and submits only the commitment.
    pub fn commit_reading(
        e: Env,
        oracle: Address,
        project_id: BytesN<32>,
        nonce: u64,
        commitment: BytesN<32>,
    ) {
        oracle.require_auth();

        if !e
            .storage()
            .persistent()
            .get(&DataKey::OracleActive(oracle.clone()))
            .unwrap_or(false)
        {
            panic!("oracle not active");
        }

        let config: OracleConfig = read_config(&e);
        if config.min_stake > 0 {
            let stake_info: StakeInfo = e
                .storage()
                .persistent()
                .get(&DataKey::OracleStake(oracle.clone()))
                .unwrap_or(StakeInfo {
                    amount: 0,
                    unstake_request: None,
                });
            if stake_info.amount < config.min_stake {
                panic!("insufficient stake");
            }
        }

        let nonce_key = DataKey::OracleNonce((project_id.clone(), oracle.clone()));
        let expected_nonce: u64 = e.storage().persistent().get(&nonce_key).unwrap_or(0) + 1;
        if nonce != expected_nonce {
            panic!("invalid nonce");
        }

        let window_key = DataKey::WindowState(project_id.clone());
        let window: WindowState = e
            .storage()
            .temporary()
            .get(&window_key)
            .expect("no window open");

        if window.finalized {
            panic!("window already finalized");
        }
        if window.phase != WindowPhase::Commit {
            panic!("not in commit phase");
        }

        let commit_key = DataKey::OracleCommitted((project_id.clone(), oracle.clone()));
        if e.storage().temporary().has(&commit_key) {
            panic!("oracle already committed");
        }

        e.storage().persistent().set(&nonce_key, &nonce);
        e.storage()
            .persistent()
            .extend_ttl(&nonce_key, ORACLE_TTL_THRESHOLD, ORACLE_TTL_BUMP);

        e.storage().temporary().set(
            &commit_key,
            &CommitInfo {
                commitment: commitment.clone(),
                nonce,
            },
        );
        e.storage()
            .temporary()
            .extend_ttl(&commit_key, WINDOW_TTL_THRESHOLD, WINDOW_TTL_BUMP);

        e.events()
            .publish((EVENT_ORACLE_COMMITTED,), (oracle, project_id, commitment));
    }

    /// Transition a window from commit phase to reveal phase.
    /// Callable by anyone after the commit phase duration has elapsed.
    pub fn begin_reveal_phase(e: Env, project_id: BytesN<32>) {
        let window_key = DataKey::WindowState(project_id.clone());
        let window: WindowState = e
            .storage()
            .temporary()
            .get(&window_key)
            .expect("no window open");

        if window.finalized {
            panic!("window already finalized");
        }
        if window.phase != WindowPhase::Commit {
            panic!("not in commit phase");
        }

        let config: OracleConfig = read_config(&e);
        let now = e.ledger().timestamp();
        if now < window.opened_at + config.commit_phase_secs {
            panic!("commit phase not ended");
        }

        let mut window = window;
        window.phase = WindowPhase::Reveal;
        e.storage().temporary().set(&window_key, &window);
        e.storage()
            .temporary()
            .extend_ttl(&window_key, WINDOW_TTL_THRESHOLD, WINDOW_TTL_BUMP);
    }

    /// Reveal the actual reading values + salt during the reveal phase.
    /// The contract recomputes the hash and verifies it matches the stored commitment.
    pub fn reveal_reading(
        e: Env,
        oracle: Address,
        project_id: BytesN<32>,
        params: RevealParams,
    ) -> Option<VerificationResult> {
        oracle.require_auth();

        if !e
            .storage()
            .persistent()
            .get(&DataKey::OracleActive(oracle.clone()))
            .unwrap_or(false)
        {
            panic!("oracle not active");
        }

        let config: OracleConfig = read_config(&e);
        if config.min_stake > 0 {
            let stake_info: StakeInfo = e
                .storage()
                .persistent()
                .get(&DataKey::OracleStake(oracle.clone()))
                .unwrap_or(StakeInfo {
                    amount: 0,
                    unstake_request: None,
                });
            if stake_info.amount < config.min_stake {
                panic!("insufficient stake");
            }
        }

        let window_key = DataKey::WindowState(project_id.clone());
        let mut window: WindowState = e
            .storage()
            .temporary()
            .get(&window_key)
            .expect("no window open");

        if window.finalized {
            panic!("window already finalized");
        }
        if window.phase != WindowPhase::Reveal {
            panic!("not in reveal phase");
        }

        let commit_key = DataKey::OracleCommitted((project_id.clone(), oracle.clone()));
        let commit_info: CommitInfo = e
            .storage()
            .temporary()
            .get(&commit_key)
            .expect("oracle did not commit");

        if commit_info.nonce != params.nonce {
            panic!("nonce mismatch with commitment");
        }

        let reveal_key = DataKey::OracleRevealed((project_id.clone(), oracle.clone()));
        if e.storage().temporary().has(&reveal_key) {
            panic!("oracle already revealed");
        }

        // Verify the hash matches the commitment
        let computed = sha256_commitment(
            &e,
            params.nonce,
            params.ph,
            params.turbidity,
            params.dissolved_oxygen,
            params.flow_rate,
            params.temperature,
            params.total_nitrogen,
            params.total_phosphorus,
            &params.salt,
        );
        if computed != commit_info.commitment {
            panic!("hash mismatch: revealed values do not match commitment");
        }

        // Track per-oracle and global submission counts
        let submit_key = DataKey::OracleSubmitCount(oracle.clone());
        let oracle_submit_count: u64 = e.storage().persistent().get(&submit_key).unwrap_or(0);
        e.storage()
            .persistent()
            .set(&submit_key, &(oracle_submit_count + 1));
        e.storage()
            .persistent()
            .extend_ttl(&submit_key, ORACLE_TTL_THRESHOLD, ORACLE_TTL_BUMP);

        let total: u64 = e
            .storage()
            .instance()
            .get(&DataKey::TotalSubmissions)
            .unwrap_or(0);
        e.storage()
            .instance()
            .set(&DataKey::TotalSubmissions, &(total + 1));

        let timestamp = e.ledger().timestamp();

        let submission = ReadingSubmission {
            oracle: oracle.clone(),
            nonce: params.nonce,
            timestamp,
            ph: params.ph,
            turbidity: params.turbidity,
            dissolved_oxygen: params.dissolved_oxygen,
            flow_rate: params.flow_rate,
            temperature: params.temperature,
            total_nitrogen: params.total_nitrogen,
            total_phosphorus: params.total_phosphorus,
        };

        window.submissions.push_back(submission);
        e.storage().temporary().set(&window_key, &window);
        e.storage()
            .temporary()
            .extend_ttl(&window_key, WINDOW_TTL_THRESHOLD, WINDOW_TTL_BUMP);

        e.storage().temporary().set(&reveal_key, &true);
        e.storage()
            .temporary()
            .extend_ttl(&reveal_key, WINDOW_TTL_THRESHOLD, WINDOW_TTL_BUMP);

        e.events()
            .publish((EVENT_ORACLE_REVEALED,), (oracle, project_id.clone()));

        if window.submissions.len() >= config.min_oracles {
            Self::finalize_reveals(e, project_id)
        } else {
            None
        }
    }

    /// Finalize a commit-reveal window after the reveal phase ends.
    /// Penalizes oracles that committed but did not reveal.
    /// Can be called by anyone once the reveal phase duration has elapsed.
    pub fn finalize_window(e: Env, project_id: BytesN<32>) -> Option<VerificationResult> {
        let window_key = DataKey::WindowState(project_id.clone());
        let window: WindowState = e
            .storage()
            .temporary()
            .get(&window_key)
            .expect("no window open");

        if window.finalized {
            panic!("window already finalized");
        }
        if window.phase != WindowPhase::Reveal {
            panic!("not in reveal phase");
        }

        let config: OracleConfig = read_config(&e);
        let now = e.ledger().timestamp();
        let reveal_end = window.opened_at + config.commit_phase_secs + config.reveal_phase_secs;
        if now < reveal_end {
            panic!("reveal phase not ended");
        }

        Self::penalize_non_revealers(&e, &project_id);
        Self::finalize_reveals(e, project_id)
    }

    /// Internal: penalize oracles that committed but did not reveal.
    fn penalize_non_revealers(e: &Env, project_id: &BytesN<32>) {
        let oracles: Vec<Address> = e
            .storage()
            .instance()
            .get(&DataKey::OracleList)
            .unwrap_or_else(|| Vec::new(e));

        let config: OracleConfig = read_config(e);

        for i in 0..oracles.len() {
            let oracle = oracles.get(i).unwrap();
            let commit_key = DataKey::OracleCommitted((project_id.clone(), oracle.clone()));
            let reveal_key = DataKey::OracleRevealed((project_id.clone(), oracle.clone()));

            let committed = e.storage().temporary().has(&commit_key);
            let revealed = e.storage().temporary().has(&reveal_key);

            if committed && !revealed {
                // Increment missed reveals counter
                let missed_key = DataKey::OracleMissedReveals(oracle.clone());
                let missed: u64 = e.storage().persistent().get(&missed_key).unwrap_or(0);
                e.storage().persistent().set(&missed_key, &(missed + 1));
                e.storage().persistent().extend_ttl(
                    &missed_key,
                    ORACLE_TTL_THRESHOLD,
                    ORACLE_TTL_BUMP,
                );

                // Slash the oracle's stake
                let stake_key = DataKey::OracleStake(oracle.clone());
                let mut stake_info: StakeInfo =
                    e.storage()
                        .persistent()
                        .get(&stake_key)
                        .unwrap_or(StakeInfo {
                            amount: 0,
                            unstake_request: None,
                        });

                if stake_info.amount > 0 {
                    let slash_amount = stake_info.amount.min(config.min_stake);
                    if slash_amount > 0 {
                        stake_info.amount -= slash_amount;
                        e.storage().persistent().set(&stake_key, &stake_info);
                        e.storage().persistent().extend_ttl(
                            &stake_key,
                            ORACLE_TTL_THRESHOLD,
                            ORACLE_TTL_BUMP,
                        );

                        let transfer_args: Vec<Val> = vec![
                            e,
                            e.current_contract_address().to_val(),
                            config.treasury.to_val(),
                            slash_amount.into_val(e),
                        ];
                        e.invoke_contract::<()>(
                            &config.staking_token,
                            &Symbol::new(e, "transfer"),
                            transfer_args,
                        );

                        let slash_record = SlashReason {
                            reason: 3, // missed_reveal
                            timestamp: e.ledger().timestamp(),
                        };
                        let slash_key = DataKey::OracleSlashed(oracle.clone());
                        e.storage().persistent().set(&slash_key, &slash_record);
                        e.storage().persistent().extend_ttl(
                            &slash_key,
                            ORACLE_TTL_THRESHOLD,
                            ORACLE_TTL_BUMP,
                        );

                        e.events().publish(
                            (EVENT_ORACLE_MISSED_REVEAL,),
                            (oracle.clone(), slash_amount),
                        );
                    }
                }

                // Clean up commitment from temporary storage
                e.storage().temporary().remove(&commit_key);
            }
        }
    }

    /// Internal: finalize a window with current submissions (used by both
    /// auto-finalization in reveal_reading and explicit finalize_window).
    fn finalize_reveals(e: Env, project_id: BytesN<32>) -> Option<VerificationResult> {
        let window_key = DataKey::WindowState(project_id.clone());
        let mut window: WindowState = e
            .storage()
            .temporary()
            .get(&window_key)
            .expect("no window open");

        if window.finalized {
            return None;
        }

        let config: OracleConfig = read_config(&e);
        let subs = &window.submissions;
        let n_subs = subs.len();

        if n_subs < config.min_oracles {
            return None;
        }

        let mut ph_vals: Vec<i64> = Vec::new(&e);
        let mut turb_vals: Vec<i64> = Vec::new(&e);
        let mut do_vals: Vec<i64> = Vec::new(&e);
        let mut temp_vals: Vec<i64> = Vec::new(&e);
        let mut flow_vals: Vec<i64> = Vec::new(&e);
        let mut n_vals: Vec<i64> = Vec::new(&e);
        let mut p_vals: Vec<i64> = Vec::new(&e);
        for k in 0..n_subs {
            let s = subs.get(k).unwrap();
            ph_vals.push_back(s.ph);
            turb_vals.push_back(s.turbidity);
            do_vals.push_back(s.dissolved_oxygen);
            temp_vals.push_back(s.temperature);
            flow_vals.push_back(s.flow_rate);
            n_vals.push_back(s.total_nitrogen);
            p_vals.push_back(s.total_phosphorus);
        }

        let med_ph = median_i64(&e, &ph_vals);
        let med_turb = median_i64(&e, &turb_vals);
        let med_do = median_i64(&e, &do_vals);
        let med_temp = median_i64(&e, &temp_vals);
        let med_flow = median_i64(&e, &flow_vals);
        let med_n = median_i64(&e, &n_vals);
        let med_p = median_i64(&e, &p_vals);

        let baseline_n: i128 = 10;
        let n_removed: i128 = if (med_n as i128) < baseline_n {
            (baseline_n - med_n as i128) * med_flow as i128 * 3600 / 1000000
        } else {
            0
        };

        let baseline_p: i128 = 2;
        let p_removed: i128 = if (med_p as i128) < baseline_p {
            (baseline_p - med_p as i128) * med_flow as i128 * 3600 / 1000000
        } else {
            0
        };

        let mut penalty: i64 = 0;
        if med_ph < config.quality_threshold_ph || med_ph > (config.quality_threshold_ph + 100) {
            penalty += 2000;
        }
        if med_turb > config.quality_threshold_turbidity {
            penalty += 2000;
        }
        if med_do < config.quality_threshold_do {
            penalty += 2000;
        }
        if med_temp > config.quality_threshold_temp {
            penalty += 1000;
        }
        if penalty > 8000 {
            penalty = 8000;
        }

        let volumetric_credit: i128 = if med_flow > 0 {
            med_flow as i128 * 100 / 1000
        } else {
            0
        };

        let n_credit: i128 = n_removed * config.credit_per_kg_n;
        let p_credit: i128 = p_removed * config.credit_per_kg_p;
        let gross = n_credit + p_credit + volumetric_credit;
        let total: i128 = gross * (10000 - penalty as i128) / 10000;

        let mut result = VerificationResult {
            project_id: project_id.clone(),
            n_removal_kg: n_removed,
            p_removal_kg: p_removed,
            quality_penalty: penalty,
            volumetric_credit,
            total_credits: total,
            credits_minted: 0,
            oracle_count: window.submissions.len(),
            finalized_at: e.ledger().timestamp(),
        };

        // Mint credits to the beneficiary, clamped to the token's max_supply cap
        // (same handling as submit_reading_impl; see Issue #36). Runs before the
        // result is persisted so `credits_minted` is recorded accurately.
        let cfg_key = DataKey::ProjectConfig(project_id.clone());
        if let Some(config) = e.storage().persistent().get::<_, ProjectConfig>(&cfg_key) {
            result.credits_minted = mint_credits_respecting_cap(
                &e,
                &config.token_contract,
                &config.beneficiary,
                result.total_credits,
            );
        }

        // Persist last result
        let last_key = DataKey::LastResult(project_id.clone());
        e.storage().persistent().set(&last_key, &result);
        e.storage()
            .persistent()
            .extend_ttl(&last_key, RESULT_TTL_THRESHOLD, RESULT_TTL_BUMP);

        // Append to paginated history
        let count_key = DataKey::ResultCount(project_id.clone());
        let hist_pos: u64 = e.storage().persistent().get(&count_key).unwrap_or(0);
        let hist_key = DataKey::ResultAt(project_id.clone(), hist_pos);
        e.storage().persistent().set(&hist_key, &result);
        e.storage()
            .persistent()
            .extend_ttl(&hist_key, RESULT_TTL_THRESHOLD, RESULT_TTL_BUMP);
        e.storage().persistent().set(&count_key, &(hist_pos + 1));
        e.storage()
            .persistent()
            .extend_ttl(&count_key, RESULT_TTL_THRESHOLD, RESULT_TTL_BUMP);

        window.finalized = true;
        window.phase = WindowPhase::Finalized;
        // Write finalized state back; window will naturally expire via TTL
        e.storage().temporary().set(&window_key, &window);

        remove_open_project(&e, &project_id);

        // Clean up commit/reveal markers for all oracles in this window
        let oracles: Vec<Address> = e
            .storage()
            .instance()
            .get(&DataKey::OracleList)
            .unwrap_or_else(|| Vec::new(&e));
        for i in 0..oracles.len() {
            let oracle = oracles.get(i).unwrap();
            e.storage().temporary().remove(&DataKey::OracleCommitted((
                project_id.clone(),
                oracle.clone(),
            )));
            e.storage().temporary().remove(&DataKey::OracleRevealed((
                project_id.clone(),
                oracle.clone(),
            )));
        }

        e.events()
            .publish((EVENT_READING_VERIFIED,), (project_id, result.clone()));

        Some(result)
    }

    /// Get the number of missed reveals for an oracle across all windows.
    pub fn oracle_missed_reveals(e: Env, oracle: Address) -> u64 {
        e.storage()
            .persistent()
            .get(&DataKey::OracleMissedReveals(oracle))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::Ledger as _;

    fn set_ledger_timestamp(e: &Env, timestamp: u64) {
        let mut info = e.ledger().get();
        info.timestamp = timestamp;
        e.ledger().set(info);
    }

    // Minimal mock token that implements transfer_from and transfer.
    // In tests with mock_all_auths, auth checks are bypassed.
    #[contract]
    pub struct MockToken;

    #[contractimpl]
    impl MockToken {
        pub fn initialize(_e: Env, _admin: Address) {}

        pub fn transfer(_e: Env, _from: Address, _to: Address, _amount: i128) {}

        pub fn transfer_from(
            _e: Env,
            _spender: Address,
            _from: Address,
            _to: Address,
            _amount: i128,
        ) {
        }

        pub fn balance(_e: Env, _addr: Address) -> i128 {
            1_000_000
        }
    }

    fn setup_with_client() -> (Env, Address, VerificationOracleClient<'static>) {
        let e = Env::default();
        let admin = Address::generate(&e);
        let staking_token = Address::generate(&e);
        let treasury = Address::generate(&e);
        let contract_id = e.register_contract(None, VerificationOracle);
        let client = VerificationOracleClient::new(&e, &contract_id);
        client.initialize(&admin, &staking_token, &treasury);
        (e, admin, client)
    }

    #[test]
    fn test_initialize_sets_default_config() {
        let (_e, _admin, client) = setup_with_client();
        let config = client.get_config();
        assert_eq!(config.min_oracles, 3);
        assert_eq!(config.max_oracles, 10);
        assert_eq!(config.credit_per_kg_n, 10);
        assert_eq!(config.credit_per_kg_p, 20);
        assert_eq!(config.min_stake, 1000);
        assert_eq!(config.unstake_cooldown_secs, 86400);
        assert_eq!(config.commit_phase_secs, 300);
        assert_eq!(config.reveal_phase_secs, 300);
    }

    #[test]
    fn test_transfer_admin_succeeds() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let new_admin = Address::generate(&e);
        client.transfer_admin(&admin, &new_admin);

        // New admin can now perform admin actions.
        let oracle = Address::generate(&e);
        client.add_oracle(&new_admin, &oracle);
        assert!(client.is_oracle_active(&oracle));
    }

    #[test]
    #[should_panic(expected = "unauthorized")]
    fn test_transfer_admin_old_admin_rejected() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let new_admin = Address::generate(&e);
        client.transfer_admin(&admin, &new_admin);

        // Old admin can no longer act as admin.
        let oracle = Address::generate(&e);
        client.add_oracle(&admin, &oracle);
    }

    #[test]
    fn test_add_oracle_succeeds() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);
        client.add_oracle(&admin, &oracle);
        assert!(client.is_oracle_active(&oracle));
    }

    #[test]
    fn test_add_oracle_already_active() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);
        client.add_oracle(&admin, &oracle);
        assert!(client.is_oracle_active(&oracle));
    }

    #[test]
    fn test_remove_oracle_succeeds() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        let o4 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        client.add_oracle(&admin, &o4);
        client.remove_oracle(&admin, &o4);
        assert!(!client.is_oracle_active(&o4));
    }

    #[test]
    fn test_remove_oracle_above_minimum() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        let o4 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        client.add_oracle(&admin, &o4);
        client.remove_oracle(&admin, &o4);
        assert!(!client.is_oracle_active(&o4));
        assert!(client.is_oracle_active(&o1));
        assert!(client.is_oracle_active(&o2));
        assert!(client.is_oracle_active(&o3));
    }

    #[test]
    fn test_authorized_add_oracle_succeeds() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);
        client.add_oracle(&admin, &oracle);
        assert!(client.is_oracle_active(&oracle));
    }

    #[test]
    fn test_oracle_submission_works() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);
        client.add_oracle(&admin, &oracle);

        let project_id = BytesN::from_array(&e, &[1u8; 32]);
        client.submit_reading(&oracle, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
    }

    #[test]
    fn test_multi_oracle_aggregation_triggers_finalization() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[2u8; 32]);

        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &1, &710, &12, &75, &480, &260, &9, &1);
        let result =
            client.submit_reading(&o3, &project_id, &1, &690, &11, &78, &510, &245, &7, &1);

        assert!(result.is_some());
        let res = result.unwrap();
        assert!(res.total_credits > 0);
        assert_eq!(res.oracle_count, 3);
    }

    #[test]
    fn test_finalized_window_has_result() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[3u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o3, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);

        let result = client.get_last_result(&project_id);
        assert!(result.is_some());
        assert_eq!(result.unwrap().oracle_count, 3);
    }

    #[test]
    fn test_get_last_result_after_finalization() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[4u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o3, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);

        let result = client.get_last_result(&project_id);
        assert!(result.is_some());
        assert_eq!(result.unwrap().oracle_count, 3);
    }

    #[test]
    fn test_get_last_result_none_before_finalization() {
        let (e, _admin, client) = setup_with_client();
        e.mock_all_auths();

        let project_id = BytesN::from_array(&e, &[5u8; 32]);
        let result = client.get_last_result(&project_id);
        assert!(result.is_none());
    }

    #[test]
    fn test_result_history_accumulates_across_windows() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[50u8; 32]);

        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o3, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);

        let history = client.get_result_history(&project_id, &0, &10);
        assert_eq!(history.len(), 1);

        client.reset_window(&admin, &project_id);
        client.submit_reading(&o1, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o3, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);

        let history = client.get_result_history(&project_id, &0, &10);
        assert_eq!(history.len(), 2);

        assert_eq!(history.get(0).unwrap().oracle_count, 3);
        assert_eq!(history.get(1).unwrap().oracle_count, 3);
    }

    #[test]
    fn test_config_update_succeeds() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let new_config = OracleConfig {
            min_oracles: 5,
            max_oracles: 15,
            quality_threshold_ph: 550,
            quality_threshold_turbidity: 40,
            quality_threshold_do: 60,
            quality_threshold_temp: 310,
            credit_per_kg_n: 15,
            credit_per_kg_p: 25,
            staking_token: Address::generate(&e),
            treasury: Address::generate(&e),
            min_stake: 2000,
            unstake_cooldown_secs: 172800,
            commit_phase_secs: 600,
            reveal_phase_secs: 600,
        };
        client.update_config(&admin, &new_config);

        let config = client.get_config();
        assert_eq!(config.min_oracles, 5);
        assert_eq!(config.credit_per_kg_n, 15);
        assert_eq!(config.min_stake, 2000);
    }

    #[test]
    fn test_math_high_np_zero_removal() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[6u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &15, &5);
        client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &500, &250, &15, &5);
        let result =
            client.submit_reading(&o3, &project_id, &1, &700, &10, &80, &500, &250, &15, &5);

        assert!(result.is_some());
        let res = result.unwrap();
        assert_eq!(res.n_removal_kg, 0);
        assert_eq!(res.p_removal_kg, 0);
    }

    #[test]
    fn test_penalty_boundaries() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[7u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &300, &200, &10, &500, &350, &8, &1);
        client.submit_reading(&o2, &project_id, &1, &300, &200, &10, &500, &350, &8, &1);
        let result =
            client.submit_reading(&o3, &project_id, &1, &300, &200, &10, &500, &350, &8, &1);

        assert!(result.is_some());
        assert_eq!(result.unwrap().quality_penalty, 7000);
    }

    #[test]
    fn test_oracle_submit_count_increments() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        assert_eq!(client.oracle_submit_count(&o1), 0);
        assert_eq!(client.total_submissions(), 0);

        let project_id = BytesN::from_array(&e, &[10u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        assert_eq!(client.oracle_submit_count(&o1), 1);
        assert_eq!(client.total_submissions(), 1);

        let project_id2 = BytesN::from_array(&e, &[11u8; 32]);
        client.submit_reading(&o2, &project_id2, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o3, &project_id2, &1, &700, &10, &80, &500, &250, &8, &1);
        assert_eq!(client.oracle_submit_count(&o2), 1);
        assert_eq!(client.oracle_submit_count(&o3), 1);
        assert_eq!(client.total_submissions(), 3);
    }

    #[test]
    fn test_nonce_independent_across_projects() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        client.add_oracle(&admin, &o1);

        let p1 = BytesN::from_array(&e, &[50u8; 32]);
        let p2 = BytesN::from_array(&e, &[51u8; 32]);
        let p3 = BytesN::from_array(&e, &[52u8; 32]);

        client.submit_reading(&o1, &p1, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o1, &p2, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o1, &p3, &1, &700, &10, &80, &500, &250, &8, &1);

        client.submit_reading(&o1, &p1, &2, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o1, &p2, &2, &700, &10, &80, &500, &250, &8, &1);
    }

    #[test]
    fn test_oracle_count_tracks_additions_and_removals() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        assert_eq!(client.oracle_count(), 0);

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        assert_eq!(client.oracle_count(), 1);

        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        assert_eq!(client.oracle_count(), 3);

        client.remove_oracle(&admin, &o2);
        assert_eq!(client.oracle_count(), 2);
    }

    #[test]
    fn test_get_oracles_returns_active_list() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = client.get_oracles();
        assert_eq!(oracles.len(), 0);

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let oracles = client.get_oracles();
        assert_eq!(oracles.len(), 3);
        assert!(oracles.contains(&o1));
        assert!(oracles.contains(&o2));
        assert!(oracles.contains(&o3));

        client.remove_oracle(&admin, &o2);
        let oracles = client.get_oracles();
        assert_eq!(oracles.len(), 2);
        assert!(oracles.contains(&o1));
        assert!(!oracles.contains(&o2));
        assert!(oracles.contains(&o3));
    }

    #[test]
    fn test_reset_window_clears_submissions() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[30u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        assert_eq!(client.window_submission_count(&project_id), 2);

        client.reset_window(&admin, &project_id);
        assert_eq!(client.window_submission_count(&project_id), 0);
    }

    #[test]
    fn test_oracles_can_resubmit_after_reset() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[31u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);

        client.reset_window(&admin, &project_id);

        client.submit_reading(&o1, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        let result =
            client.submit_reading(&o3, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);

        assert!(result.is_some());
        assert_eq!(result.unwrap().oracle_count, 3);
    }

    #[test]
    fn test_zero_flow_produces_zero_volumetric_credit() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[40u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &0, &250, &2, &0);
        client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &0, &250, &2, &0);
        let result = client.submit_reading(&o3, &project_id, &1, &700, &10, &80, &0, &250, &2, &0);

        assert!(result.is_some());
        let res = result.unwrap();
        assert_eq!(res.volumetric_credit, 0);
        assert_eq!(res.n_removal_kg, 0);
        assert_eq!(res.p_removal_kg, 0);
        assert_eq!(res.total_credits, 0);
    }

    #[test]
    fn test_single_oracle_submission_does_not_finalize() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[41u8; 32]);
        let result =
            client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);

        assert!(result.is_none());
        assert!(client.get_last_result(&project_id).is_none());
    }

    #[test]
    fn test_two_oracle_submissions_does_not_finalize() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[42u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        let result =
            client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);

        assert!(result.is_none());
        assert!(client.get_last_result(&project_id).is_none());
    }

    #[test]
    fn test_all_zero_readings_no_credits_no_removal() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[43u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &300, &200, &10, &0, &350, &20, &5);
        client.submit_reading(&o2, &project_id, &1, &300, &200, &10, &0, &350, &20, &5);
        let result =
            client.submit_reading(&o3, &project_id, &1, &300, &200, &10, &0, &350, &20, &5);

        assert!(result.is_some());
        let res = result.unwrap();
        assert_eq!(res.volumetric_credit, 0);
        assert_eq!(res.n_removal_kg, 0);
        assert_eq!(res.p_removal_kg, 0);
        assert_eq!(res.total_credits, 0);
    }

    #[test]
    fn test_median_with_even_number_of_oracles_uses_lower_middle() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let mut config = client.get_config();
        config.min_oracles = 2;
        client.update_config(&admin, &config);

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);

        let project_id = BytesN::from_array(&e, &[44u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &400, &250, &8, &1);
        let result =
            client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &600, &250, &8, &1);

        assert!(result.is_some());
        let res = result.unwrap();
        assert_eq!(res.volumetric_credit, 50);
    }

    // ── Staking & Slashing Tests ──

    #[test]
    fn test_stake_increases_balance() {
        let (e, _admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &5000);
        let info = client.get_stake(&oracle);
        assert_eq!(info.amount, 5000);
        assert!(info.unstake_request.is_none());
    }

    #[test]
    fn test_stake_accumulates() {
        let (e, _admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &2000);
        client.stake(&oracle, &3000);
        let info = client.get_stake(&oracle);
        assert_eq!(info.amount, 5000);
    }

    #[test]
    fn test_stake_zero_panics() {
        let (e, _admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        let result = client.try_stake(&oracle, &0);
        assert!(result.is_err());
    }

    #[test]
    fn test_unstake_reduces_balance() {
        let (e, _admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &5000);
        client.unstake(&oracle, &2000);
        let info = client.get_stake(&oracle);
        assert_eq!(info.amount, 3000);
        assert!(info.unstake_request.is_some());
    }

    #[test]
    fn test_unstake_insufficient_balance_panics() {
        let (e, _admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &1000);
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "unstake"),
            vec![&e, oracle.to_val(), 2000i128.into_val(&e)],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_unstake_below_min_stake_for_active_oracle_panics() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &1500);
        client.add_oracle(&admin, &oracle);

        // min_stake is 1000, staking 1500, trying to unstake 600 would leave 900 < 1000
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "unstake"),
            vec![&e, oracle.to_val(), 600i128.into_val(&e)],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_unstake_active_oracle_can_unstake_to_min() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &2000);
        client.add_oracle(&admin, &oracle);

        // Unstake 1000, leaving exactly min_stake = 1000
        client.unstake(&oracle, &1000);
        let info = client.get_stake(&oracle);
        assert_eq!(info.amount, 1000);
    }

    #[test]
    fn test_stake_clears_unstake_request() {
        let (e, _admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &5000);
        client.unstake(&oracle, &2000);
        let info = client.get_stake(&oracle);
        assert!(info.unstake_request.is_some());

        client.stake(&oracle, &1000);
        let info = client.get_stake(&oracle);
        assert!(info.unstake_request.is_none());
        assert_eq!(info.amount, 4000);
    }

    #[test]
    fn test_slash_reduces_stake() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &5000);
        client.slash(&admin, &oracle, &2000, &1);
        let info = client.get_stake(&oracle);
        assert_eq!(info.amount, 3000);
    }

    #[test]
    fn test_slash_records_reason() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &5000);
        client.slash(&admin, &oracle, &2000, &1);
        let record = client.get_slash_record(&oracle);
        assert!(record.is_some());
        let rec = record.unwrap();
        assert_eq!(rec.reason, 1);
    }

    #[test]
    fn test_slash_fraud_proof_reason() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &5000);
        client.slash(&admin, &oracle, &5000, &2);
        let info = client.get_stake(&oracle);
        assert_eq!(info.amount, 0);
        let record = client.get_slash_record(&oracle).unwrap();
        assert_eq!(record.reason, 2);
    }

    #[test]
    fn test_slash_exceeds_stake_panics() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &1000);
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "slash"),
            vec![
                &e,
                admin.to_val(),
                oracle.to_val(),
                2000i128.into_val(&e),
                1u32.into_val(&e),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_slash_unauthorized_panics() {
        let (e, _admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);
        let rando = Address::generate(&e);

        client.stake(&oracle, &5000);
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "slash"),
            vec![
                &e,
                rando.to_val(),
                oracle.to_val(),
                1000i128.into_val(&e),
                1u32.into_val(&e),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_add_oracle_requires_min_stake() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        // min_stake is 1000 by default, oracle has 0 stake
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "add_oracle"),
            vec![&e, admin.to_val(), oracle.to_val()],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_add_oracle_with_sufficient_stake() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &1500);
        client.add_oracle(&admin, &oracle);
        assert!(client.is_oracle_active(&oracle));
    }

    #[test]
    fn test_remove_oracle_requires_unstake() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        let o4 = Address::generate(&e);

        client.stake(&o1, &1500);
        client.stake(&o2, &1500);
        client.stake(&o3, &1500);
        client.stake(&o4, &1500);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        client.add_oracle(&admin, &o4);

        // Cannot remove while staked
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "remove_oracle"),
            vec![&e, admin.to_val(), o4.to_val()],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_oracle_after_full_unstake() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        let o4 = Address::generate(&e);

        client.stake(&o1, &1500);
        client.stake(&o2, &1500);
        client.stake(&o3, &1500);
        client.stake(&o4, &1500);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        client.add_oracle(&admin, &o4);

        // Set min_stake to 0 so full unstake is allowed
        let mut config = client.get_config();
        config.min_stake = 0;
        client.update_config(&admin, &config);

        client.unstake(&o4, &1500);
        client.remove_oracle(&admin, &o4);
        assert!(!client.is_oracle_active(&o4));
    }

    #[test]
    fn test_submit_reading_requires_min_stake() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        // Force-add oracle bypassing the stake check by using update_config
        let mut config = client.get_config();
        config.min_stake = 0;
        client.update_config(&admin, &config);
        client.add_oracle(&admin, &oracle);

        // Now re-enable min_stake
        config.min_stake = 5000;
        client.update_config(&admin, &config);

        let project_id = BytesN::from_array(&e, &[1u8; 32]);
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "submit_reading"),
            vec![
                &e,
                oracle.to_val(),
                project_id.to_val(),
                1u64.into_val(&e),
                700i64.into_val(&e),
                10i64.into_val(&e),
                80i64.into_val(&e),
                500i64.into_val(&e),
                250i64.into_val(&e),
                8i64.into_val(&e),
                1i64.into_val(&e),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_submit_reading_with_sufficient_stake() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &2000);
        client.add_oracle(&admin, &oracle);

        let project_id = BytesN::from_array(&e, &[1u8; 32]);
        client.submit_reading(&oracle, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
    }

    #[test]
    fn test_claim_unstake_before_cooldown_panics() {
        let (e, _admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &5000);
        client.unstake(&oracle, &2000);

        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "claim_unstake"),
            vec![&e, oracle.to_val()],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_getters_return_config_values() {
        let (_e, _admin, client) = setup_with_client();

        let cooldown = client.get_unstake_cooldown();
        assert_eq!(cooldown, 86400);

        let _treasury = client.get_treasury();
        let _staking_token = client.get_staking_token();
    }

    #[test]
    fn test_initial_stake_is_zero() {
        let (e, _admin, client) = setup_with_client();
        let oracle = Address::generate(&e);
        let info = client.get_stake(&oracle);
        assert_eq!(info.amount, 0);
        assert!(info.unstake_request.is_none());
    }

    #[test]
    fn test_initial_slash_record_is_none() {
        let (e, _admin, client) = setup_with_client();
        let oracle = Address::generate(&e);
        assert!(client.get_slash_record(&oracle).is_none());
    }

    #[test]
    fn test_full_stake_slash_unstake_lifecycle() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        // Stake
        client.stake(&oracle, &10000);
        assert_eq!(client.get_stake(&oracle).amount, 10000);

        // Add as oracle
        client.add_oracle(&admin, &oracle);
        assert!(client.is_oracle_active(&oracle));

        // Slash partial
        client.slash(&admin, &oracle, &3000, &1);
        assert_eq!(client.get_stake(&oracle).amount, 7000);
        assert_eq!(client.get_slash_record(&oracle).unwrap().reason, 1);

        // Slash rest
        client.slash(&admin, &oracle, &7000, &2);
        assert_eq!(client.get_stake(&oracle).amount, 0);
        assert_eq!(client.get_slash_record(&oracle).unwrap().reason, 2);
    }

    // ── Commit-Reveal Scheme Tests ──

    fn setup_oracles_with_stakes(
        e: &Env,
        admin: &Address,
        client: &VerificationOracleClient<'static>,
        count: u32,
        stake: i128,
    ) -> Vec<Address> {
        let mut oracles = Vec::new(e);
        for _ in 0..count {
            let o = Address::generate(e);
            client.stake(&o, &stake);
            client.add_oracle(admin, &o);
            oracles.push_back(o);
        }
        oracles
    }

    fn make_reveal_params(
        _e: &Env,
        nonce: u64,
        ph: i64,
        turbidity: i64,
        dissolved_oxygen: i64,
        flow_rate: i64,
        temperature: i64,
        total_nitrogen: i64,
        total_phosphorus: i64,
        salt: &BytesN<32>,
    ) -> RevealParams {
        RevealParams {
            nonce,
            ph,
            turbidity,
            dissolved_oxygen,
            flow_rate,
            temperature,
            total_nitrogen,
            total_phosphorus,
            salt: salt.clone(),
        }
    }

    #[test]
    fn test_commit_reveal_happy_path() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);

        let project_id = BytesN::from_array(&e, &[100u8; 32]);
        client.open_window(&admin, &project_id);

        let phase = client.get_window_phase(&project_id);
        assert_eq!(phase.unwrap(), WindowPhase::Commit);

        let salt = BytesN::from_array(&e, &[0xAAu8; 32]);
        let nonce: u64 = 1;

        // Compute expected hash off-chain and commit
        for i in 0..3u32 {
            let o = oracles.get(i).unwrap();
            let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
            client.commit_reading(&o, &project_id, &nonce, &commitment);
        }

        // Advance time past commit phase
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);

        client.begin_reveal_phase(&project_id);
        let phase = client.get_window_phase(&project_id);
        assert_eq!(phase.unwrap(), WindowPhase::Reveal);

        // All oracles reveal
        let params = make_reveal_params(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        let result = client.reveal_reading(&oracles.get(0).unwrap(), &project_id, &params);
        assert!(result.is_none()); // not finalized yet

        client.reveal_reading(&oracles.get(1).unwrap(), &project_id, &params);

        let result = client.reveal_reading(&oracles.get(2).unwrap(), &project_id, &params);

        assert!(result.is_some());
        let res = result.unwrap();
        assert!(res.total_credits > 0);
        assert_eq!(res.oracle_count, 3);

        let phase = client.get_window_phase(&project_id);
        assert_eq!(phase.unwrap(), WindowPhase::Finalized);
    }

    #[test]
    fn test_commit_reveal_hash_mismatch_panics() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);

        let project_id = BytesN::from_array(&e, &[101u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0xBBu8; 32]);
        let nonce: u64 = 1;
        let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&oracles.get(0).unwrap(), &project_id, &nonce, &commitment);

        // Advance to reveal phase
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);

        // Try to reveal with wrong values (different salt)
        let wrong_salt = BytesN::from_array(&e, &[0xCCu8; 32]);
        let wrong_params = make_reveal_params(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &wrong_salt);
        let result = e.try_invoke_contract::<Option<VerificationResult>, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "reveal_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                wrong_params.into_val(&e),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_late_reveal_after_phase_ends_panics() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);

        let project_id = BytesN::from_array(&e, &[102u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0xDDu8; 32]);
        let nonce: u64 = 1;
        let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&oracles.get(0).unwrap(), &project_id, &nonce, &commitment);

        // Advance past both commit and reveal phases
        set_ledger_timestamp(&e, e.ledger().timestamp() + 601);

        // Trying to reveal after reveal phase ended should panic
        let params = make_reveal_params(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        let result = e.try_invoke_contract::<Option<VerificationResult>, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "reveal_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                params.into_val(&e),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_commit_without_reveal_penalized() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 4, 1500);

        let project_id = BytesN::from_array(&e, &[103u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0xEEu8; 32]);
        let nonce: u64 = 1;

        // All 4 oracles commit
        for i in 0..4u32 {
            let o = oracles.get(i).unwrap();
            let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
            client.commit_reading(&o, &project_id, &nonce, &commitment);
        }

        // Advance to reveal phase
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);

        // Only 3 out of 4 oracles reveal
        let params = make_reveal_params(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        for i in 0..3u32 {
            let o = oracles.get(i).unwrap();
            client.reveal_reading(&o, &project_id, &params);
        }

        // Advance past reveal phase
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);

        // finalize_window penalizes the non-revealer
        let result = client.finalize_window(&project_id);
        assert!(result.is_some());
        let res = result.unwrap();
        assert_eq!(res.oracle_count, 3);

        // The 4th oracle should have a missed reveal
        let missed = client.oracle_missed_reveals(&oracles.get(3).unwrap());
        assert_eq!(missed, 1);

        // The 4th oracle should be slashed
        let slash = client.get_slash_record(&oracles.get(3).unwrap());
        assert!(slash.is_some());
        assert_eq!(slash.unwrap().reason, 3); // missed_reveal
    }

    #[test]
    fn test_open_window_requires_admin() {
        let (e, _admin, client) = setup_with_client();
        e.mock_all_auths();

        let rando = Address::generate(&e);
        let project_id = BytesN::from_array(&e, &[104u8; 32]);

        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "open_window"),
            vec![&e, rando.to_val(), project_id.to_val()],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_cannot_open_window_while_active() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let project_id = BytesN::from_array(&e, &[105u8; 32]);
        client.open_window(&admin, &project_id);

        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "open_window"),
            vec![&e, admin.to_val(), project_id.to_val()],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_commit_requires_active_oracle() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let project_id = BytesN::from_array(&e, &[106u8; 32]);
        client.open_window(&admin, &project_id);

        let inactive = Address::generate(&e);
        let commitment = BytesN::from_array(&e, &[0xFFu8; 32]);
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "commit_reading"),
            vec![
                &e,
                inactive.to_val(),
                project_id.to_val(),
                1u64.into_val(&e),
                commitment.to_val(),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_cannot_commit_twice() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);

        let project_id = BytesN::from_array(&e, &[107u8; 32]);
        client.open_window(&admin, &project_id);

        let commitment = BytesN::from_array(&e, &[0x11u8; 32]);
        let nonce: u64 = 1;
        client.commit_reading(&oracles.get(0).unwrap(), &project_id, &nonce, &commitment);

        // Second commit from same oracle should fail
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "commit_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                nonce.into_val(&e),
                commitment.to_val(),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_cannot_reveal_without_committing() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);

        let project_id = BytesN::from_array(&e, &[108u8; 32]);
        client.open_window(&admin, &project_id);

        // Skip commit phase
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);

        let salt = BytesN::from_array(&e, &[0x22u8; 32]);
        let params = make_reveal_params(&e, 1, 700, 10, 80, 500, 250, 8, 1, &salt);
        let result = e.try_invoke_contract::<Option<VerificationResult>, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "reveal_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                params.into_val(&e),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_begin_reveal_phase_requires_commit_duration_elapsed() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let project_id = BytesN::from_array(&e, &[109u8; 32]);
        client.open_window(&admin, &project_id);

        // Try to transition before commit phase ends
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "begin_reveal_phase"),
            vec![&e, project_id.to_val()],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_finalize_window_requires_reveal_duration_elapsed() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);

        let project_id = BytesN::from_array(&e, &[110u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0x33u8; 32]);
        let nonce: u64 = 1;
        for i in 0..3u32 {
            let o = oracles.get(i).unwrap();
            let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
            client.commit_reading(&o, &project_id, &nonce, &commitment);
        }

        // Advance to reveal phase
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);

        // All oracles reveal
        let params = make_reveal_params(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        for i in 0..3u32 {
            let o = oracles.get(i).unwrap();
            client.reveal_reading(&o, &project_id, &params);
        }

        // Try to finalize_window before reveal phase ends should fail (already auto-finalized)
        let result = e.try_invoke_contract::<Option<VerificationResult>, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "finalize_window"),
            vec![&e, project_id.to_val()],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_cannot_reveal_twice() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);

        let project_id = BytesN::from_array(&e, &[111u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0x44u8; 32]);
        let nonce: u64 = 1;
        let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&oracles.get(0).unwrap(), &project_id, &nonce, &commitment);

        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);

        let params = make_reveal_params(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.reveal_reading(&oracles.get(0).unwrap(), &project_id, &params);

        // Second reveal should fail
        let result = e.try_invoke_contract::<Option<VerificationResult>, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "reveal_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                params.into_val(&e),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_commit_requires_valid_nonce() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);

        let project_id = BytesN::from_array(&e, &[112u8; 32]);
        client.open_window(&admin, &project_id);

        let commitment = BytesN::from_array(&e, &[0x55u8; 32]);

        // First oracle tries to commit with wrong nonce (should be 1)
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "commit_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                5u64.into_val(&e),
                commitment.to_val(),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_hash_deterministic() {
        let (e, _admin, _client) = setup_with_client();

        let salt = BytesN::from_array(&e, &[0xAAu8; 32]);
        let h1 = sha256_commitment(&e, 1, 700, 10, 80, 500, 250, 8, 1, &salt);
        let h2 = sha256_commitment(&e, 1, 700, 10, 80, 500, 250, 8, 1, &salt);
        assert_eq!(h1, h2);

        // Different values produce different hashes
        let h3 = sha256_commitment(&e, 1, 701, 10, 80, 500, 250, 8, 1, &salt);
        assert_ne!(h1, h3);

        // Different salts produce different hashes
        let salt2 = BytesN::from_array(&e, &[0xBBu8; 32]);
        let h4 = sha256_commitment(&e, 1, 700, 10, 80, 500, 250, 8, 1, &salt2);
        assert_ne!(h1, h4);
    }

    #[test]
    fn test_finalize_window_with_insufficient_reveals() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 5, 1500);

        let project_id = BytesN::from_array(&e, &[113u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0x66u8; 32]);
        let nonce: u64 = 1;

        // All 5 commit
        for i in 0..5u32 {
            let o = oracles.get(i).unwrap();
            let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
            client.commit_reading(&o, &project_id, &nonce, &commitment);
        }

        // Advance to reveal phase
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);

        // Only 2 reveal (below min_oracles=3)
        let params = make_reveal_params(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.reveal_reading(&oracles.get(0).unwrap(), &project_id, &params);
        client.reveal_reading(&oracles.get(1).unwrap(), &project_id, &params);

        // Advance past reveal phase
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);

        // finalize_window - but with only 2 reveals (below min), no result
        let result = client.finalize_window(&project_id);
        assert!(result.is_none());

        // But the 3 non-revealers should be penalized
        for i in 2..5u32 {
            let missed = client.oracle_missed_reveals(&oracles.get(i).unwrap());
            assert_eq!(missed, 1);
        }
    }

    #[test]
    fn test_reset_window_clears_commit_reveal_state() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);

        let project_id = BytesN::from_array(&e, &[114u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0x77u8; 32]);
        let nonce: u64 = 1;
        let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&oracles.get(0).unwrap(), &project_id, &nonce, &commitment);

        // Reset should work on commit-phase window
        client.reset_window(&admin, &project_id);

        // Oracle should be able to re-commit with the next nonce
        let nonce2: u64 = 2;
        let commitment2 = sha256_commitment(&e, nonce2, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&oracles.get(0).unwrap(), &project_id, &nonce2, &commitment2);
    }

    #[test]
    fn test_commit_requires_min_stake() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        // Add oracle with no stake (min_stake=0 first)
        let mut config = client.get_config();
        config.min_stake = 0;
        client.update_config(&admin, &config);

        let oracle = Address::generate(&e);
        client.add_oracle(&admin, &oracle);

        let project_id = BytesN::from_array(&e, &[115u8; 32]);
        client.open_window(&admin, &project_id);

        // Re-enable min_stake
        config.min_stake = 5000;
        client.update_config(&admin, &config);

        let commitment = BytesN::from_array(&e, &[0x88u8; 32]);
        let result = e.try_invoke_contract::<Val, soroban_sdk::Error>(
            &client.address,
            &Symbol::new(&e, "commit_reading"),
            vec![
                &e,
                oracle.to_val(),
                project_id.to_val(),
                1u64.into_val(&e),
                commitment.to_val(),
            ],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_finalize_window_after_reveal_phase_penalizes_all_non_revealers() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 4, 1500);

        let project_id = BytesN::from_array(&e, &[116u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0x99u8; 32]);
        let nonce: u64 = 1;

        // All 4 commit
        for i in 0..4u32 {
            let o = oracles.get(i).unwrap();
            let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
            client.commit_reading(&o, &project_id, &nonce, &commitment);
        }

        // Advance to reveal phase
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);

        // Only oracle 0 reveals
        let params = make_reveal_params(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.reveal_reading(&oracles.get(0).unwrap(), &project_id, &params);

        // Advance past reveal phase
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);

        let result = client.finalize_window(&project_id);
        assert!(result.is_none()); // Only 1 reveal, below min_oracles

        // Oracles 1, 2, 3 should all have missed reveals
        for i in 1..4u32 {
            let missed = client.oracle_missed_reveals(&oracles.get(i).unwrap());
            assert_eq!(missed, 1);
            let slash = client.get_slash_record(&oracles.get(i).unwrap());
            assert!(slash.is_some());
            assert_eq!(slash.unwrap().reason, 3);
        }
    }

    // ── Zero-credit window fix (issue #24) ──

    /// Three oracles submit readings that produce zero credits (zero flow, N and P
    /// at or above baseline, bad quality).  The window must finalize cleanly,
    /// get_last_result must return Some with total_credits == 0, and all oracle
    /// nonces must have advanced so the oracles can participate in the next window.
    #[test]
    fn test_zero_credit_window_finalizes_and_nonces_advance() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[200u8; 32]);

        // Readings that produce zero credits:
        //   flow_rate = 0  → volumetric_credit = 0
        //   total_nitrogen = 15 (≥ baseline 10) → n_removal = 0
        //   total_phosphorus = 3 (≥ baseline 2)  → p_removal = 0
        //   Poor quality (ph=300, turb=200, do=10) → large penalty, but gross is
        //   already 0 so total stays 0.
        let result1 =
            client.submit_reading(&o1, &project_id, &1, &300, &200, &10, &0, &350, &15, &3);
        assert!(
            result1.is_none(),
            "window should not finalize after 1 oracle"
        );

        let result2 =
            client.submit_reading(&o2, &project_id, &1, &300, &200, &10, &0, &350, &15, &3);
        assert!(
            result2.is_none(),
            "window should not finalize after 2 oracles"
        );

        let result3 =
            client.submit_reading(&o3, &project_id, &1, &300, &200, &10, &0, &350, &15, &3);

        // Window must finalize and return a result even though total_credits == 0.
        assert!(
            result3.is_some(),
            "window must finalize when min_oracles reached"
        );
        let res = result3.unwrap();
        assert_eq!(
            res.total_credits, 0,
            "credits should be zero for this reading"
        );
        assert_eq!(res.oracle_count, 3);

        // get_last_result must reflect the finalized zero-credit result.
        let stored = client.get_last_result(&project_id);
        assert!(
            stored.is_some(),
            "get_last_result must return Some after finalization"
        );
        assert_eq!(stored.unwrap().total_credits, 0);

        // Oracle nonces must have advanced (each oracle consumed nonce 1).
        // Verify indirectly: after reset_window, all three oracles must accept nonce 2
        // (not nonce 1).  If the fix were broken, the stored nonce would still be 0 and
        // nonce 1 would be accepted, but nonce 2 would be rejected as "invalid nonce".
        // A successful three-oracle submission with nonce 2 proves all nonces advanced.
        client.reset_window(&admin, &project_id);

        // nonce 2 must be accepted for all three oracles
        client.submit_reading(&o1, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        let next_result =
            client.submit_reading(&o3, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);

        // The second window also produces zero credits (same readings), but it must finalize.
        assert!(
            next_result.is_some(),
            "nonce 2 must be accepted after zero-credit window advanced nonces"
        );
        assert_eq!(
            next_result.unwrap().total_credits,
            0,
            "same zero-credit readings must still produce zero credits"
        );
    }

    /// After a zero-credit window finalizes, reset_window + a new window with
    /// positive credits must work end-to-end without any state corruption.
    #[test]
    fn test_positive_credit_window_after_zero_credit_window() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);

        let project_id = BytesN::from_array(&e, &[201u8; 32]);

        // ── Window 1: zero credits ──
        client.submit_reading(&o1, &project_id, &1, &300, &200, &10, &0, &350, &15, &3);
        client.submit_reading(&o2, &project_id, &1, &300, &200, &10, &0, &350, &15, &3);
        let zero_result =
            client.submit_reading(&o3, &project_id, &1, &300, &200, &10, &0, &350, &15, &3);

        assert!(zero_result.is_some());
        assert_eq!(zero_result.unwrap().total_credits, 0);

        // ── Window 2: positive credits ──
        // reset_window is required to open a new direct-submission window after
        // the previous one was finalized.
        client.reset_window(&admin, &project_id);

        // Good readings: good pH (700=7.0), low turbidity (10), high DO (80),
        // positive flow (500), low temperature (250), N below baseline (8 < 10),
        // P below baseline (1 < 2).
        let r1 = client.submit_reading(&o1, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        assert!(r1.is_none());

        let r2 = client.submit_reading(&o2, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        assert!(r2.is_none());

        let r3 = client.submit_reading(&o3, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        assert!(r3.is_some(), "second window must finalize");

        let res = r3.unwrap();
        assert!(
            res.total_credits > 0,
            "second window must produce positive credits"
        );
        assert_eq!(res.oracle_count, 3);

        // get_last_result must now reflect the positive-credit window.
        let stored = client.get_last_result(&project_id).unwrap();
        assert!(stored.total_credits > 0);

        // History must contain both results.
        let history = client.get_result_history(&project_id, &0, &10);
        assert_eq!(history.len(), 2, "history must contain both windows");
        assert_eq!(history.get(0).unwrap().total_credits, 0);
        assert!(history.get(1).unwrap().total_credits > 0);
    }

    // ── remove_oracle open-window guard tests ──

    #[test]
    fn test_remove_oracle_with_open_submissions_panics() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        let o4 = Address::generate(&e);
        let mut config = client.get_config();
        config.min_stake = 0;
        client.update_config(&admin, &config);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        client.add_oracle(&admin, &o4);

        let project_id = BytesN::from_array(&e, &[210u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);

        let result = e.try_invoke_contract::<_, ()>(
            &client.address,
            &Symbol::new(&e, "remove_oracle"),
            vec![&e, admin.to_val(), o1.to_val()],
        );
        assert!(result.is_err());
        assert!(client.is_oracle_active(&o1));
    }

    #[test]
    fn test_remove_oracle_no_open_submissions_succeeds() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        let o4 = Address::generate(&e);
        let mut config = client.get_config();
        config.min_stake = 0;
        client.update_config(&admin, &config);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        client.add_oracle(&admin, &o4);

        let project_id = BytesN::from_array(&e, &[211u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o3, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);

        client.remove_oracle(&admin, &o4);
        assert!(!client.is_oracle_active(&o4));
    }

    #[test]
    fn test_remove_oracle_after_window_finalization_succeeds() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        let o4 = Address::generate(&e);
        let mut config = client.get_config();
        config.min_stake = 0;
        client.update_config(&admin, &config);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        client.add_oracle(&admin, &o4);

        let project_id = BytesN::from_array(&e, &[212u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o3, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);

        client.remove_oracle(&admin, &o4);
        assert!(!client.is_oracle_active(&o4));

        let result = client.get_last_result(&project_id);
        assert!(result.is_some());
        assert_eq!(result.unwrap().oracle_count, 3);
    }

    #[test]
    fn test_window_finalizes_after_oracle_removed_between_windows() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        let o4 = Address::generate(&e);
        let mut config = client.get_config();
        config.min_stake = 0;
        client.update_config(&admin, &config);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        client.add_oracle(&admin, &o4);

        let project_id = BytesN::from_array(&e, &[213u8; 32]);
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o3, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);

        let first = client.get_last_result(&project_id);
        assert!(first.is_some());
        assert_eq!(first.unwrap().oracle_count, 3);

        client.remove_oracle(&admin, &o4);
        assert!(!client.is_oracle_active(&o4));

        client.reset_window(&admin, &project_id);
        client.submit_reading(&o1, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        let result =
            client.submit_reading(&o3, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);

        assert!(result.is_some());
        assert_eq!(result.unwrap().oracle_count, 3);
    }

    #[test]
    fn test_remove_oracle_with_open_window_panics() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        let o4 = Address::generate(&e);
        let mut config = client.get_config();
        config.min_stake = 0;
        client.update_config(&admin, &config);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        client.add_oracle(&admin, &o4);

        let project_id = BytesN::from_array(&e, &[214u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0xA1u8; 32]);
        let commitment = sha256_commitment(&e, 1, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&o1, &project_id, &1, &commitment);

        let result = e.try_invoke_contract::<_, ()>(
            &client.address,
            &Symbol::new(&e, "remove_oracle"),
            vec![&e, admin.to_val(), o1.to_val()],
        );
        assert!(result.is_err());
        assert!(client.is_oracle_active(&o1));
    }

    #[test]
    fn test_reset_window_cleans_committed_markers_enabling_removal() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        let o4 = Address::generate(&e);
        let mut config = client.get_config();
        config.min_stake = 0;
        client.update_config(&admin, &config);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        client.add_oracle(&admin, &o4);

        let project_id = BytesN::from_array(&e, &[215u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0xB1u8; 32]);
        let commitment = sha256_commitment(&e, 1, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&o1, &project_id, &1, &commitment);

        // Cannot remove o1 while it has an open committed marker
        let result = e.try_invoke_contract::<_, ()>(
            &client.address,
            &Symbol::new(&e, "remove_oracle"),
            vec![&e, admin.to_val(), o1.to_val()],
        );
        assert!(result.is_err());

        // Reset clears committed markers — o1 can now be removed
        client.reset_window(&admin, &project_id);
        client.remove_oracle(&admin, &o1);
        assert!(!client.is_oracle_active(&o1));
    }

    #[test]
    fn test_window_finalizes_with_min_oracles_after_one_removed() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let o1 = Address::generate(&e);
        let o2 = Address::generate(&e);
        let o3 = Address::generate(&e);
        let o4 = Address::generate(&e);
        let mut config = client.get_config();
        config.min_stake = 0;
        client.update_config(&admin, &config);
        client.add_oracle(&admin, &o1);
        client.add_oracle(&admin, &o2);
        client.add_oracle(&admin, &o3);
        client.add_oracle(&admin, &o4);

        let project_id = BytesN::from_array(&e, &[216u8; 32]);

        // First window: 3 oracles submit, finalize
        client.submit_reading(&o1, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        let result =
            client.submit_reading(&o3, &project_id, &1, &700, &10, &80, &500, &250, &8, &1);
        assert!(result.is_some());
        assert_eq!(result.unwrap().oracle_count, 3);

        // Remove o4 (never submitted to this window)
        client.remove_oracle(&admin, &o4);
        assert!(!client.is_oracle_active(&o4));

        // Reset and resubmit — 3 remaining oracles finalize the window
        client.reset_window(&admin, &project_id);
        client.submit_reading(&o1, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        client.submit_reading(&o2, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        let result =
            client.submit_reading(&o3, &project_id, &2, &700, &10, &80, &500, &250, &8, &1);
        assert!(result.is_some());
        assert_eq!(result.unwrap().oracle_count, 3);
    }
}
