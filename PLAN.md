# Implementation Plan: Proposal Deposit Mechanism for Governance Contract

## Overview

This plan implements the `min_proposal_deposit` feature that currently exists in `GovernanceConfig` but is never enforced. The deposit mechanism provides economic spam protection by requiring proposers to escrow tokens when creating proposals.

## Current State Analysis

**Problem:** `min_proposal_deposit: i128 = 1000` exists in `GovernanceConfig` (line 36) but is never checked or enforced in `propose()` (lines 206-274). Any governance member can create proposals for free.

**Key Findings:**
- No `DepositToken` or `ProposalDeposit` storage keys exist in `DataKey` enum
- Cross-contract token transfer patterns already exist in `verification_oracle` (stake/unstake)
- Credit token has `transfer_from()`, `approve()`, and `balance()` functions available
- Backward compatibility required: when `deposit_token` is not configured, no deposit is required

## Implementation Plan

### Step 1: Add New DataKey Variants

**File:** `contracts/governance/src/lib.rs` (lines 88-101)

Add two new variants to the `DataKey` enum:
```rust
pub enum DataKey {
    // ... existing variants ...
    // ── Instance ──
    DepositToken,           // Address - token contract for proposal deposits
    // ── Persistent ──
    ProposalDeposit(u64),   // i128 - deposit amount for proposal_id
}
```

**Storage TTL:** `ProposalDeposit` should use the same TTL constants as `Proposal` (2 years).

### Step 2: Add `set_deposit_token()` Admin Function

**File:** `contracts/governance/src/lib.rs`

Add a new public function after `update_config()` (line 459):
```rust
/// Set the token contract used for proposal deposits. Admin only.
/// When set, proposers must escrow `min_proposal_deposit` tokens.
/// Pass None/empty to disable deposits.
pub fn set_deposit_token(e: Env, admin: Address, token: Option<Address>) {
    admin.require_auth();
    let stored: Address = read_admin(&e);
    if admin != stored {
        panic!("unauthorized");
    }
    match token {
        Some(addr) => e.storage().instance().set(&DataKey::DepositToken, &addr),
        None => e.storage().instance().remove(&DataKey::DepositToken),
    }
}
```

### Step 3: Add Helper Function to Read Deposit Token

**File:** `contracts/governance/src/lib.rs`

Add a helper function after `is_member()` (line 129):
```rust
fn read_deposit_token(e: &Env) -> Option<Address> {
    e.storage().instance().get(&DataKey::DepositToken)
}
```

### Step 4: Modify `propose()` to Enforce Deposit

**File:** `contracts/governance/src/lib.rs` (lines 206-274)

Insert deposit logic after the member check (line 217) and before proposal creation (line 219):

```rust
// Check and collect deposit if configured
let deposit_amount = config.min_proposal_deposit;
if deposit_amount > 0 {
    if let Some(token_addr) = read_deposit_token(&e) {
        // Transfer deposit from proposer to governance contract
        let transfer_args: Vec<Val> = vec![
            &e,
            proposer.to_val(),                    // from
            e.current_contract_address().to_val(), // to (self)
            deposit_amount.into_val(&e),           // amount
        ];
        e.invoke_contract::<()>(&token_addr, &Symbol::new(&e, "transfer_from"), transfer_args);
        
        // Store the deposit amount for this proposal
        e.storage().persistent().set(
            &DataKey::ProposalDeposit(proposal_id),
            &deposit_amount,
        );
        e.storage().persistent().extend_ttl(
            &DataKey::ProposalDeposit(proposal_id),
            PROPOSAL_TTL_THRESHOLD,
            PROPOSAL_TTL_BUMP,
        );
    }
}
```

**Note:** The proposer must first call `approve()` on the token contract to allow the governance contract to spend their tokens.

### Step 5: Modify `execute()` to Refund Deposit

**File:** `contracts/governance/src/lib.rs` (lines 373-449)

After marking the proposal as `Executed` (line 439), add deposit refund logic:

```rust
// Refund deposit if configured
if let Some(token_addr) = read_deposit_token(&e) {
    let deposit_key = DataKey::ProposalDeposit(proposal_id);
    if let Some(deposit_amount) = e.storage().persistent().get::<DataKey, i128>(&deposit_key) {
        let transfer_args: Vec<Val> = vec![
            &e,
            e.current_contract_address().to_val(), // from (self)
            proposal.proposer.to_val(),             // to (proposer)
            deposit_amount.into_val(&e),            // amount
        ];
        e.invoke_contract::<()>(&token_addr, &Symbol::new(&e, "transfer"), transfer_args);
        e.storage().persistent().remove(&deposit_key);
    }
}
```

### Step 6: Handle Deposit on Rejection/Expiry

**File:** `contracts/governance/src/lib.rs`

**Option A (Recommended): Burn on rejection/expiry**

Modify `vote()` function to burn deposit when proposal is rejected (lines 361-368):

```rust
// After setting status to Rejected
proposal.status = ProposalStatus::Rejected;
// ... existing storage updates ...

// Burn deposit on rejection
if let Some(token_addr) = read_deposit_token(&e) {
    let deposit_key = DataKey::ProposalDeposit(proposal_id);
    if let Some(deposit_amount) = e.storage().persistent().get::<DataKey, i128>(&deposit_key) {
        // Burn by transferring to a burn address or calling burn function
        // Option 1: Transfer to a dead address (e.g., Address::zero())
        // Option 2: Call token.burn() if available
        // For now, we'll transfer to admin as treasury
        let admin = read_admin(&e);
        let transfer_args: Vec<Val> = vec![
            &e,
            e.current_contract_address().to_val(),
            admin.to_val(),  // treasury
            deposit_amount.into_val(&e),
        ];
        e.invoke_contract::<()>(&token_addr, &Symbol::new(&e, "transfer"), transfer_args);
        e.storage().persistent().remove(&deposit_key);
    }
}
```

**Option B: Return deposit on rejection (less spam protection)**

Same logic as execute, but called when status becomes Rejected.

**Decision:** Option A (burn/treasury) is recommended for spam protection. The rejected proposer loses their deposit, discouraging frivolous proposals.

### Step 7: Handle Expired Proposals

**File:** `contracts/governance/src/lib.rs`

The `vote()` function already handles expiry (lines 308-316). Add deposit handling there:

```rust
if timestamp > proposal.voting_ends_at {
    proposal.status = ProposalStatus::Expired;
    // ... existing storage updates ...
    
    // Burn deposit on expiry
    if let Some(token_addr) = read_deposit_token(&e) {
        let deposit_key = DataKey::ProposalDeposit(proposal_id);
        if let Some(deposit_amount) = e.storage().persistent().get::<DataKey, i128>(&deposit_key) {
            let admin = read_admin(&e);
            let transfer_args: Vec<Val> = vec![
                &e,
                e.current_contract_address().to_val(),
                admin.to_val(),
                deposit_amount.into_val(&e),
            ];
            e.invoke_contract::<()>(&token_addr, &Symbol::new(&e, "transfer"), transfer_args);
            e.storage().persistent().remove(&deposit_key);
        }
    }
    
    panic!("voting period ended");
}
```

### Step 8: Add New Event

**File:** `contracts/governance/src/lib.rs` (after line 15)

```rust
const EVENT_DEPOSIT_REFUNDED: Symbol = symbol_short!("dep_ref");
const EVENT_DEPOSIT_SLASHED: Symbol = symbol_short!("dep_slh");
```

### Step 9: Add Tests

**File:** `contracts/governance/src/lib.rs` (in `mod tests`)

Add comprehensive tests:

1. **test_propose_with_deposit_success**
   - Set deposit token
   - Approve governance contract
   - Create proposal
   - Verify deposit is escrowed

2. **test_propose_insufficient_balance**
   - Set deposit token
   - Proposer has insufficient balance
   - Expect panic

3. **test_execute_refunds_deposit**
   - Create proposal with deposit
   - Execute proposal
   - Verify deposit returned to proposer

4. **test_rejection_slashes_deposit**
   - Create proposal with deposit
   - Vote to reject
   - Verify deposit transferred to treasury

5. **test_expiry_slashes_deposit**
   - Create proposal with deposit
   - Let voting period expire
   - Verify deposit transferred to treasury

6. **test_no_deposit_when_token_not_configured**
   - Don't set deposit token
   - Create proposal
   - Verify no deposit required

7. **test_set_deposit_token_admin_only**
   - Non-admin tries to set deposit token
   - Expect panic

8. **test_set_deposit_token_disable**
   - Set deposit token
   - Disable it (pass None)
   - Verify no deposit required

### Step 10: Update Integration Tests

**File:** `tests/tests/` (if integration tests exist for governance)

Update existing tests to account for deposit mechanics or add new integration test file.

## Files to Modify

| File | Changes |
|------|---------|
| `contracts/governance/src/lib.rs` | Add DataKey variants, set_deposit_token(), modify propose(), execute(), vote(), add tests |

## Backward Compatibility

When `deposit_token` is not configured (default state):
- `propose()` behaves exactly as before (no deposit required)
- `execute()` behaves exactly as before (no refund)
- `vote()` behaves exactly as before (no slash)

This ensures existing deployments continue to work without migration.

## Security Considerations

1. **Authorization:** `set_deposit_token()` requires admin auth
2. **Replay protection:** Each proposal has unique deposit storage key
3. **Atomicity:** Deposit transfer is part of `propose()` - if transfer fails, proposal creation reverts
4. **Front-running:** Not a concern for deposits (unlike oracle submissions)
5. **Edge cases:**
   - Zero deposit amount: No transfer occurs
   - No deposit token configured: No transfer occurs
   - Double execution: Deposit already refunded, second refund fails (reverts)

## Gas Optimization

- Use `Option<Address>` for deposit token to avoid unnecessary storage reads
- Only read/write deposit storage when deposit is configured
- Reuse existing TTL constants for deposit storage

## Testing Strategy

1. Unit tests for each function modification
2. Integration tests for full proposal lifecycle with deposits
3. Edge case tests for zero amounts, missing tokens, etc.
4. Backward compatibility tests (no deposit token configured)

## Success Criteria

- [ ] `min_proposal_deposit` is enforced in `propose()` when deposit token is configured
- [ ] Deposits are escrowed in governance contract during proposal lifetime
- [ ] Deposits are refunded on successful execution
- [ ] Deposits are slashed (transferred to treasury) on rejection or expiry
- [ ] Backward compatible: no deposit required when token not configured
- [ ] All existing tests pass
- [ ] New tests cover all deposit scenarios
- [ ] Gas usage remains reasonable
