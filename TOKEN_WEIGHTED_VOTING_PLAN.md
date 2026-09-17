# Token-Weighted Voting Implementation Plan

## Overview
Transition from count-based (1-member-1-vote) governance to token-weighted voting where voting power is proportional to credit token holdings. This is the v2.0 roadmap headline item enabling legitimate decentralized control.

## Key Design Decisions

### 1. Voting Power Snapshot
- **Strategy**: Capture total voting power at proposal creation time (not at vote time)
- **Rationale**: Prevents last-block balance manipulation and keeps snapshot immutable for proposal lifetime
- **Implementation**: Store `total_eligible_weight: i128` in Proposal struct
- **Calculation**: Sum of credit_token total_supply at proposal creation

### 2. Voting Power Source
- **Primary**: Credit token balance (via credit_token contract)
- **Token Reference**: Governance will track a designated governance token address
- **Query**: Per-voter at vote time (voters must have registered balances)

### 3. Weight Accumulation
- Replace `votes_for: u32` and `votes_against: u32` counters with weighted accumulation
- New fields: `votes_for_weight: i128` and `votes_against_weight: i128`
- Keep vote counts for audit trail (in separate tracking)

### 4. Quorum/Approval Math
- **Quorum**: Change from `ceil(quorum_bps/10000 * eligible_voters_count)` to `ceil(quorum_bps/10000 * total_eligible_weight)`
- **Approval**: Change from percentage of cast votes to percentage of cast voting power
- Both calculations remain basis-point based (same config format)

### 5. Backwards Compatibility
- Migrate gracefully: new proposals use token-weighted voting
- Existing member-count-based proposals remain valid
- No retroactive changes to already-created proposals

## Implementation Changes

### Phase 1: Data Structures

#### Proposal Struct Addition
```rust
pub struct Proposal {
    // ... existing fields ...
    pub eligible_voters: u32,           // KEEP for backwards compat
    pub votes_for: u32,                 // KEEP for audit trail
    pub votes_against: u32,             // KEEP for audit trail
    
    // NEW: Token-weighted voting fields
    pub total_eligible_weight: i128,    // Snapshot of total voting power at creation
    pub votes_for_weight: i128,         // Accumulated voting power for yes votes
    pub votes_against_weight: i128,     // Accumulated voting power for no votes
}
```

#### DataKey Addition
```rust
pub enum DataKey {
    // ... existing ...
    GovernanceTokenAddress,    // NEW: Address of token used for voting power
    VoterWeight(u64, Address), // NEW: Cached weight per voter per proposal (optional optimization)
}
```

#### GovernanceConfig Addition (Optional)
```rust
pub struct GovernanceConfig {
    // ... existing fields ...
    // NEW: Could add fields for weight-based minimums (e.g., min_voting_power)
    // For now, reuse existing quorum_bps and approval_threshold_bps
}
```

### Phase 2: Helper Functions

#### Query Voter Weight
```rust
fn get_voter_voting_weight(e: &Env, voter: &Address) -> i128 {
    // Query credit_token balance for this voter
    // Return 0 if no balance or cross-contract call fails
}
```

#### Snapshot Total Voting Power
```rust
fn get_total_eligible_weight(e: &Env) -> i128 {
    // Query credit_token total_supply at proposal creation
    // This is immutable for the proposal's lifetime
}
```

#### Resolve Proposal with Weight
```rust
fn resolve_proposal_with_weight(e: &Env, proposal: &mut Proposal, timestamp: u64) {
    // Apply weighted quorum/approval logic
    // Update proposal.status based on voting power
}
```

### Phase 3: Core Function Changes

#### `propose()`
- When creating proposal, call `get_total_eligible_weight()` and store in `total_eligible_weight`
- Set `votes_for_weight = 0` and `votes_against_weight = 0` (start of voting)
- Validate governance token is registered

#### `vote()`
- Query voter's credit_token balance: `get_voter_voting_weight(voter)`
- Add to `votes_for_weight` or `votes_against_weight` instead of incrementing counts
- Use weighted resolution logic in quorum/approval check
- Mark voter as having voted (existing `HasVoted` dedup)

#### Resolution Logic (in `vote()` or separate function)
```
total_weight_cast = votes_for_weight + votes_against_weight
quorum_weight = (total_eligible_weight * quorum_bps) / 10000

if total_weight_cast >= quorum_weight:
    yes_pct = (votes_for_weight * 10000) / total_weight_cast
    if yes_pct >= approval_threshold_bps:
        status = Approved
    else:
        status = Rejected
```

### Phase 4: Governance Token Registration

#### Set Governance Token
```rust
pub fn set_governance_token(e: Env, admin: Address, token: Address) {
    // Only admin can set
    // Validate token contract exists
    // Store at DataKey::GovernanceTokenAddress
}
```

#### Get Governance Token
```rust
fn get_governance_token(e: &Env) -> Address {
    // Return registered governance token
    // Panic if not set
}
```

## Cross-Contract Integration

### Credit Token Contract Changes (Minimal)
- Ensure `balance()` function is public (already is at line 727-730)
- Ensure `get_total_supply()` or similar is accessible
- No modifications needed; governance queries via existing public interface

### Governance Contract Integration Points
1. **At `propose()`**: Query credit_token for total supply → `total_eligible_weight`
2. **At `vote()`**: Query credit_token for voter balance → voter voting weight
3. **Error handling**: If cross-contract call fails, treat as 0 weight (graceful degradation)

## Acceptance Criteria Verification

### ✅ Criterion 1: Approval is function of aggregated voting power
- [ ] Proposal stores `votes_for_weight` and `votes_against_weight` (i128)
- [ ] Approval threshold applied to weighted votes, not counts
- [ ] Tests verify weight-based approval works

### ✅ Criterion 2: Vote-power snapshot immutable and resistant to manipulation
- [ ] `total_eligible_weight` captured at proposal creation
- [ ] Immutable for proposal lifetime
- [ ] Voting power taken at vote time, not vote creation time
- [ ] Last-block balance changes don't affect already-voted proposals

### ✅ Criterion 3: Existing quorum/threshold properties hold
- [ ] Proposals resolve within timelock (liveness preserved)
- [ ] Cannot get stuck waiting for 100% participation (quorum-based)
- [ ] Member removal doesn't retroactively change thresholds (snapshot)
- [ ] Tests verify these properties still hold with token weights

### ✅ Criterion 4: Backward compatibility (Nice to have)
- [ ] Existing count-based fields stay for audit trail
- [ ] Member-based fallback mechanism option (if needed)

## File Changes Summary

### Primary Files
1. **contracts/governance/src/lib.rs**
   - Modify `Proposal` struct: add weight fields
   - Modify `DataKey` enum: add governance token key
   - Add helper functions: `get_voter_voting_weight()`, `get_total_eligible_weight()`, `resolve_proposal_with_weight()`
   - Update `propose()`: capture total eligible weight
   - Update `vote()`: apply weighted voting logic
   - Add `set_governance_token()` and `get_governance_token()`
   - Update tests: verify weight-based voting

2. **contracts/governance/src/lib.rs (Tests)**
   - Add test: `test_token_weighted_proposal_approval()`
   - Add test: `test_quorum_with_token_weights()`
   - Add test: `test_voting_power_snapshot_immutability()`
   - Add test: `test_last_block_manipulation_prevention()`
   - Update existing tests to handle both vote tracking and weight tracking

### Secondary Files
- contracts/credit_token/src/lib.rs: No changes (query only)

## Rollout Strategy

1. **Canary**: Deploy governance v2 with token-weighted voting alongside existing system
2. **Testing**: Run full test suite including edge cases (zero-weight voters, delegation prevention)
3. **Migration**: Require admin to call `set_governance_token()` once token is ready
4. **Cutover**: New proposals use weighted voting; existing proposals unaffected

## Known Limitations & Out of Scope

- **Delegation**: Voters cannot delegate voting power (quadratic voting also out of scope)
- **Zero-balance voters**: Can still vote if they're members, but with 0 weight
- **Token supply changes**: Voting power snapshot prevents issues, but weight calculation uses balance at vote time
- **Multiple tokens**: Single governance token per contract (could be extended)

## Risk Mitigation

| Risk | Mitigation |
|------|------------|
| Cross-contract call failure | Graceful degradation: treat as 0 weight, allow voting |
| Last-block manipulation | Voting power snapshot at proposal creation |
| Stuck proposals | Quorum based on total weight, not 100% turnout |
| Member vs weight mismatch | Member status checked independently; weight is orthogonal |
| Integer overflow | Use i128 for voting power; Soroban enforces bounds |

## Success Metrics

1. All acceptance criteria pass
2. Existing tests continue to pass (backwards compat)
3. New token-weighted tests achieve >90% coverage of weight logic
4. Gas cost acceptable (profile against count-based voting)
5. No breaking changes to public API (only addition of new function)
