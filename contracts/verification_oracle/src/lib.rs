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
    /// Per-project nitrogen baseline (mg/L, raw integer — same encoding as
    /// `total_nitrogen`; see doc/MATH.md §1). Falls back to the global
    /// `OracleConfig` default when unset (0).
    pub baseline_n: i64,
    /// Per-project phosphorus baseline (mg/L, raw integer — same encoding as
    /// `total_phosphorus`; see doc/MATH.md §1). Falls back to the global
    /// `OracleConfig` default when unset (0).
    pub baseline_p: i64,
    /// Per-project temperature baseline (×10 °C — same encoding as
    /// `temperature`; see doc/MATH.md §1). Used as the implicit baseline for the
    /// temperature quality penalty (compared against the project baseline rather
    /// than the global `quality_threshold_temp` max threshold). Falls back to the
    /// global `OracleConfig` default when unset (0).
    pub baseline_temp: i64,
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
    /// Lower bound (inclusive) of the acceptable pH band, ×100 (e.g. 600 = pH 6.00).
    pub quality_threshold_ph: i64,
    /// Upper bound (inclusive) of the acceptable pH band, ×100. Previously
    /// hardcoded as `quality_threshold_ph + 100`; now independently configurable
    /// so the band width isn't tied to a fixed offset that may not make physical
    /// sense for a given `quality_threshold_ph` (Issue: credit formula boundary hardening).
    pub quality_threshold_ph_max: i64,
    pub quality_threshold_turbidity: i64,
    pub quality_threshold_do: i64,
    pub quality_threshold_temp: i64,
    pub credit_per_kg_n: i128,
    pub credit_per_kg_p: i128,
    pub staking_token: Address,
    pub treasury: Address,
    pub min_stake: i128,
    pub unstake_cooldown_secs: u64,
    /// Seconds that must elapse after `open_window` before anyone can call
    /// `begin_reveal_phase` to transition Commit → Reveal.
    pub commit_phase_secs: u64,
    /// Minimum number of ledgers that must elapse after the reveal phase opens
    /// before a reveal is accepted. Guards against a reveal landing in the same
    /// ledger the phase transitioned, which would otherwise let a still-open
    /// mempool observer react within the same block.
    pub min_reveal_ledgers: u32,
    /// Maximum number of ledgers after the reveal phase opens during which a
    /// reveal is still accepted. A commitment not revealed within this window
    /// is forfeited: `finalize_window` penalizes the oracle and the window
    /// finalizes without its reading.
    pub max_reveal_ledgers: u32,
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
    /// Ledger sequence number at which `begin_reveal_phase` transitioned this
    /// window into `WindowPhase::Reveal`. `0` until that happens. Reveals are
    /// only valid within `[reveal_opened_ledger + min_reveal_ledgers, reveal_opened_ledger + max_reveal_ledgers]`.
    pub reveal_opened_ledger: u32,
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
    /// SHA-256 commitment for an oracle's pending reading in the current window.
    Commitment((BytesN<32>, Address)),
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
///
/// Byte layout (all integers big-endian, fixed-width, no padding — see
/// `doc/SPEC.md` §2.2 for the full commit-reveal writeup):
///
/// ```text
/// nonce(8B) || ph(8B) || turbidity(8B) || dissolved_oxygen(8B) || flow_rate(8B)
///   || temperature(8B) || total_nitrogen(8B) || total_phosphorus(8B) || salt(32B)
/// ```
///
/// `pub` so off-chain oracle nodes and integration tests can compute the exact
/// same commitment the contract will recompute on reveal, without
/// reimplementing the byte layout themselves.
pub fn sha256_commitment(
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

/// Compute the median of a `Vec<i64>`. Copies values into a local fixed-size
/// array (max 10 elements, matching `max_oracles`) and uses an insertion sort
/// on the stack — zero Soroban host allocations. For the hard config bound of
/// `max_oracles = 10` this runs at most 45 local comparisons, a dramatic
/// improvement over the previous O(n²) host-call-based insertion sort.
///
/// Even-length median: `(sorted[n/2-1] + sorted[n/2]) / 2` (Rust integer
/// division truncates toward zero, matching the historical behaviour).
fn median_i64(values: &Vec<i64>) -> i64 {
    let n = values.len() as usize;
    // `max_oracles = 10` is a hard config bound enforced at `add_oracle`,
    // so `n` is always in [1, 10].
    let mut arr = [0i64; 10];
    for (i, val) in values.iter().enumerate() {
        arr[i] = val;
    }
    // Insertion sort on the local stack array — zero host calls.
    for i in 1..n {
        let key = arr[i];
        let mut j = i;
        while j > 0 && arr[j - 1] > key {
            arr[j] = arr[j - 1];
            j -= 1;
        }
        arr[j] = key;
    }
    if n % 2 == 0 {
        (arr[n / 2 - 1] + arr[n / 2]) / 2
    } else {
        arr[n / 2]
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
            .has(&DataKey::Commitment((pid.clone(), oracle.clone())))
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

/// Reject sensor readings that are structurally valid `i64` values but physically
/// impossible, before they can enter median aggregation and the credit formula.
/// Without this, a negative or malformed reading (e.g. a malfunctioning sensor
/// reporting `total_nitrogen = -1`) would inflate `baseline - med_n` and mint
/// credits far beyond any physically plausible removal.
///
/// `pub` (rather than the on-chain `#[contractimpl]` surface) purely so the
/// integration-test crate can unit-test the panic paths directly: soroban's
/// native test-contract dispatch (`Env::register_contract` + client calls)
/// cannot catch a contract panic in this SDK/toolchain combination — it
/// aborts the whole test process instead of unwinding — so panic-path
/// coverage has to bypass that dispatch entirely and call the plain
/// function.
pub fn validate_sensor_reading(
    ph: i64,
    flow_rate: i64,
    total_nitrogen: i64,
    total_phosphorus: i64,
    dissolved_oxygen: i64,
) {
    if !(0..=1400).contains(&ph) {
        panic!("ph out of valid range [0, 1400]");
    }
    if flow_rate < 0 {
        panic!("flow_rate must be non-negative");
    }
    if total_nitrogen < 0 {
        panic!("total_nitrogen must be non-negative");
    }
    if total_phosphorus < 0 {
        panic!("total_phosphorus must be non-negative");
    }
    if dissolved_oxygen < 0 {
        panic!("dissolved_oxygen must be non-negative");
    }
}

/// Result of the credit finalization formula (nutrient removal, quality penalty,
/// volumetric credit, and the penalty-adjusted total).
pub struct FinalizationResult {
    pub n_removed: i128,
    pub p_removed: i128,
    pub penalty: i64,
    pub volumetric_credit: i128,
    pub total: i128,
}

/// Shared finalization arithmetic used by both `submit_reading_impl` and
/// `finalize_reveals`. Every multiplication uses `checked_mul` so that
/// near-`i64::MAX` intermediate values (e.g. `flow_rate` at the top of its
/// valid range) panic and revert the transaction instead of silently
/// wrapping in `i128`. `total` is floored at 0 so a maximal quality penalty
/// can never be misread as a negative credit balance.
///
/// `baseline_n` / `baseline_p` and `temp_threshold` are passed in rather than
/// read from config/project state here, since the two call sites use
/// different baseline sources (per-project baselines in the direct-submit
/// path vs. the global config in the commit-reveal path — see doc/MATH.md).
#[allow(clippy::too_many_arguments)]
pub fn compute_finalization(
    config: &OracleConfig,
    med_ph: i64,
    med_turb: i64,
    med_do: i64,
    med_temp: i64,
    med_flow: i64,
    med_n: i64,
    med_p: i64,
    baseline_n: i128,
    baseline_p: i128,
    temp_threshold: i128,
) -> FinalizationResult {
    let med_flow_i128 = med_flow as i128;
    let med_n_i128 = med_n as i128;
    let med_p_i128 = med_p as i128;

    let n_removed: i128 = if med_n_i128 < baseline_n {
        (baseline_n - med_n_i128)
            .checked_mul(med_flow_i128)
            .unwrap_or_else(|| panic!("n removal: flow multiplication overflow"))
            .checked_mul(3600)
            .unwrap_or_else(|| panic!("n removal: time-window multiplication overflow"))
            / 1_000_000
    } else {
        0
    };

    let p_removed: i128 = if med_p_i128 < baseline_p {
        (baseline_p - med_p_i128)
            .checked_mul(med_flow_i128)
            .unwrap_or_else(|| panic!("p removal: flow multiplication overflow"))
            .checked_mul(3600)
            .unwrap_or_else(|| panic!("p removal: time-window multiplication overflow"))
            / 1_000_000
    } else {
        0
    };

    // Quality penalty (basis points: 0-10000)
    let mut penalty: i64 = 0;
    if med_ph < config.quality_threshold_ph || med_ph > config.quality_threshold_ph_max {
        penalty += 2000;
    }
    if med_turb > config.quality_threshold_turbidity {
        penalty += 2000;
    }
    if med_do < config.quality_threshold_do {
        penalty += 2000;
    }
    if (med_temp as i128) > temp_threshold {
        penalty += 1000;
    }
    if penalty > 8000 {
        penalty = 8000;
    }

    // Volumetric credit based on flow
    let volumetric_credit: i128 = if med_flow > 0 {
        med_flow_i128
            .checked_mul(100)
            .unwrap_or_else(|| panic!("volumetric credit multiplication overflow"))
            / 1000
    } else {
        0
    };

    let n_credit: i128 = n_removed
        .checked_mul(config.credit_per_kg_n)
        .unwrap_or_else(|| panic!("n credit multiplication overflow"));
    let p_credit: i128 = p_removed
        .checked_mul(config.credit_per_kg_p)
        .unwrap_or_else(|| panic!("p credit multiplication overflow"));
    let gross: i128 = n_credit
        .checked_add(p_credit)
        .and_then(|s| s.checked_add(volumetric_credit))
        .unwrap_or_else(|| panic!("gross credit addition overflow"));

    // Apply quality penalty; `penalty` is capped at 8000 above so
    // `10000 - penalty` is always in [2000, 10000] and cannot underflow.
    let total: i128 = (gross
        .checked_mul(10000 - penalty as i128)
        .unwrap_or_else(|| panic!("total credit multiplication overflow"))
        / 10000)
        .max(0);

    FinalizationResult {
        n_removed,
        p_removed,
        penalty,
        volumetric_credit,
        total,
    }
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
            quality_threshold_ph_max: 700,
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
            min_reveal_ledgers: 0,
            max_reveal_ledgers: 60,
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

        validate_sensor_reading(
            ph,
            flow_rate,
            total_nitrogen,
            total_phosphorus,
            dissolved_oxygen,
        );

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

            let med_ph = median_i64(&ph_vals);
            let med_turb = median_i64(&turb_vals);
            let med_do = median_i64(&do_vals);
            let med_temp = median_i64(&temp_vals);
            let med_flow = median_i64(&flow_vals);
            let med_n = median_i64(&n_vals);
            let med_p = median_i64(&p_vals);

            // Per-project baselines (doc/MATH.md §1: N/P raw mg/L, temp ×10 °C).
            //
            // Scale note (Issue #26): `total_nitrogen` and `total_phosphorus` are
            // raw integers in mg/L (no ×100 scaling — see doc/MATH.md §1 table).
            // `temperature` is ×10 °C. The previous hardcoded `baseline_n = 10`
            // and `baseline_p = 2` were already in the same raw mg/L encoding,
            // so there is no scale mismatch. `baseline_temp = 300` matches the
            // ×10 °C encoding (30.0 °C).
            //
            // Fall back to the global defaults when a project has not set its
            // own baselines (ProjectConfig.baseline_* == 0). This preserves
            // backward compatibility: existing projects with old ProjectConfig
            // structs that predate these fields will deserialise with all-zero
            // baselines and behave identically to the old hardcoded constants.
            let proj_cfg = Self::get_project_config(e.clone(), project_id.clone());
            let default_baseline_n: i128 = 10;
            let default_baseline_p: i128 = 2;
            let default_baseline_temp: i128 = 300;
            let baseline_n: i128 = match proj_cfg {
                Some(ref pc) if pc.baseline_n != 0 => pc.baseline_n as i128,
                _ => default_baseline_n,
            };
            let baseline_p: i128 = match proj_cfg {
                Some(ref pc) if pc.baseline_p != 0 => pc.baseline_p as i128,
                _ => default_baseline_p,
            };
            let baseline_temp: i128 = match proj_cfg {
                Some(ref pc) if pc.baseline_temp != 0 => pc.baseline_temp as i128,
                _ => default_baseline_temp,
            };

            let fin = compute_finalization(
                &config,
                med_ph,
                med_turb,
                med_do,
                med_temp,
                med_flow,
                med_n,
                med_p,
                baseline_n,
                baseline_p,
                baseline_temp,
            );

            let mut result = VerificationResult {
                project_id: project_id.clone(),
                n_removal_kg: fin.n_removed,
                p_removal_kg: fin.p_removed,
                quality_penalty: fin.penalty,
                volumetric_credit: fin.volumetric_credit,
                total_credits: fin.total,
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
    ///
    /// `baseline_n` / `baseline_p` are per-project nutrient baselines in mg/L
    /// (raw integer, same encoding as `total_nitrogen` / `total_phosphorus`;
    /// see doc/MATH.md §1). `baseline_temp` is the per-project temperature
    /// baseline in ×10 °C (same encoding as `temperature`). Pass the protocol
    /// defaults (e.g. `10`, `2`, `300`) to preserve the previous behavior.
    pub fn set_project_config(
        e: Env,
        admin: Address,
        project_id: BytesN<32>,
        token_contract: Address,
        beneficiary: Address,
        baseline_n: i64,
        baseline_p: i64,
        baseline_temp: i64,
    ) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        let config = ProjectConfig {
            token_contract,
            beneficiary,
            baseline_n,
            baseline_p,
            baseline_temp,
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
                .remove(&DataKey::Commitment((project_id.clone(), oracle.clone())));
            e.storage()
                .temporary()
                .remove(&DataKey::OracleRevealed((project_id.clone(), oracle)));
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
            phase: WindowPhase::Commit,
            opened_at: e.ledger().timestamp(),
            reveal_opened_ledger: 0,
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
            reveal_opened_ledger: 0,
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

        let commit_key = DataKey::Commitment((project_id.clone(), oracle.clone()));
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
        window.reveal_opened_ledger = e.ledger().sequence();
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

        let current_ledger = e.ledger().sequence();
        if current_ledger < window.reveal_opened_ledger + config.min_reveal_ledgers {
            panic!("reveal submitted before the reveal window opened");
        }
        if current_ledger > window.reveal_opened_ledger + config.max_reveal_ledgers {
            panic!("reveal window has closed");
        }

        let commit_key = DataKey::Commitment((project_id.clone(), oracle.clone()));
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

        validate_sensor_reading(
            params.ph,
            params.flow_rate,
            params.total_nitrogen,
            params.total_phosphorus,
            params.dissolved_oxygen,
        );

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
        let current_ledger = e.ledger().sequence();
        if current_ledger <= window.reveal_opened_ledger + config.max_reveal_ledgers {
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
            let commit_key = DataKey::Commitment((project_id.clone(), oracle.clone()));
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

        let med_ph = median_i64(&ph_vals);
        let med_turb = median_i64(&turb_vals);
        let med_do = median_i64(&do_vals);
        let med_temp = median_i64(&temp_vals);
        let med_flow = median_i64(&flow_vals);
        let med_n = median_i64(&n_vals);
        let med_p = median_i64(&p_vals);

        let baseline_n: i128 = 10;
        let baseline_p: i128 = 2;
        let temp_threshold: i128 = config.quality_threshold_temp as i128;

        let fin = compute_finalization(
            &config,
            med_ph,
            med_turb,
            med_do,
            med_temp,
            med_flow,
            med_n,
            med_p,
            baseline_n,
            baseline_p,
            temp_threshold,
        );

        let mut result = VerificationResult {
            project_id: project_id.clone(),
            n_removal_kg: fin.n_removed,
            p_removal_kg: fin.p_removed,
            quality_penalty: fin.penalty,
            volumetric_credit: fin.volumetric_credit,
            total_credits: fin.total,
            credits_minted: 0,
            oracle_count: window.submissions.len(),
            finalized_at: e.ledger().timestamp(),
        };

        // Mint credits to the beneficiary, clamped to the token's max_supply cap
        // (see Issue #36). Runs before the result is persisted so
        // `credits_minted` is recorded accurately.
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
            e.storage().temporary().remove(&DataKey::Commitment((
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

    // ── Commit-Reveal Test Helpers ──
    //
    // All submissions now go through commit_reading + reveal_reading (the old
    // single-call submit_reading was removed as part of Issue #33 — it was the
    // plaintext bypass that made commit-reveal pointless). These helpers keep the
    // many aggregation/math/staking tests below readable.

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

    /// Commit+reveal one round for `oracles.len()` oracles on an already-open
    /// Commit-phase window, each submitting the reading at the matching index in
    /// `readings`. Waits out the default commit phase, transitions to reveal, and
    /// reveals in order. Returns the last reveal's result (`Some` only if it
    /// crossed `min_oracles`).
    fn commit_reveal_round_no_open(
        e: &Env,
        client: &VerificationOracleClient<'static>,
        project_id: &BytesN<32>,
        oracles: &Vec<Address>,
        nonce: u64,
        readings: &[(i64, i64, i64, i64, i64, i64, i64)],
        salt: &BytesN<32>,
    ) -> Option<VerificationResult> {
        for i in 0..oracles.len() {
            let o = oracles.get(i).unwrap();
            let r = readings[i as usize];
            let commitment = sha256_commitment(e, nonce, r.0, r.1, r.2, r.3, r.4, r.5, r.6, salt);
            client.commit_reading(&o, project_id, &nonce, &commitment);
        }
        set_ledger_timestamp(e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(project_id);
        let mut last = None;
        for i in 0..oracles.len() {
            let o = oracles.get(i).unwrap();
            let r = readings[i as usize];
            let params = make_reveal_params(e, nonce, r.0, r.1, r.2, r.3, r.4, r.5, r.6, salt);
            last = client.reveal_reading(&o, project_id, &params);
        }
        last
    }

    /// Opens a fresh window, then runs `commit_reveal_round_no_open`.
    fn commit_reveal_round(
        e: &Env,
        admin: &Address,
        client: &VerificationOracleClient<'static>,
        project_id: &BytesN<32>,
        oracles: &Vec<Address>,
        nonce: u64,
        readings: &[(i64, i64, i64, i64, i64, i64, i64)],
        salt: &BytesN<32>,
    ) -> Option<VerificationResult> {
        client.open_window(admin, project_id);
        commit_reveal_round_no_open(e, client, project_id, oracles, nonce, readings, salt)
    }

    /// Same-reading convenience wrapper around `commit_reveal_round`.
    fn commit_reveal_round_same(
        e: &Env,
        admin: &Address,
        client: &VerificationOracleClient<'static>,
        project_id: &BytesN<32>,
        oracles: &Vec<Address>,
        nonce: u64,
        reading: (i64, i64, i64, i64, i64, i64, i64),
        salt: &BytesN<32>,
    ) -> Option<VerificationResult> {
        let readings = std::vec![reading; oracles.len() as usize];
        commit_reveal_round(e, admin, client, project_id, oracles, nonce, &readings, salt)
    }

    /// Same-reading convenience wrapper around `commit_reveal_round_no_open`, for
    /// use after `reset_window` (which already reopens the window in the Commit
    /// phase, so calling `open_window` again would panic).
    fn commit_reveal_round_same_no_open(
        e: &Env,
        client: &VerificationOracleClient<'static>,
        project_id: &BytesN<32>,
        oracles: &Vec<Address>,
        nonce: u64,
        reading: (i64, i64, i64, i64, i64, i64, i64),
        salt: &BytesN<32>,
    ) -> Option<VerificationResult> {
        let readings = std::vec![reading; oracles.len() as usize];
        commit_reveal_round_no_open(e, client, project_id, oracles, nonce, &readings, salt)
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
        assert_eq!(config.min_reveal_ledgers, 0);
        assert_eq!(config.max_reveal_ledgers, 60);
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
        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 1, 1500);

        let project_id = BytesN::from_array(&e, &[1u8; 32]);
        let salt = BytesN::from_array(&e, &[0x01u8; 32]);
        let result = commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );
        assert!(result.is_none()); // only 1 of min_oracles=3 revealed
    }

    #[test]
    fn test_multi_oracle_aggregation_triggers_finalization() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[2u8; 32]);
        let salt = BytesN::from_array(&e, &[0x02u8; 32]);

        let readings = [
            (700i64, 10i64, 80i64, 500i64, 250i64, 8i64, 1i64),
            (710, 12, 75, 480, 260, 9, 1),
            (690, 11, 78, 510, 245, 7, 1),
        ];
        let result =
            commit_reveal_round(&e, &admin, &client, &project_id, &oracles, 1, &readings, &salt);

        assert!(result.is_some());
        let res = result.unwrap();
        assert!(res.total_credits > 0);
        assert_eq!(res.oracle_count, 3);
    }

    #[test]
    fn test_finalized_window_has_result() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[3u8; 32]);
        let salt = BytesN::from_array(&e, &[0x03u8; 32]);
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

        let result = client.get_last_result(&project_id);
        assert!(result.is_some());
        assert_eq!(result.unwrap().oracle_count, 3);
    }

    #[test]
    fn test_get_last_result_after_finalization() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[4u8; 32]);
        let salt = BytesN::from_array(&e, &[0x04u8; 32]);
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

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

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[50u8; 32]);
        let salt = BytesN::from_array(&e, &[0x50u8; 32]);

        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

        let history = client.get_result_history(&project_id, &0, &10);
        assert_eq!(history.len(), 1);

        client.reset_window(&admin, &project_id);
        commit_reveal_round_same_no_open(
            &e,
            &client,
            &project_id,
            &oracles,
            2,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

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
            max_oracles: 10,
            quality_threshold_ph: 550,
            quality_threshold_ph_max: 650,
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
            min_reveal_ledgers: 0,
            max_reveal_ledgers: 120,
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

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[6u8; 32]);
        let salt = BytesN::from_array(&e, &[0x06u8; 32]);
        let result = commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (700, 10, 80, 500, 250, 15, 5),
            &salt,
        );

        assert!(result.is_some());
        let res = result.unwrap();
        assert_eq!(res.n_removal_kg, 0);
        assert_eq!(res.p_removal_kg, 0);
    }

    #[test]
    fn test_penalty_boundaries() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[7u8; 32]);
        let salt = BytesN::from_array(&e, &[0x07u8; 32]);
        let result = commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (300, 200, 10, 500, 350, 8, 1),
            &salt,
        );

        assert!(result.is_some());
        assert_eq!(result.unwrap().quality_penalty, 7000);
    }

    #[test]
    fn test_oracle_submit_count_increments() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let o1 = oracles.get(0).unwrap();
        let o2 = oracles.get(1).unwrap();
        let o3 = oracles.get(2).unwrap();
        let salt = BytesN::from_array(&e, &[0x10u8; 32]);

        assert_eq!(client.oracle_submit_count(&o1), 0);
        assert_eq!(client.total_submissions(), 0);

        let project_id = BytesN::from_array(&e, &[10u8; 32]);
        let mut single = Vec::new(&e);
        single.push_back(o1.clone());
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &single,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );
        assert_eq!(client.oracle_submit_count(&o1), 1);
        assert_eq!(client.total_submissions(), 1);

        let project_id2 = BytesN::from_array(&e, &[11u8; 32]);
        let mut pair = Vec::new(&e);
        pair.push_back(o2.clone());
        pair.push_back(o3.clone());
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id2,
            &pair,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );
        assert_eq!(client.oracle_submit_count(&o2), 1);
        assert_eq!(client.oracle_submit_count(&o3), 1);
        assert_eq!(client.total_submissions(), 3);
    }

    #[test]
    fn test_nonce_independent_across_projects() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        // A single oracle finalizing per round requires min_oracles=1 so each
        // round completes and a fresh window can be opened for the next nonce.
        let mut config = client.get_config();
        config.min_oracles = 1;
        client.update_config(&admin, &config);

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 1, 1500);
        let salt = BytesN::from_array(&e, &[0x60u8; 32]);

        let p1 = BytesN::from_array(&e, &[50u8; 32]);
        let p2 = BytesN::from_array(&e, &[51u8; 32]);
        let p3 = BytesN::from_array(&e, &[52u8; 32]);

        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &p1,
            &oracles,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &p2,
            &oracles,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &p3,
            &oracles,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

        // nonce 2 must be accepted for p1 and p2 (new windows after finalization).
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &p1,
            &oracles,
            2,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &p2,
            &oracles,
            2,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );
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

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[30u8; 32]);
        let salt = BytesN::from_array(&e, &[0x30u8; 32]);

        client.open_window(&admin, &project_id);
        let commitment = sha256_commitment(&e, 1, 700, 10, 80, 500, 250, 8, 1, &salt);
        for i in 0..3u32 {
            let o = oracles.get(i).unwrap();
            client.commit_reading(&o, &project_id, &1, &commitment);
        }
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);

        // Only 2 of 3 committed oracles reveal.
        let params = make_reveal_params(&e, 1, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.reveal_reading(&oracles.get(0).unwrap(), &project_id, &params);
        client.reveal_reading(&oracles.get(1).unwrap(), &project_id, &params);
        assert_eq!(client.window_submission_count(&project_id), 2);

        client.reset_window(&admin, &project_id);
        assert_eq!(client.window_submission_count(&project_id), 0);
    }

    #[test]
    fn test_oracles_can_resubmit_after_reset() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[31u8; 32]);
        let salt = BytesN::from_array(&e, &[0x31u8; 32]);

        // Round 1: o1 and o2 commit+reveal with nonce 1; o3 sits out.
        client.open_window(&admin, &project_id);
        let commitment1 = sha256_commitment(&e, 1, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&oracles.get(0).unwrap(), &project_id, &1, &commitment1);
        client.commit_reading(&oracles.get(1).unwrap(), &project_id, &1, &commitment1);
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);
        let params1 = make_reveal_params(&e, 1, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.reveal_reading(&oracles.get(0).unwrap(), &project_id, &params1);
        client.reveal_reading(&oracles.get(1).unwrap(), &project_id, &params1);

        client.reset_window(&admin, &project_id);

        // Round 2: o1 and o2 resubmit with nonce 2; o3 submits for the first time
        // with nonce 1 (its nonce is independent of o1/o2's).
        let commitment2 = sha256_commitment(&e, 2, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&oracles.get(0).unwrap(), &project_id, &2, &commitment2);
        client.commit_reading(&oracles.get(1).unwrap(), &project_id, &2, &commitment2);
        client.commit_reading(&oracles.get(2).unwrap(), &project_id, &1, &commitment1);
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);
        let params2 = make_reveal_params(&e, 2, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.reveal_reading(&oracles.get(0).unwrap(), &project_id, &params2);
        client.reveal_reading(&oracles.get(1).unwrap(), &project_id, &params2);
        let result = client.reveal_reading(&oracles.get(2).unwrap(), &project_id, &params1);

        assert!(result.is_some());
        assert_eq!(result.unwrap().oracle_count, 3);
    }

    #[test]
    fn test_zero_flow_produces_zero_volumetric_credit() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[40u8; 32]);
        let salt = BytesN::from_array(&e, &[0x40u8; 32]);
        let result = commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (700, 10, 80, 0, 250, 2, 0),
            &salt,
        );

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

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[41u8; 32]);
        let salt = BytesN::from_array(&e, &[0x41u8; 32]);

        let mut single = Vec::new(&e);
        single.push_back(oracles.get(0).unwrap());
        let result = commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &single,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

        assert!(result.is_none());
        assert!(client.get_last_result(&project_id).is_none());
    }

    #[test]
    fn test_two_oracle_submissions_does_not_finalize() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[42u8; 32]);
        let salt = BytesN::from_array(&e, &[0x42u8; 32]);

        let mut pair = Vec::new(&e);
        pair.push_back(oracles.get(0).unwrap());
        pair.push_back(oracles.get(1).unwrap());
        let result = commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &pair,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

        assert!(result.is_none());
        assert!(client.get_last_result(&project_id).is_none());
    }

    #[test]
    fn test_all_zero_readings_no_credits_no_removal() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[43u8; 32]);
        let salt = BytesN::from_array(&e, &[0x43u8; 32]);
        let result = commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (300, 200, 10, 0, 350, 20, 5),
            &salt,
        );

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

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 2, 1500);
        let project_id = BytesN::from_array(&e, &[44u8; 32]);
        let salt = BytesN::from_array(&e, &[0x44u8; 32]);
        let readings = [
            (700i64, 10i64, 80i64, 400i64, 250i64, 8i64, 1i64),
            (700, 10, 80, 600, 250, 8, 1),
        ];
        let result =
            commit_reveal_round(&e, &admin, &client, &project_id, &oracles, 1, &readings, &salt);

        assert!(result.is_some());
        let res = result.unwrap();
        assert_eq!(res.volumetric_credit, 50);
    }

    // ── Staking & Slashing Tests ──

    #[test]
    fn test_stake_increases_balance() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &5000);
        let info = client.get_stake(&oracle);
        assert_eq!(info.amount, 5000);
        assert!(info.unstake_request.is_none());
    }

    #[test]
    fn test_stake_accumulates() {
        let (e, admin, client) = setup_with_client();
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

        let result = e.try_invoke_contract::<_, ()>(
            &client.address,
            &Symbol::new(&e, "stake"),
            vec![&e, oracle.to_val(), 0i128.into_val(&e)],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_unstake_reduces_balance() {
        let (e, admin, client) = setup_with_client();
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
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &1000);
        let result = e.try_invoke_contract::<_, ()>(
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
        let result = e.try_invoke_contract::<_, ()>(
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
        let (e, admin, client) = setup_with_client();
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
        let result = e.try_invoke_contract::<_, ()>(
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
        let result = e.try_invoke_contract::<_, ()>(
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
        let result = e.try_invoke_contract::<_, ()>(
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
        let result = e.try_invoke_contract::<_, ()>(
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

    // Note: the "insufficient stake blocks participation" case is covered by
    // `test_commit_requires_min_stake` below, against `commit_reading` (the
    // only entry point into a submission round now that `submit_reading` is
    // gone).

    #[test]
    fn test_commit_reading_with_sufficient_stake() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &2000);
        client.add_oracle(&admin, &oracle);

        let project_id = BytesN::from_array(&e, &[1u8; 32]);
        client.open_window(&admin, &project_id);
        let salt = BytesN::from_array(&e, &[0x09u8; 32]);
        let commitment = sha256_commitment(&e, 1, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&oracle, &project_id, &1, &commitment);
    }

    #[test]
    fn test_claim_unstake_before_cooldown_panics() {
        let (e, _admin, client) = setup_with_client();
        e.mock_all_auths();
        let oracle = Address::generate(&e);

        client.stake(&oracle, &5000);
        client.unstake(&oracle, &2000);

        let result = e.try_invoke_contract::<_, ()>(
            &client.address,
            &Symbol::new(&e, "claim_unstake"),
            vec![&e, oracle.to_val()],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_getters_return_config_values() {
        let (e, _admin, client) = setup_with_client();

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
        let result = e.try_invoke_contract::<_, Option<VerificationResult>>(
            &client.address,
            &Symbol::new(&e, "reveal_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                wrong_params.to_val(),
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
        let result = e.try_invoke_contract::<_, Option<VerificationResult>>(
            &client.address,
            &Symbol::new(&e, "reveal_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                params.to_val(),
            ],
        );
        assert!(result.is_err());
    }

    /// Unlike `test_late_reveal_after_phase_ends_panics` (which never leaves the
    /// Commit phase), this test actually enters the Reveal phase and then lets
    /// `max_reveal_ledgers` elapse *without* anyone calling `finalize_window`.
    /// `reveal_reading` itself must reject the reveal — the ledger-window check
    /// cannot depend on `finalize_window` having already run.
    #[test]
    fn test_reveal_rejected_after_max_reveal_ledgers() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[103u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0xDEu8; 32]);
        let nonce: u64 = 1;
        let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&oracles.get(0).unwrap(), &project_id, &nonce, &commitment);

        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);

        // Advance well past max_reveal_ledgers (default 60) without revealing.
        set_ledger_timestamp(&e, e.ledger().timestamp() + 400);

        let params = make_reveal_params(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        let result = e.try_invoke_contract::<_, Option<VerificationResult>>(
            &client.address,
            &Symbol::new(&e, "reveal_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                params.to_val(),
            ],
        );
        assert!(
            result.is_err(),
            "reveal past max_reveal_ledgers must be rejected even before finalize_window runs"
        );
    }

    /// `min_reveal_ledgers` guards against a reveal landing in the very same
    /// ledger the phase transitioned to Reveal.
    #[test]
    fn test_reveal_rejected_before_min_reveal_ledgers() {
        let (e, admin, client) = setup_with_client();
        e.mock_all_auths();

        let mut config = client.get_config();
        config.min_reveal_ledgers = 5;
        client.update_config(&admin, &config);

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[104u8; 32]);
        client.open_window(&admin, &project_id);

        let salt = BytesN::from_array(&e, &[0xDFu8; 32]);
        let nonce: u64 = 1;
        let commitment = sha256_commitment(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        client.commit_reading(&oracles.get(0).unwrap(), &project_id, &nonce, &commitment);

        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);

        // Reveal immediately: no ledgers have elapsed since the reveal phase
        // opened, so this must be rejected as too early.
        let params = make_reveal_params(&e, nonce, 700, 10, 80, 500, 250, 8, 1, &salt);
        let result = e.try_invoke_contract::<_, Option<VerificationResult>>(
            &client.address,
            &Symbol::new(&e, "reveal_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                params.to_val(),
            ],
        );
        assert!(result.is_err());

        // Once enough ledgers elapse, the identical reveal succeeds.
        set_ledger_timestamp(&e, e.ledger().timestamp() + 30);
        client.reveal_reading(&oracles.get(0).unwrap(), &project_id, &params);
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

        let result = e.try_invoke_contract::<_, ()>(
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

        let result = e.try_invoke_contract::<_, ()>(
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
        let result = e.try_invoke_contract::<_, ()>(
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
        let result = e.try_invoke_contract::<_, ()>(
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
        let result = e.try_invoke_contract::<_, Option<VerificationResult>>(
            &client.address,
            &Symbol::new(&e, "reveal_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                params.to_val(),
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
        let result = e.try_invoke_contract::<_, ()>(
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
        let result = e.try_invoke_contract::<_, Option<VerificationResult>>(
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
        let result = e.try_invoke_contract::<_, Option<VerificationResult>>(
            &client.address,
            &Symbol::new(&e, "reveal_reading"),
            vec![
                &e,
                oracles.get(0).unwrap().to_val(),
                project_id.to_val(),
                params.to_val(),
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
        let result = e.try_invoke_contract::<_, ()>(
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

        // Reset should work on a commit-phase window and clear the pending
        // commitment, even though the oracle never revealed.
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
        let result = e.try_invoke_contract::<_, ()>(
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

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[200u8; 32]);
        let salt = BytesN::from_array(&e, &[0xC8u8; 32]);

        // Readings that produce zero credits:
        //   flow_rate = 0  → volumetric_credit = 0
        //   total_nitrogen = 15 (≥ baseline 10) → n_removal = 0
        //   total_phosphorus = 3 (≥ baseline 2)  → p_removal = 0
        //   Poor quality (ph=300, turb=200, do=10) → large penalty, but gross is
        //   already 0 so total stays 0.
        client.open_window(&admin, &project_id);
        let commitment = sha256_commitment(&e, 1, 300, 200, 10, 0, 350, 15, 3, &salt);
        for i in 0..3u32 {
            let o = oracles.get(i).unwrap();
            client.commit_reading(&o, &project_id, &1, &commitment);
        }
        set_ledger_timestamp(&e, e.ledger().timestamp() + 301);
        client.begin_reveal_phase(&project_id);
        let params = make_reveal_params(&e, 1, 300, 200, 10, 0, 350, 15, 3, &salt);

        let result1 = client.reveal_reading(&oracles.get(0).unwrap(), &project_id, &params);
        assert!(
            result1.is_none(),
            "window should not finalize after 1 oracle"
        );

        let result2 = client.reveal_reading(&oracles.get(1).unwrap(), &project_id, &params);
        assert!(
            result2.is_none(),
            "window should not finalize after 2 oracles"
        );

        let result3 = client.reveal_reading(&oracles.get(2).unwrap(), &project_id, &params);

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
        // A successful three-oracle round with nonce 2 proves all nonces advanced.
        client.reset_window(&admin, &project_id);

        let next_result = commit_reveal_round_same_no_open(
            &e,
            &client,
            &project_id,
            &oracles,
            2,
            (300, 200, 10, 0, 350, 15, 3),
            &salt,
        );

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

        let oracles = setup_oracles_with_stakes(&e, &admin, &client, 3, 1500);
        let project_id = BytesN::from_array(&e, &[201u8; 32]);
        let salt = BytesN::from_array(&e, &[0xC9u8; 32]);

        // ── Window 1: zero credits ──
        let zero_result = commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (300, 200, 10, 0, 350, 15, 3),
            &salt,
        );
        assert!(zero_result.is_some());
        assert_eq!(zero_result.unwrap().total_credits, 0);

        // ── Window 2: positive credits ──
        // reset_window is required to open a new round after the previous one
        // was finalized.
        client.reset_window(&admin, &project_id);

        // Good readings: good pH (700=7.0), low turbidity (10), high DO (80),
        // positive flow (500), low temperature (250), N below baseline (8 < 10),
        // P below baseline (1 < 2).
        let positive_result = commit_reveal_round_same_no_open(
            &e,
            &client,
            &project_id,
            &oracles,
            2,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );
        assert!(positive_result.is_some(), "second window must finalize");

        let res = positive_result.unwrap();
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
        let mut only_o1 = Vec::new(&e);
        only_o1.push_back(o1.clone());
        let salt = BytesN::from_array(&e, &[0xD0u8; 32]);
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &only_o1,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

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
        let mut oracles = Vec::new(&e);
        oracles.push_back(o1.clone());
        oracles.push_back(o2.clone());
        oracles.push_back(o3.clone());
        let salt = BytesN::from_array(&e, &[0xD1u8; 32]);
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

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
        let mut oracles = Vec::new(&e);
        oracles.push_back(o1.clone());
        oracles.push_back(o2.clone());
        oracles.push_back(o3.clone());
        let salt = BytesN::from_array(&e, &[0xD2u8; 32]);
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

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
        let mut oracles = Vec::new(&e);
        oracles.push_back(o1.clone());
        oracles.push_back(o2.clone());
        oracles.push_back(o3.clone());
        let salt = BytesN::from_array(&e, &[0xD3u8; 32]);
        commit_reveal_round_same(
            &e,
            &admin,
            &client,
            &project_id,
            &oracles,
            1,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

        let first = client.get_last_result(&project_id);
        assert!(first.is_some());
        assert_eq!(first.unwrap().oracle_count, 3);

        client.remove_oracle(&admin, &o4);
        assert!(!client.is_oracle_active(&o4));

        client.reset_window(&admin, &project_id);
        let result = commit_reveal_round_same_no_open(
            &e,
            &client,
            &project_id,
            &oracles,
            2,
            (700, 10, 80, 500, 250, 8, 1),
            &salt,
        );

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

    // ── median_i64 unit tests ──

    /// Build a `Vec<i64>` of length `n` from a Rust slice. Requires `Env`
    /// for Soroban `Vec` construction and `#[cfg(test)]` (`extern crate std`).
    fn make_i64_vec(e: &Env, values: &[i64]) -> Vec<i64> {
        let mut v = Vec::new(e);
        for val in values {
            v.push_back(*val);
        }
        v
    }

    #[test]
    fn test_median_odd_count() {
        let e = Env::default();
        let v = make_i64_vec(&e, &[30, 10, 20]);
        assert_eq!(median_i64(&v), 20);
    }

    #[test]
    fn test_median_odd_count_five() {
        let e = Env::default();
        let v = make_i64_vec(&e, &[50, 10, 30, 40, 20]);
        assert_eq!(median_i64(&v), 30);
    }

    #[test]
    fn test_median_even_count_averages_two_middles() {
        let e = Env::default();
        // [10, 20, 30, 40] → middles are 20 and 30 → (20+30)/2 = 25
        let v = make_i64_vec(&e, &[40, 10, 30, 20]);
        assert_eq!(median_i64(&v), 25);
    }

    #[test]
    fn test_median_even_count_truncates_toward_zero() {
        let e = Env::default();
        // [11, 20] → (11+20)/2 = 15 (truncates toward zero)
        let v = make_i64_vec(&e, &[11, 20]);
        assert_eq!(median_i64(&v), 15);
    }

    #[test]
    fn test_median_with_negative_values_odd() {
        let e = Env::default();
        // [-50, -10, -30] → sorted: [-50, -30, -10] → median = -30
        let v = make_i64_vec(&e, &[-10, -50, -30]);
        assert_eq!(median_i64(&v), -30);
    }

    #[test]
    fn test_median_with_negative_values_even() {
        let e = Env::default();
        // [-2, -1] → (-2 + -1)/2 = -3/2 = -1 (truncates toward zero)
        let v = make_i64_vec(&e, &[-2, -1]);
        assert_eq!(median_i64(&v), -1);
    }

    #[test]
    fn test_median_mixed_signs_even() {
        let e = Env::default();
        // [-5, 5] → (5 + -5)/2 = 0/2 = 0
        let v = make_i64_vec(&e, &[-5, 5]);
        assert_eq!(median_i64(&v), 0);
    }

    #[test]
    fn test_median_single_element() {
        let e = Env::default();
        let v = make_i64_vec(&e, &[42]);
        assert_eq!(median_i64(&v), 42);
    }

    #[test]
    fn test_median_ten_elements_max_oracles() {
        let e = Env::default();
        // 10 elements — max_oracles boundary. Sorted: [0,1,2,3,4,5,6,7,8,9]
        // even → (4 + 5)/2 = 4
        let v = make_i64_vec(&e, &[5, 3, 8, 1, 9, 7, 2, 6, 0, 4]);
        assert_eq!(median_i64(&v), 4);
    }

    #[test]
    fn test_median_all_same_values() {
        let e = Env::default();
        let v = make_i64_vec(&e, &[7, 7, 7, 7, 7]);
        assert_eq!(median_i64(&v), 7);
    }

    #[test]
    fn test_median_extreme_i64_values() {
        let e = Env::default();
        let v = make_i64_vec(&e, &[i64::MAX, i64::MIN, 0]);
        assert_eq!(median_i64(&v), 0);
    }
}
