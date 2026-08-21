#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, vec, Address, Env, InvokeError, String,
    Symbol, Val, Vec,
};

#[cfg(test)]
extern crate std;

const EVENT_PROPOSAL_CREATED: Symbol = symbol_short!("prop_crt");
const EVENT_PROPOSAL_EXECUTED: Symbol = symbol_short!("prop_exe");
const EVENT_VOTE_CAST: Symbol = symbol_short!("vote_cst");
const EVENT_MEMBER_ADDED: Symbol = symbol_short!("memb_add");
const EVENT_MEMBER_REMOVED: Symbol = symbol_short!("memb_rmv");
const EVENT_EMERGENCY_PAUSE: Symbol = symbol_short!("emrg_pse");
const EVENT_EMERGENCY_UNPAUSE: Symbol = symbol_short!("emrg_ups");
const EVENT_INITIALIZED: Symbol = symbol_short!("init");
const EVENT_ADMIN_TRANSFERRED: Symbol = symbol_short!("adm_xfer");

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalStatus {
    Pending,
    Active,
    Approved,
    Executed,
    Cancelled,
    Rejected,
    Expired,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct GovernanceConfig {
    pub fee_bps: u32,
    pub voting_period: u64,
    pub timelock_duration: u64,
    pub approval_threshold_bps: u32,
    /// Minimum participation required to resolve a proposal, expressed as a
    /// basis-point fraction of `Proposal.eligible_voters`. A proposal is only
    /// evaluated for approval/rejection once the number of cast votes reaches
    /// this quorum. This decouples resolution from 100% turnout and from
    /// retroactive membership changes.
    pub quorum_bps: u32,
    pub min_proposal_deposit: i128,
    pub max_active_proposals: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub id: u64,
    pub proposer: Address,
    pub title: String,
    pub description: String,
    pub actions: Vec<GovernanceAction>,
    /// When true, `execute()` dispatches each action in best-effort mode and
    /// records per-action success/failure in `action_results` instead of
    /// reverting the whole call. Defaults to false (atomic execution).
    pub allow_partial_execution: bool,
    /// Populated by `execute()` when `allow_partial_execution` is true. Each
    /// entry mirrors `actions[i]`: true if the action succeeded, false if it
    /// failed. Stays empty for atomic proposals.
    pub action_results: Vec<bool>,
    /// Number of "yes" votes. Stored as a count (not a voter list) for bounded
    /// storage and O(1) updates. Per-voter dedup is enforced via
    /// `DataKey::HasVoted`.
    pub votes_for: u32,
    /// Number of "no" votes. See `votes_for` for rationale.
    pub votes_against: u32,
    /// Number of eligible voters snapshotted at proposal creation time. Used as
    /// the stable denominator for both quorum and approval math so that
    /// members added/removed after creation cannot change the threshold
    /// retroactively.
    pub eligible_voters: u32,
    pub status: ProposalStatus,
    pub created_at: u64,
    pub voting_ends_at: u64,
    pub timelock_ends_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct GovernanceAction {
    pub target: Address,
    pub function: Symbol,
    pub args: Vec<Val>,
}

/// Built-in protocol action types. These are dispatched by `execute` and
/// `emergency_pause`/`emergency_unpause` without requiring a generic
/// cross-contract call encoding.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProtocolAction {
    /// Pause all registered credit token contracts.
    EmergencyPause,
    /// Unpause all registered credit token contracts.
    EmergencyUnpause,
}

#[contracttype]
pub struct VoteCounts {
    pub yes: u32,
    pub no: u32,
}

/// Storage key enum.
///
/// Instance:   Admin, Config, MemberCount, ProposalCount, ProtocolPaused,
///             RegisteredTokens, ActiveProposals
/// Persistent: Member(Address), Proposal(u64), HasVoted(u64, Address)
#[contracttype]
pub enum DataKey {
    // ── Instance ──
    Admin,
    Config,
    MemberCount,
    ProposalCount,
    ActiveProposals,
    ProtocolPaused,
    RegisteredTokens,
    // ── Persistent ──
    Member(Address),
    Proposal(u64),
    HasVoted(u64, Address),
}

// ── TTL constants ──
/// Members and proposals are long-lived: 2 years.
const MEMBER_TTL_THRESHOLD: u32 = 12_614_400;
const MEMBER_TTL_BUMP: u32 = 12_614_400;
const PROPOSAL_TTL_THRESHOLD: u32 = 12_614_400;
const PROPOSAL_TTL_BUMP: u32 = 12_614_400;
const VOTED_TTL_THRESHOLD: u32 = 12_614_400;
const VOTED_TTL_BUMP: u32 = 12_614_400;

/// Safe ranges for `GovernanceConfig` fields, enforced by `update_config`.
/// These are constants (not config-derived) to avoid a circular dependency
/// where an invalid config could loosen the bounds used to validate itself.
/// Minimum voting period (1 hour): shorter windows leave members no
/// realistic chance to vote before a proposal expires.
const MIN_VOTING_PERIOD: u64 = 3_600;
/// Maximum voting period (30 days).
const MAX_VOTING_PERIOD: u64 = 2_592_000;
/// Maximum timelock duration (7 days). `0` is a valid "no timelock".
const MAX_TIMELOCK_DURATION: u64 = 604_800;
/// Minimum approval threshold (0.01%). `0` would let any single vote (or no
/// votes) approve a proposal.
const MIN_APPROVAL_THRESHOLD_BPS: u32 = 1;
/// Maximum approval threshold (100%).
const MAX_APPROVAL_THRESHOLD_BPS: u32 = 10_000;
/// Maximum quorum (100%). `0` is a valid "no quorum requirement".
const MAX_QUORUM_BPS: u32 = 10_000;
/// Minimum active-proposal cap: at least one proposal must be allowed to be
/// in flight at a time.
const MIN_ACTIVE_PROPOSALS: u32 = 1;
/// Maximum active-proposal cap, to keep the active-proposals list bounded.
const MAX_ACTIVE_PROPOSALS: u32 = 100;

fn has_admin(e: &Env) -> bool {
    e.storage().instance().has(&DataKey::Admin)
}

fn read_admin(e: &Env) -> Address {
    e.storage().instance().get(&DataKey::Admin).unwrap()
}

fn read_config(e: &Env) -> GovernanceConfig {
    e.storage().instance().get(&DataKey::Config).unwrap()
}

/// Validate `GovernanceConfig` fields against the safe ranges above before
/// they're written to storage. An out-of-range value here governs every
/// future proposal, so a bad `update_config` call must be rejected outright
/// rather than silently degrading governance (e.g. `voting_period = 0`
/// expiring every proposal instantly, or `approval_threshold_bps = 0`
/// letting a single vote approve anything).
fn validate_config(config: &GovernanceConfig) {
    if !(MIN_VOTING_PERIOD..=MAX_VOTING_PERIOD).contains(&config.voting_period) {
        panic!("voting_period out of range");
    }
    if config.timelock_duration > MAX_TIMELOCK_DURATION {
        panic!("timelock_duration out of range");
    }
    if !(MIN_APPROVAL_THRESHOLD_BPS..=MAX_APPROVAL_THRESHOLD_BPS)
        .contains(&config.approval_threshold_bps)
    {
        panic!("approval_threshold_bps out of range");
    }
    if config.quorum_bps > MAX_QUORUM_BPS {
        panic!("quorum_bps out of range");
    }
    if config.min_proposal_deposit < 0 {
        panic!("min_proposal_deposit out of range");
    }
    if !(MIN_ACTIVE_PROPOSALS..=MAX_ACTIVE_PROPOSALS).contains(&config.max_active_proposals) {
        panic!("max_active_proposals out of range");
    }
}

fn is_member(e: &Env, addr: &Address) -> bool {
    e.storage()
        .persistent()
        .get(&DataKey::Member(addr.clone()))
        .unwrap_or(false)
}

fn member_count(e: &Env) -> u32 {
    e.storage().instance().get(&DataKey::MemberCount).unwrap()
}

#[contract]
pub struct Governance;

#[contractimpl]
impl Governance {
    /// Initialize the governance contract with an admin and initial member list. Callable once.
    pub fn initialize(e: Env, admin: Address, initial_members: Vec<Address>) {
        if has_admin(&e) {
            panic!("already initialized");
        }
        e.storage().instance().set(&DataKey::Admin, &admin);

        let config = GovernanceConfig {
            fee_bps: 50,
            voting_period: 604800,
            timelock_duration: 86400,
            approval_threshold_bps: 6000,
            quorum_bps: 5000,
            min_proposal_deposit: 1000,
            max_active_proposals: 10,
        };
        e.storage().instance().set(&DataKey::Config, &config);
        e.storage().instance().set(&DataKey::ProposalCount, &0u64);
        e.storage()
            .instance()
            .set(&DataKey::ActiveProposals, &Vec::<u64>::new(&e));

        let mut count: u32 = 0;
        for i in 0..initial_members.len() {
            let member = initial_members.get(i).unwrap();
            if !e
                .storage()
                .persistent()
                .has(&DataKey::Member(member.clone()))
            {
                e.storage()
                    .persistent()
                    .set(&DataKey::Member(member.clone()), &true);
                e.storage().persistent().extend_ttl(
                    &DataKey::Member(member.clone()),
                    MEMBER_TTL_THRESHOLD,
                    MEMBER_TTL_BUMP,
                );
                count += 1;
            }
        }
        e.storage().instance().set(&DataKey::MemberCount, &count);
        e.storage().instance().set(&DataKey::ProtocolPaused, &false);
        e.storage()
            .instance()
            .set(&DataKey::RegisteredTokens, &Vec::<Address>::new(&e));

        e.events().publish((EVENT_INITIALIZED,), (admin,));
    }

    /// Get the current governance configuration (fee, voting period, thresholds).
    pub fn get_config(e: Env) -> GovernanceConfig {
        read_config(&e)
    }

    /// Get a proposal by ID. Returns None if not found.
    pub fn get_proposal(e: Env, proposal_id: u64) -> Option<Proposal> {
        let key = DataKey::Proposal(proposal_id);
        let result: Option<Proposal> = e.storage().persistent().get(&key);
        if result.is_some() {
            e.storage()
                .persistent()
                .extend_ttl(&key, PROPOSAL_TTL_THRESHOLD, PROPOSAL_TTL_BUMP);
        }
        result
    }

    /// Create a new proposal. Only governance members can propose.
    ///
    /// `allow_partial_execution` opts the proposal into best-effort execution:
    /// when true, `execute()` records per-action success/failure in
    /// `Proposal.action_results` instead of reverting the whole call on the
    /// first failure. Defaults to false (atomic, all-or-nothing execution).
    /// Returns the auto-incremented proposal ID.
    pub fn propose(
        e: Env,
        proposer: Address,
        title: String,
        description: String,
        actions: Vec<GovernanceAction>,
        allow_partial_execution: bool,
    ) -> u64 {
        proposer.require_auth();

        if !is_member(&e, &proposer) {
            panic!("not a governance member");
        }

        let count: u64 = e.storage().instance().get(&DataKey::ProposalCount).unwrap();
        let proposal_id = count + 1;
        let timestamp = e.ledger().timestamp();
        let config: GovernanceConfig = read_config(&e);

        // Check active proposal limit
        let active: Vec<u64> = e
            .storage()
            .instance()
            .get(&DataKey::ActiveProposals)
            .unwrap();
        if active.len() >= config.max_active_proposals {
            panic!("too many active proposals");
        }

        if title.len() == 0 {
            panic!("title must not be empty");
        }

        let proposal = Proposal {
            id: proposal_id,
            proposer: proposer.clone(),
            title,
            description,
            actions,
            allow_partial_execution,
            action_results: Vec::new(&e),
            votes_for: 0,
            votes_against: 0,
            eligible_voters: member_count(&e),
            status: ProposalStatus::Pending,
            created_at: timestamp,
            voting_ends_at: timestamp + config.voting_period,
            timelock_ends_at: 0,
        };

        e.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);
        e.storage().persistent().extend_ttl(
            &DataKey::Proposal(proposal_id),
            PROPOSAL_TTL_THRESHOLD,
            PROPOSAL_TTL_BUMP,
        );

        let mut active = active;
        active.push_back(proposal_id);
        e.storage()
            .instance()
            .set(&DataKey::ActiveProposals, &active);
        e.storage()
            .instance()
            .set(&DataKey::ProposalCount, &proposal_id);

        e.events()
            .publish((EVENT_PROPOSAL_CREATED,), (proposal_id, proposer));

        proposal_id
    }

    /// Vote on a proposal. Members can vote once. Auto-activates pending proposals.
    ///
    /// A proposal is resolved (Approved/Rejected) once the number of cast votes
    /// reaches the quorum derived from `Proposal.eligible_voters` (snapshotted at
    /// creation), not when 100% turnout is achieved. Because the quorum
    /// denominator is frozen at creation time, adding or removing members after a
    /// proposal exists cannot change its threshold retroactively — this fixes the
    /// liveness bug where a proposal could get stuck in `Active` forever when a
    /// member was added after creation.
    ///
    /// Approval is measured as a fraction of *cast* votes once quorum (a minimum
    /// participation level of `eligible_voters`) is reached. The quorum gate
    /// ensures a proposal cannot resolve on a single vote from a tiny subset of
    /// the membership; see `GovernanceConfig::quorum_bps`.
    pub fn vote(e: Env, voter: Address, proposal_id: u64, approve: bool) {
        voter.require_auth();

        if !is_member(&e, &voter) {
            panic!("not a governance member");
        }

        let proposal_key = DataKey::Proposal(proposal_id);
        let mut proposal: Proposal = e
            .storage()
            .persistent()
            .get(&proposal_key)
            .unwrap_or_else(|| panic!("proposal not found"));

        let timestamp = e.ledger().timestamp();

        if matches!(proposal.status, ProposalStatus::Pending) {
            proposal.status = ProposalStatus::Active;
        }

        if !matches!(proposal.status, ProposalStatus::Active) {
            panic!("proposal already resolved");
        }

        let voted_key = DataKey::HasVoted(proposal_id, voter.clone());
        if e.storage().persistent().has(&voted_key) {
            panic!("already voted");
        }

        if timestamp > proposal.voting_ends_at {
            proposal.status = ProposalStatus::Expired;
            e.storage().persistent().set(&proposal_key, &proposal);
            e.storage().persistent().extend_ttl(
                &proposal_key,
                PROPOSAL_TTL_THRESHOLD,
                PROPOSAL_TTL_BUMP,
            );
            panic!("voting period ended");
        }

        if approve {
            proposal.votes_for += 1;
        } else {
            proposal.votes_against += 1;
        }

        e.storage().persistent().set(&voted_key, &true);
        e.storage()
            .persistent()
            .extend_ttl(&voted_key, VOTED_TTL_THRESHOLD, VOTED_TTL_BUMP);
        e.storage().persistent().set(&proposal_key, &proposal);
        e.storage().persistent().extend_ttl(
            &proposal_key,
            PROPOSAL_TTL_THRESHOLD,
            PROPOSAL_TTL_BUMP,
        );

        e.events()
            .publish((EVENT_VOTE_CAST,), (proposal_id, voter, approve));

        // Resolution math uses the creation-time snapshot (`eligible_voters`),
        // never the live membership count. This guarantees membership changes
        // after proposal creation never retroactively alter the threshold.
        let config: GovernanceConfig = read_config(&e);
        let total_votes = proposal.votes_for + proposal.votes_against;

        // Quorum = ceil(quorum_bps/10000 * eligible_voters), computed against the
        // snapshot. At least one vote is required to resolve.
        let quorum = if proposal.eligible_voters == 0 {
            0u64
        } else {
            (proposal.eligible_voters as u64 * config.quorum_bps as u64).div_ceil(10000)
        };

        if (total_votes as u64) >= quorum {
            // Approval is measured as a fraction of cast votes. The quorum gate
            // above already guarantees a minimum participation level, so a
            // proposal cannot resolve on a single vote from a tiny subset of the
            // membership.
            let yes_pct = if total_votes > 0 {
                (proposal.votes_for as u64 * 10000) / total_votes as u64
            } else {
                0
            };
            if yes_pct >= config.approval_threshold_bps as u64 {
                proposal.status = ProposalStatus::Approved;
                proposal.timelock_ends_at = timestamp + config.timelock_duration;
                e.storage().persistent().set(&proposal_key, &proposal);
                e.storage().persistent().extend_ttl(
                    &proposal_key,
                    PROPOSAL_TTL_THRESHOLD,
                    PROPOSAL_TTL_BUMP,
                );
            } else {
                proposal.status = ProposalStatus::Rejected;
                e.storage().persistent().set(&proposal_key, &proposal);
                e.storage().persistent().extend_ttl(
                    &proposal_key,
                    PROPOSAL_TTL_THRESHOLD,
                    PROPOSAL_TTL_BUMP,
                );
            }
        }
    }

    /// Execute an approved proposal after the timelock has elapsed. Members only.
    pub fn execute(e: Env, caller: Address, proposal_id: u64) {
        caller.require_auth();

        if !is_member(&e, &caller) {
            panic!("not a governance member");
        }

        let proposal_key = DataKey::Proposal(proposal_id);
        let mut proposal: Proposal = e
            .storage()
            .persistent()
            .get(&proposal_key)
            .unwrap_or_else(|| panic!("proposal not found"));

        if !matches!(proposal.status, ProposalStatus::Approved) {
            panic!("proposal not approved");
        }

        let timestamp = e.ledger().timestamp();
        if timestamp < proposal.timelock_ends_at {
            panic!("timelock not elapsed");
        }

        // Remove from the active list before dispatch. In atomic mode a
        // failing action reverts the whole transaction, which rolls this
        // removal back; in best-effort mode the removal persists so a
        // partially-executed proposal can never re-block the active slot.
        Self::remove_from_active(&e, proposal_id);

        // Dispatch proposal actions.
        //
        // Built-in protocol actions are identified by the `function` field:
        //   "emergency_pause"   → pause all registered token contracts
        //   "emergency_unpause" → unpause all registered token contracts
        //
        // All other actions are executed as generic cross-contract invocations
        // via `e.invoke_contract()`, using the target address, function symbol,
        // and arguments stored in the GovernanceAction.
        //
        // Error policy:
        //   * Atomic (allow_partial_execution == false) — REVERT: if any
        //     cross-contract invocation fails the entire `execute()` call is
        //     reverted and the proposal retains its `Approved` status for retry.
        //   * Best-effort (allow_partial_execution == true) — each action is
        //     dispatched via `try_invoke_contract`; failures are recorded in
        //     `action_results` without reverting the surrounding call.
        let best_effort = proposal.allow_partial_execution;
        let mut action_results: Vec<bool> = Vec::new(&e);

        for i in 0..proposal.actions.len() {
            let action = proposal.actions.get(i).unwrap();
            let ok = Self::dispatch_action(&e, &action, best_effort);
            if best_effort {
                action_results.push_back(ok);
            }
        }

        proposal.status = ProposalStatus::Executed;
        if best_effort {
            proposal.action_results = action_results;
            e.storage().persistent().set(&proposal_key, &proposal);
            e.storage().persistent().extend_ttl(
                &proposal_key,
                PROPOSAL_TTL_THRESHOLD,
                PROPOSAL_TTL_BUMP,
            );
            // `exec_partial` is longer than the 9-char `symbol_short!` limit,
            // so the topic is built at runtime.
            e.events().publish(
                (Symbol::new(&e, "exec_partial"),),
                (proposal_id, proposal.action_results.clone()),
            );
        } else {
            // Mark executed only after all actions succeed (revert-safe ordering).
            e.storage().persistent().set(&proposal_key, &proposal);
            e.storage().persistent().extend_ttl(
                &proposal_key,
                PROPOSAL_TTL_THRESHOLD,
                PROPOSAL_TTL_BUMP,
            );
            e.events()
                .publish((EVENT_PROPOSAL_EXECUTED,), (proposal_id,));
        }
    }

    /// Cancel an approved or active proposal. Admin only.
    ///
    /// This is the on-chain recovery path for a proposal whose actions can no
    /// longer succeed (for example a target contract was upgraded and its
    /// function signature changed, leaving an `Approved` proposal permanently
    /// un-executable). It transitions the proposal to the terminal `Cancelled`
    /// state and frees its active-proposals slot without requiring a full new
    /// voting cycle.
    ///
    /// Restricted to the admin so a member cannot use it to block a proposal
    /// they dislike. Already-resolved proposals (Executed/Rejected/Expired) are
    /// not cancellable.
    pub fn cancel_proposal(e: Env, admin: Address, proposal_id: u64) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }

        let proposal_key = DataKey::Proposal(proposal_id);
        let mut proposal: Proposal = e
            .storage()
            .persistent()
            .get(&proposal_key)
            .unwrap_or_else(|| panic!("proposal not found"));

        if !matches!(proposal.status, ProposalStatus::Approved)
            && !matches!(proposal.status, ProposalStatus::Active)
        {
            panic!("proposal not cancellable");
        }

        proposal.status = ProposalStatus::Cancelled;
        e.storage().persistent().set(&proposal_key, &proposal);
        e.storage().persistent().extend_ttl(
            &proposal_key,
            PROPOSAL_TTL_THRESHOLD,
            PROPOSAL_TTL_BUMP,
        );

        Self::remove_from_active(&e, proposal_id);

        // `prop_cancel` is longer than the 9-char `symbol_short!` limit, so
        // the topic is built at runtime.
        e.events()
            .publish((Symbol::new(&e, "prop_cancel"),), (proposal_id,));
    }

    /// Update the governance configuration parameters. Admin only.
    pub fn update_config(e: Env, admin: Address, config: GovernanceConfig) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        validate_config(&config);
        e.storage().instance().set(&DataKey::Config, &config);
    }

    /// Transfer admin rights to a new address. Admin only.
    pub fn transfer_admin(e: Env, admin: Address, new_admin: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Admin, &new_admin);

        e.events()
            .publish((EVENT_ADMIN_TRANSFERRED,), (stored, new_admin));
    }

    /// Add a new governance member. Admin only.
    pub fn add_member(e: Env, admin: Address, new_member: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        if e.storage()
            .persistent()
            .has(&DataKey::Member(new_member.clone()))
        {
            panic!("already a member");
        }
        e.storage()
            .persistent()
            .set(&DataKey::Member(new_member.clone()), &true);
        e.storage().persistent().extend_ttl(
            &DataKey::Member(new_member.clone()),
            MEMBER_TTL_THRESHOLD,
            MEMBER_TTL_BUMP,
        );
        let count: u32 = e.storage().instance().get(&DataKey::MemberCount).unwrap();
        e.storage()
            .instance()
            .set(&DataKey::MemberCount, &(count + 1));

        e.events().publish((EVENT_MEMBER_ADDED,), (new_member,));
    }

    /// Remove a governance member. Cannot remove the last member. Admin only.
    pub fn remove_member(e: Env, admin: Address, member: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        if !e
            .storage()
            .persistent()
            .has(&DataKey::Member(member.clone()))
        {
            panic!("not a member");
        }
        let count: u32 = e.storage().instance().get(&DataKey::MemberCount).unwrap();
        if count <= 1 {
            panic!("cannot remove last member");
        }
        e.storage()
            .persistent()
            .remove(&DataKey::Member(member.clone()));
        e.storage()
            .instance()
            .set(&DataKey::MemberCount, &(count - 1));

        e.events().publish((EVENT_MEMBER_REMOVED,), (member,));
    }

    /// Check if an address is a governance member.
    pub fn is_member_fn(e: Env, addr: Address) -> bool {
        is_member(&e, &addr)
    }

    /// Get the total number of governance members.
    pub fn member_count_fn(e: Env) -> u32 {
        member_count(&e)
    }

    // ── Token Registry ──

    /// Register a credit token contract address so it can be paused/unpaused
    /// by governance during an emergency. Admin only.
    pub fn register_token(e: Env, admin: Address, token: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        let mut tokens: Vec<Address> = e
            .storage()
            .instance()
            .get(&DataKey::RegisteredTokens)
            .unwrap_or_else(|| Vec::new(&e));
        // Idempotent: only add if not already present.
        for i in 0..tokens.len() {
            if tokens.get(i).unwrap() == token {
                return;
            }
        }
        tokens.push_back(token);
        e.storage()
            .instance()
            .set(&DataKey::RegisteredTokens, &tokens);
    }

    /// Remove a credit token contract from the governance registry. Admin only.
    pub fn deregister_token(e: Env, admin: Address, token: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        let tokens: Vec<Address> = e
            .storage()
            .instance()
            .get(&DataKey::RegisteredTokens)
            .unwrap_or_else(|| Vec::new(&e));
        let mut filtered: Vec<Address> = Vec::new(&e);
        for i in 0..tokens.len() {
            let addr = tokens.get(i).unwrap();
            if addr != token {
                filtered.push_back(addr);
            }
        }
        e.storage()
            .instance()
            .set(&DataKey::RegisteredTokens, &filtered);
    }

    /// Return the list of all registered credit token contract addresses.
    pub fn list_registered_tokens(e: Env) -> Vec<Address> {
        e.storage()
            .instance()
            .get(&DataKey::RegisteredTokens)
            .unwrap_or_else(|| Vec::new(&e))
    }

    // ── Emergency Pause ──

    /// Returns true when the protocol is in emergency-pause state.
    pub fn is_protocol_paused(e: Env) -> bool {
        e.storage()
            .instance()
            .get(&DataKey::ProtocolPaused)
            .unwrap_or(false)
    }

    /// Emergency pause: immediately calls `pause(governance_contract)` on every
    /// registered credit token contract, then records the paused state.
    ///
    /// Authorization: admin only.
    /// For a governance-proposal-triggered pause use `emergency_pause_via_proposal`.
    pub fn emergency_pause(e: Env, admin: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        Self::do_pause(&e);
    }

    /// Emergency unpause: calls `unpause(governance_contract)` on every registered
    /// credit token contract and clears the paused state.
    ///
    /// Authorization: admin only.
    pub fn emergency_unpause(e: Env, admin: Address) {
        admin.require_auth();
        let stored: Address = read_admin(&e);
        if admin != stored {
            panic!("unauthorized");
        }
        Self::do_unpause(&e);
    }

    // ── Internal helpers ──

    /// Remove `proposal_id` from `ActiveProposals` if present. Used by both
    /// `execute` (before dispatch) and `cancel_proposal` so a resolved or
    /// cancelled proposal always frees its slot.
    fn remove_from_active(e: &Env, proposal_id: u64) {
        let active: Vec<u64> = e
            .storage()
            .instance()
            .get(&DataKey::ActiveProposals)
            .unwrap_or_else(|| Vec::new(e));
        let mut new_active: Vec<u64> = Vec::new(e);
        for i in 0..active.len() {
            let id = active.get(i).unwrap();
            if id != proposal_id {
                new_active.push_back(id);
            }
        }
        e.storage()
            .instance()
            .set(&DataKey::ActiveProposals, &new_active);
    }

    /// Dispatch a single proposal action.
    ///
    /// In atomic mode (`best_effort == false`) a failing action panics and
    /// aborts the whole `execute()` call. In best-effort mode the outcome is
    /// reported as a boolean so `execute()` can record per-action results.
    fn dispatch_action(e: &Env, action: &GovernanceAction, best_effort: bool) -> bool {
        if action.function == soroban_sdk::Symbol::new(e, "emergency_pause") {
            return Self::pause_all(e, best_effort);
        }
        if action.function == soroban_sdk::Symbol::new(e, "emergency_unpause") {
            return Self::unpause_all(e, best_effort);
        }

        // Generic cross-contract invocation.
        if best_effort {
            e.try_invoke_contract::<Val, InvokeError>(
                &action.target,
                &action.function,
                action.args.clone(),
            )
            .is_ok()
        } else {
            e.invoke_contract::<()>(&action.target, &action.function, action.args.clone());
            true
        }
    }

    /// Pause every registered token contract.
    ///
    /// In best-effort mode a failing token is skipped and the overall result is
    /// reported via the returned boolean; in atomic mode the first failure
    /// panics. Returns true when every token paused successfully.
    fn pause_all(e: &Env, best_effort: bool) -> bool {
        let tokens: Vec<Address> = e
            .storage()
            .instance()
            .get(&DataKey::RegisteredTokens)
            .unwrap_or_else(|| Vec::new(e));

        let gov_addr = e.current_contract_address();
        let mut all_ok = true;
        for i in 0..tokens.len() {
            let token = tokens.get(i).unwrap();
            let args: Vec<Val> = vec![e, gov_addr.clone().to_val()];
            let ok = if best_effort {
                e.try_invoke_contract::<Val, InvokeError>(
                    &token,
                    &soroban_sdk::Symbol::new(e, "pause"),
                    args,
                )
                .is_ok()
            } else {
                e.invoke_contract::<()>(&token, &soroban_sdk::Symbol::new(e, "pause"), args);
                true
            };
            if !ok {
                all_ok = false;
            }
        }

        // Only record a clean protocol-wide pause when every token paused;
        // otherwise the protocol is left partially paused and remediation is
        // observable via `Proposal.action_results` / the `exec_partial` event.
        if all_ok {
            e.storage().instance().set(&DataKey::ProtocolPaused, &true);
            e.events().publish((EVENT_EMERGENCY_PAUSE,), ());
        }
        all_ok
    }

    /// Unpause every registered token contract. Mirrors `pause_all`.
    fn unpause_all(e: &Env, best_effort: bool) -> bool {
        let tokens: Vec<Address> = e
            .storage()
            .instance()
            .get(&DataKey::RegisteredTokens)
            .unwrap_or_else(|| Vec::new(e));

        let gov_addr = e.current_contract_address();
        let mut all_ok = true;
        for i in 0..tokens.len() {
            let token = tokens.get(i).unwrap();
            let args: Vec<Val> = vec![e, gov_addr.clone().to_val()];
            let ok = if best_effort {
                e.try_invoke_contract::<Val, InvokeError>(
                    &token,
                    &soroban_sdk::Symbol::new(e, "unpause"),
                    args,
                )
                .is_ok()
            } else {
                e.invoke_contract::<()>(&token, &soroban_sdk::Symbol::new(e, "unpause"), args);
                true
            };
            if !ok {
                all_ok = false;
            }
        }

        if all_ok {
            e.storage().instance().set(&DataKey::ProtocolPaused, &false);
            e.events().publish((EVENT_EMERGENCY_UNPAUSE,), ());
        }
        all_ok
    }

    fn do_pause(e: &Env) {
        // Atomic by default: a failing token aborts the whole batch.
        let _ = Self::pause_all(e, false);
    }

    fn do_unpause(e: &Env) {
        // Atomic by default: a failing token aborts the whole batch.
        let _ = Self::unpause_all(e, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Events, Ledger as _};
    use soroban_sdk::TryFromVal;

    mod mock_target {
        use soroban_sdk::{contract, contractimpl, contracttype, Env};

        #[contracttype]
        pub enum DataKey {
            Value,
        }

        #[contract]
        pub struct MockTarget;

        #[contractimpl]
        impl MockTarget {
            pub fn set_value(e: Env, val: i128) {
                e.storage().instance().set(&DataKey::Value, &val);
            }

            pub fn get_value(e: Env) -> i128 {
                e.storage().instance().get(&DataKey::Value).unwrap_or(0)
            }

            pub fn always_fail(_e: Env) {
                panic!("intentional failure");
            }

            pub fn echo_arg(e: Env, symbol: soroban_sdk::Symbol) -> soroban_sdk::Symbol {
                e.storage().instance().set(&DataKey::Value, &symbol);
                symbol
            }
        }
    }

    fn setup() -> (Env, Address, Address, GovernanceClient<'static>) {
        let e = Env::default();
        let admin = Address::generate(&e);
        let member1 = Address::generate(&e);
        let contract_id = e.register_contract(None, Governance);
        let client = GovernanceClient::new(&e, &contract_id);

        let members: Vec<Address> = Vec::from_array(&e, [member1.clone()]);
        client.initialize(&admin, &members);

        (e, admin, member1, client)
    }

    #[test]
    fn test_initialize_sets_config_and_members() {
        let (_e, _admin, member1, client) = setup();
        let config = client.get_config();
        assert_eq!(config.fee_bps, 50);
        assert_eq!(config.approval_threshold_bps, 6000);
        assert!(client.is_member_fn(&member1));
        assert_eq!(client.member_count_fn(), 1);
    }

    #[test]
    fn test_propose_creates_proposal() {
        let (e, _admin, member1, client) = setup();
        e.mock_all_auths();

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Test Proposal"),
            &String::from_str(&e, "A test proposal"),
            &actions,
            &false,
        );
        assert_eq!(id, 1);

        let proposal = client.get_proposal(&id).unwrap();
        assert_eq!(proposal.title, String::from_str(&e, "Test Proposal"));
        assert!(matches!(proposal.status, ProposalStatus::Pending));
    }

    #[test]
    fn test_non_member_rejected() {
        let (e, _admin, member1, client) = setup();
        let rogue = Address::generate(&e);
        assert!(client.is_member_fn(&member1));
        assert!(!client.is_member_fn(&rogue));
    }

    #[test]
    fn test_vote_approval() {
        let (e, admin, member1, client) = setup();
        e.mock_all_auths();

        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);
        client.add_member(&admin, &member2);
        client.add_member(&admin, &member3);

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Vote Test"),
            &String::from_str(&e, "desc"),
            &actions,
            &false,
        );

        client.vote(&member1, &id, &true);
        client.vote(&member2, &id, &true);

        let proposal = client.get_proposal(&id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Approved));
    }

    #[test]
    fn test_vote_rejection() {
        let (e, admin, member1, client) = setup();
        e.mock_all_auths();

        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);
        client.add_member(&admin, &member2);
        client.add_member(&admin, &member3);

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Reject Test"),
            &String::from_str(&e, "desc"),
            &actions,
            &false,
        );

        client.vote(&member1, &id, &false);
        client.vote(&member2, &id, &false);

        let proposal = client.get_proposal(&id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Rejected));
    }

    #[test]
    fn test_vote_tracking() {
        let (e, _admin, member1, client) = setup();
        e.mock_all_auths();

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Vote Tracking"),
            &String::from_str(&e, "desc"),
            &actions,
            &false,
        );

        client.vote(&member1, &id, &true);
        let proposal = client.get_proposal(&id).unwrap();
        assert_eq!(proposal.votes_for, 1);
        assert_eq!(proposal.votes_against, 0);
    }

    #[test]
    fn test_member_added_after_proposal_keeps_threshold() {
        let (e, admin, member1, client) = setup();
        e.mock_all_auths();

        // Start with 2 members.
        let member2 = Address::generate(&e);
        client.add_member(&admin, &member2);

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Add Member Test"),
            &String::from_str(&e, "desc"),
            &actions,
            &false,
        );

        let proposal = client.get_proposal(&id).unwrap();
        assert_eq!(proposal.eligible_voters, 2);

        // A third member is added AFTER the proposal was created.
        let member3 = Address::generate(&e);
        client.add_member(&admin, &member3);
        assert_eq!(client.member_count_fn(), 3);

        // Only the original two members vote. With the old 100%-turnout logic
        // this could never reach `total_votes >= total_members`, leaving the
        // proposal stuck. The snapshot-based quorum must still resolve it.
        client.vote(&member1, &id, &true);

        let proposal = client.get_proposal(&id).unwrap();
        assert_eq!(proposal.eligible_voters, 2);
        assert!(matches!(proposal.status, ProposalStatus::Approved));
    }

    #[test]
    fn test_quorum_reached_before_full_turnout() {
        let (e, admin, member1, client) = setup();
        e.mock_all_auths();

        // Three members at creation time.
        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);
        client.add_member(&admin, &member2);
        client.add_member(&admin, &member3);

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Quorum Test"),
            &String::from_str(&e, "desc"),
            &actions,
            &false,
        );

        let proposal = client.get_proposal(&id).unwrap();
        assert_eq!(proposal.eligible_voters, 3);

        // Quorum (50% of 3 -> 2 votes) is reached with two "yes" votes; the
        // third member never votes, yet the proposal must resolve.
        client.vote(&member1, &id, &true);
        client.vote(&member2, &id, &true);

        let proposal = client.get_proposal(&id).unwrap();
        assert_eq!(proposal.votes_for, 2);
        assert_eq!(proposal.votes_against, 0);
        assert!(matches!(proposal.status, ProposalStatus::Approved));
    }

    #[test]
    fn test_member_removed_after_voting_preserves_counts() {
        let (e, admin, member1, client) = setup();
        e.mock_all_auths();

        // Three members at creation time.
        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);
        client.add_member(&admin, &member2);
        client.add_member(&admin, &member3);

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Remove Member Test"),
            &String::from_str(&e, "desc"),
            &actions,
            &false,
        );

        // Two members vote yes; the third is then removed.
        client.vote(&member1, &id, &true);
        client.vote(&member2, &id, &true);
        client.remove_member(&admin, &member3);

        let proposal = client.get_proposal(&id).unwrap();
        // Eligible-voter snapshot must remain 3 (not drop to 2).
        assert_eq!(proposal.eligible_voters, 3);
        // Vote counts must reflect exactly the two recorded votes.
        assert_eq!(proposal.votes_for, 2);
        assert_eq!(proposal.votes_against, 0);
        // Quorum (50% of 3 -> 2) is met, so the proposal resolves to Approved
        // rather than being corrupted by the removed member.
        assert!(matches!(proposal.status, ProposalStatus::Approved));
    }

    #[test]
    fn test_vote_on_resolved_proposal_panics_no_hasvoted_set() {
        let e = Env::default();
        e.mock_all_auths();

        let admin = Address::generate(&e);
        let member1 = Address::generate(&e);
        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);

        let gov_id = e.register_contract(None, Governance);
        let client = GovernanceClient::new(&e, &gov_id);
        client.initialize(
            &admin,
            &Vec::from_array(&e, [member1.clone(), member2.clone(), member3.clone()]),
        );

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Resolved Proposal"),
            &String::from_str(&e, "desc"),
            &actions,
            &false,
        );

        client.vote(&member1, &id, &true);
        client.vote(&member2, &id, &true);

        let proposal = client.get_proposal(&id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Approved));

        let result = client.try_vote(&member3, &id, &true);
        assert!(result.is_err());

        let voted_key = DataKey::HasVoted(id, member3.clone());
        let has_voted_set = e.as_contract(&gov_id, || e.storage().persistent().has(&voted_key));
        assert!(
            !has_voted_set,
            "HasVoted must not be set after a failed vote on a resolved proposal"
        );
    }

    #[test]
    #[should_panic(expected = "proposal already resolved")]
    fn test_vote_on_approved_proposal_panics_message() {
        let (e, admin, member1, client) = setup();
        e.mock_all_auths();

        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);
        client.add_member(&admin, &member2);
        client.add_member(&admin, &member3);

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Panic Message Test"),
            &String::from_str(&e, "desc"),
            &actions,
            &false,
        );

        client.vote(&member1, &id, &true);
        client.vote(&member2, &id, &true);

        client.vote(&member3, &id, &true);
    }

    #[test]
    fn test_execute_after_timelock() {
        let e = Env::default();
        e.mock_all_auths();
        let admin = Address::generate(&e);
        let member1 = Address::generate(&e);
        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);
        let contract_id = e.register_contract(None, Governance);
        let client = GovernanceClient::new(&e, &contract_id);

        let members: Vec<Address> =
            Vec::from_array(&e, [member1.clone(), member2.clone(), member3.clone()]);
        client.initialize(&admin, &members);

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Exec Test"),
            &String::from_str(&e, "desc"),
            &actions,
            &false,
        );

        client.vote(&member1, &id, &true);
        client.vote(&member2, &id, &true);

        let proposal = client.get_proposal(&id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Approved));

        // Jump past timelock
        let mut info = e.ledger().get();
        info.timestamp = proposal.timelock_ends_at + 1;
        e.ledger().set(info);

        client.execute(&member1, &id);

        let proposal = client.get_proposal(&id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Executed));
    }

    #[test]
    fn test_timelock_not_elapsed() {
        let (e, admin, member1, client) = setup();
        e.mock_all_auths();

        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);
        client.add_member(&admin, &member2);
        client.add_member(&admin, &member3);

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Timelock Test"),
            &String::from_str(&e, "desc"),
            &actions,
            &false,
        );

        client.vote(&member1, &id, &true);
        client.vote(&member2, &id, &true);

        let proposal = client.get_proposal(&id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Approved));
        assert!(proposal.timelock_ends_at > e.ledger().timestamp());
    }

    #[test]
    fn test_add_member() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let new_member = Address::generate(&e);
        client.add_member(&admin, &new_member);
        assert!(client.is_member_fn(&new_member));
        assert_eq!(client.member_count_fn(), 2);
    }

    #[test]
    fn test_remove_member() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let member2 = Address::generate(&e);
        client.add_member(&admin, &member2);
        client.remove_member(&admin, &member2);
        assert!(!client.is_member_fn(&member2));
        assert_eq!(client.member_count_fn(), 1);
    }

    #[test]
    fn test_last_member_guard() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        // Add second member, remove it, then check count is still 1
        let member2 = Address::generate(&e);
        client.add_member(&admin, &member2);
        client.remove_member(&admin, &member2);
        assert_eq!(client.member_count_fn(), 1);
    }

    #[test]
    fn test_update_config_succeeds() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let new_config = GovernanceConfig {
            fee_bps: 100,
            voting_period: 432000,
            timelock_duration: 43200,
            approval_threshold_bps: 5000,
            quorum_bps: 5000,
            min_proposal_deposit: 500,
            max_active_proposals: 20,
        };
        client.update_config(&admin, &new_config);

        let config = client.get_config();
        assert_eq!(config.fee_bps, 100);
        assert_eq!(config.max_active_proposals, 20);
    }

    #[test]
    fn test_update_config_rejects_zero_voting_period() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let mut config = client.get_config();
        config.voting_period = 0;
        let result = client.try_update_config(&admin, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_config_rejects_approval_threshold_over_100_pct() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let mut config = client.get_config();
        config.approval_threshold_bps = 10001;
        let result = client.try_update_config(&admin, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_config_accepts_approval_threshold_at_100_pct() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let mut config = client.get_config();
        config.approval_threshold_bps = 10000;
        client.update_config(&admin, &config);
        assert_eq!(client.get_config().approval_threshold_bps, 10000);
    }

    #[test]
    fn test_update_config_accepts_zero_timelock() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let mut config = client.get_config();
        config.timelock_duration = 0;
        client.update_config(&admin, &config);
        assert_eq!(client.get_config().timelock_duration, 0);
    }

    #[test]
    fn test_update_config_rejected_update_does_not_change_stored_config() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let original = client.get_config();

        let mut bad_config = original.clone();
        bad_config.voting_period = 0;
        let result = client.try_update_config(&admin, &bad_config);
        assert!(result.is_err());

        assert_eq!(client.get_config(), original);
    }

    #[test]
    fn test_transfer_admin() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let new_admin = Address::generate(&e);
        client.transfer_admin(&admin, &new_admin);

        // New admin can now perform admin actions
        let config = GovernanceConfig {
            fee_bps: 200,
            voting_period: 604800,
            timelock_duration: 86400,
            approval_threshold_bps: 6000,
            quorum_bps: 5000,
            min_proposal_deposit: 1000,
            max_active_proposals: 10,
        };
        client.update_config(&new_admin, &config);
        assert_eq!(client.get_config().fee_bps, 200);

        // Old admin should be rejected
        let config2 = GovernanceConfig {
            fee_bps: 300,
            voting_period: 604800,
            timelock_duration: 86400,
            approval_threshold_bps: 6000,
            quorum_bps: 5000,
            min_proposal_deposit: 1000,
            max_active_proposals: 10,
        };
        let result = client.try_update_config(&admin, &config2);
        assert!(result.is_err());
    }

    #[test]
    fn test_expired_proposal_state() {
        let (e, _admin, member1, client) = setup();
        e.mock_all_auths();

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Expired"),
            &String::from_str(&e, "desc"),
            &actions,
            &false,
        );

        // Jump past voting deadline
        let config = client.get_config();
        let mut info = e.ledger().get();
        info.timestamp = config.voting_period + 1;
        e.ledger().set(info);

        let proposal = client.get_proposal(&id).unwrap();
        assert!(proposal.voting_ends_at < e.ledger().timestamp());
    }

    // ── Token registry tests ──

    #[test]
    fn test_register_and_list_tokens() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        // Generate fake token addresses (no real contract needed for registry tests).
        let token_a = Address::generate(&e);
        let token_b = Address::generate(&e);

        assert_eq!(client.list_registered_tokens().len(), 0);

        client.register_token(&admin, &token_a);
        assert_eq!(client.list_registered_tokens().len(), 1);

        client.register_token(&admin, &token_b);
        assert_eq!(client.list_registered_tokens().len(), 2);
    }

    #[test]
    fn test_register_token_idempotent() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let token = Address::generate(&e);
        client.register_token(&admin, &token);
        client.register_token(&admin, &token);
        client.register_token(&admin, &token);

        assert_eq!(client.list_registered_tokens().len(), 1);
    }

    #[test]
    fn test_deregister_token() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let token_a = Address::generate(&e);
        let token_b = Address::generate(&e);
        client.register_token(&admin, &token_a);
        client.register_token(&admin, &token_b);

        client.deregister_token(&admin, &token_a);

        let remaining = client.list_registered_tokens();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining.get(0).unwrap(), token_b);
    }

    #[test]
    fn test_is_protocol_paused_initial_state() {
        let (_e, _admin, _member, client) = setup();
        assert!(!client.is_protocol_paused());
    }

    // ── Cross-contract invocation tests ──

    #[test]
    fn test_execute_generic_cross_contract_action() {
        let e = Env::default();
        e.mock_all_auths();

        let admin = Address::generate(&e);
        let member = Address::generate(&e);

        let mock_id = e.register_contract(None, mock_target::MockTarget);
        let mock_client = mock_target::MockTargetClient::new(&e, &mock_id);

        let gov_id = e.register_contract(None, Governance);
        let gov_client = GovernanceClient::new(&e, &gov_id);
        gov_client.initialize(&admin, &Vec::from_array(&e, [member.clone()]));

        let action = GovernanceAction {
            target: mock_id.clone(),
            function: soroban_sdk::Symbol::new(&e, "set_value"),
            args: Vec::from_array(&e, [soroban_sdk::IntoVal::into_val(&42i128, &e)]),
        };
        let actions = Vec::from_array(&e, [action]);

        let proposal_id = gov_client.propose(
            &member,
            &String::from_str(&e, "Set Mock Value"),
            &String::from_str(&e, "Sets the mock value to 42"),
            &actions,
            &false,
        );

        gov_client.vote(&member, &proposal_id, &true);
        let proposal = gov_client.get_proposal(&proposal_id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Approved));

        let mut info = e.ledger().get();
        info.timestamp = proposal.timelock_ends_at + 1;
        e.ledger().set(info);

        gov_client.execute(&member, &proposal_id);

        assert_eq!(mock_client.get_value(), 42);

        let proposal = gov_client.get_proposal(&proposal_id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Executed));
    }

    #[test]
    fn test_execute_multiple_actions_sequential() {
        let e = Env::default();
        e.mock_all_auths();

        let admin = Address::generate(&e);
        let member = Address::generate(&e);

        let mock_id = e.register_contract(None, mock_target::MockTarget);
        let mock_client = mock_target::MockTargetClient::new(&e, &mock_id);

        let gov_id = e.register_contract(None, Governance);
        let gov_client = GovernanceClient::new(&e, &gov_id);
        gov_client.initialize(&admin, &Vec::from_array(&e, [member.clone()]));

        let action1 = GovernanceAction {
            target: mock_id.clone(),
            function: soroban_sdk::Symbol::new(&e, "set_value"),
            args: Vec::from_array(&e, [soroban_sdk::IntoVal::into_val(&42i128, &e)]),
        };
        let action2 = GovernanceAction {
            target: mock_id.clone(),
            function: soroban_sdk::Symbol::new(&e, "set_value"),
            args: Vec::from_array(&e, [soroban_sdk::IntoVal::into_val(&123i128, &e)]),
        };
        let actions = Vec::from_array(&e, [action1, action2]);

        let proposal_id = gov_client.propose(
            &member,
            &String::from_str(&e, "Set Values"),
            &String::from_str(&e, "Sets the mock value twice"),
            &actions,
            &false,
        );

        gov_client.vote(&member, &proposal_id, &true);
        let proposal = gov_client.get_proposal(&proposal_id).unwrap();

        let mut info = e.ledger().get();
        info.timestamp = proposal.timelock_ends_at + 1;
        e.ledger().set(info);

        gov_client.execute(&member, &proposal_id);

        // Last call wins — the mock stores the most recent value.
        assert_eq!(mock_client.get_value(), 123);
    }

    #[test]
    fn test_execute_reverts_on_failed_action() {
        let e = Env::default();
        e.mock_all_auths();

        let admin = Address::generate(&e);
        let member = Address::generate(&e);

        let mock_id = e.register_contract(None, mock_target::MockTarget);

        let gov_id = e.register_contract(None, Governance);
        let gov_client = GovernanceClient::new(&e, &gov_id);
        gov_client.initialize(&admin, &Vec::from_array(&e, [member.clone()]));

        let action = GovernanceAction {
            target: mock_id.clone(),
            function: soroban_sdk::Symbol::new(&e, "always_fail"),
            args: Vec::new(&e),
        };
        let actions = Vec::from_array(&e, [action]);

        let proposal_id = gov_client.propose(
            &member,
            &String::from_str(&e, "Fail Proposal"),
            &String::from_str(&e, "This will fail"),
            &actions,
            &false,
        );

        gov_client.vote(&member, &proposal_id, &true);
        let proposal = gov_client.get_proposal(&proposal_id).unwrap();

        let mut info = e.ledger().get();
        info.timestamp = proposal.timelock_ends_at + 1;
        e.ledger().set(info);

        let result = gov_client.try_execute(&member, &proposal_id);
        assert!(result.is_err(), "execute must revert when an action fails");

        let proposal = gov_client.get_proposal(&proposal_id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Approved));
    }

    #[test]
    fn test_execute_partial_execution_records_action_results() {
        let e = Env::default();
        e.mock_all_auths();

        let admin = Address::generate(&e);
        let member = Address::generate(&e);

        let mock_id = e.register_contract(None, mock_target::MockTarget);
        let mock_client = mock_target::MockTargetClient::new(&e, &mock_id);

        let gov_id = e.register_contract(None, Governance);
        let gov_client = GovernanceClient::new(&e, &gov_id);
        gov_client.initialize(&admin, &Vec::from_array(&e, [member.clone()]));

        // Three actions: set_value(1), always_fail, set_value(3). In best-effort
        // mode the middle failure must not abort the batch.
        let action1 = GovernanceAction {
            target: mock_id.clone(),
            function: soroban_sdk::Symbol::new(&e, "set_value"),
            args: Vec::from_array(&e, [soroban_sdk::IntoVal::into_val(&1i128, &e)]),
        };
        let action2 = GovernanceAction {
            target: mock_id.clone(),
            function: soroban_sdk::Symbol::new(&e, "always_fail"),
            args: Vec::new(&e),
        };
        let action3 = GovernanceAction {
            target: mock_id.clone(),
            function: soroban_sdk::Symbol::new(&e, "set_value"),
            args: Vec::from_array(&e, [soroban_sdk::IntoVal::into_val(&3i128, &e)]),
        };
        let actions = Vec::from_array(&e, [action1, action2, action3]);

        let proposal_id = gov_client.propose(
            &member,
            &String::from_str(&e, "Best-effort set values"),
            &String::from_str(&e, "action 2 fails; 1 and 3 must still run"),
            &actions,
            &true,
        );

        gov_client.vote(&member, &proposal_id, &true);
        let proposal = gov_client.get_proposal(&proposal_id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Approved));

        let mut info = e.ledger().get();
        info.timestamp = proposal.timelock_ends_at + 1;
        e.ledger().set(info);

        gov_client.execute(&member, &proposal_id);

        // The two successful actions must have run; the final value reflects
        // the last success (action 3).
        assert_eq!(mock_client.get_value(), 3);

        let proposal = gov_client.get_proposal(&proposal_id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Executed));
        assert_eq!(proposal.action_results.len(), 3);
        assert!(proposal.action_results.get(0).unwrap());
        assert!(!proposal.action_results.get(1).unwrap());
        assert!(proposal.action_results.get(2).unwrap());

        // The exec_partial event must be emitted as the final event, carrying
        // (proposal_id, action_results).
        let events = e.events().all();
        let (_contract, topics, data) = &events.get(events.len() - 1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, Symbol::new(&e, "exec_partial"));

        let (ev_id, ev_results) = <(u64, Vec<bool>)>::try_from_val(&e, data).unwrap();
        assert_eq!(ev_id, proposal_id);
        assert_eq!(ev_results.len(), 3);
        assert!(ev_results.get(0).unwrap());
        assert!(!ev_results.get(1).unwrap());
        assert!(ev_results.get(2).unwrap());
    }

    #[test]
    fn test_cancel_proposal_by_admin_frees_slot() {
        let (e, admin, member1, client) = setup();
        e.mock_all_auths();

        // Reduce the active-proposal limit to 1 so that freeing the slot is
        // observable: a second proposal is rejected while the first is active
        // and accepted after it is cancelled.
        let mut config = client.get_config();
        config.max_active_proposals = 1;
        client.update_config(&admin, &config);

        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);
        client.add_member(&admin, &member2);
        client.add_member(&admin, &member3);

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Cancel Me"),
            &String::from_str(&e, "first proposal"),
            &actions,
            &false,
        );

        // Slot is full.
        let second = client.try_propose(
            &member1,
            &String::from_str(&e, "Blocked"),
            &String::from_str(&e, "must be rejected"),
            &actions,
            &false,
        );
        assert!(second.is_err());

        // Approve, then cancel.
        client.vote(&member1, &id, &true);
        client.vote(&member2, &id, &true);
        client.cancel_proposal(&admin, &id);

        // The prop_cancel event must be emitted as the final event so far.
        let events = e.events().all();
        let (_contract, topics, _data) = &events.get(events.len() - 1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, Symbol::new(&e, "prop_cancel"));

        let proposal = client.get_proposal(&id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Cancelled));

        // A cancelled proposal can no longer be executed.
        let mut info = e.ledger().get();
        info.timestamp = proposal.timelock_ends_at + 1;
        e.ledger().set(info);
        assert!(client.try_execute(&member1, &id).is_err());

        // The freed slot allows a new proposal.
        let id2 = client.propose(
            &member1,
            &String::from_str(&e, "Second Chance"),
            &String::from_str(&e, "succeeds after cancellation"),
            &actions,
            &false,
        );
        assert_eq!(id2, 2);
    }

    #[test]
    fn test_cancel_proposal_by_non_admin_panics() {
        let (e, admin, member1, client) = setup();
        e.mock_all_auths();

        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);
        client.add_member(&admin, &member2);
        client.add_member(&admin, &member3);

        let actions: Vec<GovernanceAction> = Vec::new(&e);
        let id = client.propose(
            &member1,
            &String::from_str(&e, "Keep Me"),
            &String::from_str(&e, "must not be cancellable by a member"),
            &actions,
            &false,
        );

        client.vote(&member1, &id, &true);
        client.vote(&member2, &id, &true);

        // A non-admin member must not be able to cancel.
        assert!(client.try_cancel_proposal(&member1, &id).is_err());

        let proposal = client.get_proposal(&id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Approved));
    }

    #[test]
    fn test_execute_generic_action_preserves_auth_model() {
        let e = Env::default();
        e.mock_all_auths();

        let admin = Address::generate(&e);
        let member1 = Address::generate(&e);
        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);
        let member4 = Address::generate(&e);
        let member5 = Address::generate(&e);

        let mock_id = e.register_contract(None, mock_target::MockTarget);
        let mock_client = mock_target::MockTargetClient::new(&e, &mock_id);

        let gov_id = e.register_contract(None, Governance);
        let gov_client = GovernanceClient::new(&e, &gov_id);
        gov_client.initialize(
            &admin,
            &Vec::from_array(
                &e,
                [
                    member1.clone(),
                    member2.clone(),
                    member3.clone(),
                    member4.clone(),
                    member5.clone(),
                ],
            ),
        );

        let action = GovernanceAction {
            target: mock_id.clone(),
            function: soroban_sdk::Symbol::new(&e, "set_value"),
            args: Vec::from_array(&e, [soroban_sdk::IntoVal::into_val(&999i128, &e)]),
        };
        let actions = Vec::from_array(&e, [action]);

        let proposal_id = gov_client.propose(
            &member1,
            &String::from_str(&e, "Auth Test"),
            &String::from_str(&e, "Verifies auth model is preserved"),
            &actions,
            &false,
        );

        gov_client.vote(&member1, &proposal_id, &true);
        gov_client.vote(&member2, &proposal_id, &true);
        gov_client.vote(&member3, &proposal_id, &true);

        let proposal = gov_client.get_proposal(&proposal_id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Approved));

        let mut info = e.ledger().get();
        info.timestamp = proposal.timelock_ends_at + 1;
        e.ledger().set(info);

        gov_client.execute(&member1, &proposal_id);

        assert_eq!(mock_client.get_value(), 999);
    }

    #[test]
    fn test_emergency_pause_action_still_works() {
        let (e, admin, member1, client) = setup();
        e.mock_all_auths();

        let member2 = Address::generate(&e);
        let member3 = Address::generate(&e);
        client.add_member(&admin, &member2);
        client.add_member(&admin, &member3);

        let pause_action = GovernanceAction {
            target: admin.clone(), // target is ignored for built-in actions
            function: soroban_sdk::Symbol::new(&e, "emergency_pause"),
            args: Vec::new(&e),
        };
        let actions = Vec::from_array(&e, [pause_action]);

        let proposal_id = client.propose(
            &member1,
            &String::from_str(&e, "Pause"),
            &String::from_str(&e, "pause the protocol"),
            &actions,
            &false,
        );

        client.vote(&member1, &proposal_id, &true);
        client.vote(&member2, &proposal_id, &true);

        let proposal = client.get_proposal(&proposal_id).unwrap();
        let mut info = e.ledger().get();
        info.timestamp = proposal.timelock_ends_at + 1;
        e.ledger().set(info);

        client.execute(&member1, &proposal_id);

        assert!(client.is_protocol_paused());
        let proposal = client.get_proposal(&proposal_id).unwrap();
        assert!(matches!(proposal.status, ProposalStatus::Executed));
    }

    // ── Event tests ──

    #[test]
    fn test_initialize_emits_event() {
        let e = Env::default();
        let admin = Address::generate(&e);
        let member1 = Address::generate(&e);
        let contract_id = e.register_contract(None, Governance);
        let client = GovernanceClient::new(&e, &contract_id);

        let members: Vec<Address> = Vec::from_array(&e, [member1]);
        client.initialize(&admin, &members);

        let events = e.events().all();
        assert_eq!(events.len(), 1);
        let (_contract, topics, _data) = &events.get(0).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("init"));
    }

    #[test]
    fn test_transfer_admin_emits_event() {
        let (e, admin, _member1, client) = setup();
        e.mock_all_auths();

        let new_admin = Address::generate(&e);
        client.transfer_admin(&admin, &new_admin);

        let events = e.events().all();
        // initialize(1) + transfer_admin(1) = 2
        assert_eq!(events.len(), 2);
        let (_contract, topics, data) = &events.get(1).unwrap();
        let topic: Symbol = Symbol::try_from_val(&e, &topics.get(0).unwrap()).unwrap();
        assert_eq!(topic, symbol_short!("adm_xfer"));

        let (ev_old_admin, ev_new_admin) = <(Address, Address)>::try_from_val(&e, data).unwrap();
        assert_eq!(ev_old_admin, admin);
        assert_eq!(ev_new_admin, new_admin);
    }
}
