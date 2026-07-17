# Security Policy

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| latest  | ✅                 |
| older   | ❌                 |

## Reporting a Vulnerability

**Do not report security vulnerabilities in public GitHub issues.**

Please report security issues via email to **[ogazipromise81@gmail.com](mailto:ogazipromise81@gmail.com)**. You can also reach the maintainer on Telegram at [@Escelit](https://t.me/Escelit). You should receive a response within 48 hours.

### What to Include

- Description of the vulnerability
- Steps to reproduce
- Affected contracts/functions
- Potential impact
- Any suggested fix (optional)

### Process

1. Your report will be acknowledged within 48 hours.
2. We will investigate and determine severity.
3. A fix will be prepared and tested.
4. A security advisory will be published once the fix is released.

### Scope

Smart contracts are the primary concern. Off-chain tooling and scripts are lower priority.

### Bug Bounty

A formal bug bounty program is in development. In the interim, significant vulnerabilities will be acknowledged in release notes at the reporter's discretion.

---

## Emergency Response Procedure

This section describes how protocol operators respond to a security incident that requires an immediate halt of token operations (e.g., a compromised oracle, a minting exploit, or an abnormal on-chain pattern).

### Overview

The `governance` contract is the protocol's emergency coordinator. It maintains:

- A registry of `credit_token` contract addresses (`registered_tokens`).
- A `ProtocolPaused` flag tracking the current pause state.

Each `credit_token` can be configured with a **pause guardian** — a secondary address (the governance contract) that may call `pause()` and `unpause()` without holding full admin rights over the token.

Two paths exist to trigger an emergency pause:

| Path | Who Can Trigger | Speed | Use Case |
|---|---|---|---|
| **Admin direct** (`emergency_pause`) | Governance admin key | Immediate | Confirmed exploit or active emergency |
| **Supermajority proposal** | All governance members (≥ 60 % votes) | After voting period + timelock | Contentious or planned maintenance pause |

---

### Setup (One-Time, Per Token)

Before governance can pause a token, two one-time setup steps are required:

**1. Grant the governance contract pause-guardian rights on the token:**

```bash
# token_admin must be the current token admin key
soroban contract invoke \
  --id $TOKEN_CONTRACT_ID \
  --fn set_pause_guardian \
  --arg admin:$TOKEN_ADMIN_KEY \
  --arg guardian:$GOVERNANCE_CONTRACT_ID \
  --network testnet
```

**2. Register the token with governance:**

```bash
soroban contract invoke \
  --id $GOVERNANCE_CONTRACT_ID \
  --fn register_token \
  --arg admin:$GOVERNANCE_ADMIN_KEY \
  --arg token:$TOKEN_CONTRACT_ID \
  --network testnet
```

Repeat step 2 for every credit token that should be covered by protocol-wide pauses. The `register_token` call is idempotent — registering the same address multiple times has no effect.

---

### Path 1 — Immediate Admin Pause

Use this when a confirmed exploit or active emergency requires an instant halt.

**Requirements:** The governance admin key must be available (hardware wallet or HSM recommended).

```bash
# 1. Pause all registered tokens immediately.
soroban contract invoke \
  --id $GOVERNANCE_CONTRACT_ID \
  --fn emergency_pause \
  --arg admin:$GOVERNANCE_ADMIN_KEY \
  --network mainnet

# 2. Verify the pause state.
soroban contract invoke \
  --id $GOVERNANCE_CONTRACT_ID \
  --fn is_protocol_paused \
  --network mainnet
# Expected: true

# 3. Verify a specific token is paused.
soroban contract invoke \
  --id $TOKEN_CONTRACT_ID \
  --fn paused \
  --network mainnet
# Expected: true
```

**After remediation — resume operations:**

```bash
soroban contract invoke \
  --id $GOVERNANCE_CONTRACT_ID \
  --fn emergency_unpause \
  --arg admin:$GOVERNANCE_ADMIN_KEY \
  --network mainnet
```

---

### Path 2 — Supermajority Governance Proposal

Use this when consensus is required (non-imminent threat, contested pause, or policy-driven halt).

**Step 1 — Create the emergency pause proposal**

Any governance member can propose. The proposal action `function` field must be exactly `"emergency_pause"`:

```bash
soroban contract invoke \
  --id $GOVERNANCE_CONTRACT_ID \
  --fn propose \
  --arg proposer:$MEMBER_KEY \
  --arg title:"Emergency pause: oracle $ORACLE_ID compromised" \
  --arg description:"Sensor data anomalies detected. Pausing token operations pending investigation." \
  --arg 'actions:[{"target":"'$GOVERNANCE_CONTRACT_ID'","function":"emergency_pause","args":[]}]' \
  --network mainnet
```

Note the returned `proposal_id` — you will need it for subsequent steps.

**Step 2 — Members vote**

Each governance member votes for or against:

```bash
soroban contract invoke \
  --id $GOVERNANCE_CONTRACT_ID \
  --fn vote \
  --arg voter:$MEMBER_KEY \
  --arg proposal_id:$PROPOSAL_ID \
  --arg approve:true \
  --network mainnet
```

The proposal is resolved (Approved/Rejected) once the number of cast votes reaches the **quorum** — `quorum_bps` (default 50%) of the eligible-voter count snapshotted at proposal creation. The approval threshold `votes_for / (votes_for + votes_against) ≥ 60 %` (configurable via `approval_threshold_bps`) is then measured over the cast votes. Because the quorum denominator is frozen at creation time, adding or removing members after a proposal exists does not change its threshold, fixing the prior bug where proposals could get stuck in `Active` forever. A vote cast after a proposal is already resolved is a harmless no-op.

**Step 3 — Wait for the timelock, then execute**

After the timelock elapses (default: 86 400 seconds = 24 hours), any member can execute:

```bash
soroban contract invoke \
  --id $GOVERNANCE_CONTRACT_ID \
  --fn execute \
  --arg caller:$MEMBER_KEY \
  --arg proposal_id:$PROPOSAL_ID \
  --network mainnet
```

Execution calls `pause()` on every registered token and sets `ProtocolPaused = true`.

**Step 4 — Unpause via a second proposal**

Once the incident is resolved, create a proposal with `function: "emergency_unpause"` and follow the same vote → timelock → execute flow.

---

### Verifying Pause State

```bash
# Check protocol-level pause state in governance.
soroban contract invoke \
  --id $GOVERNANCE_CONTRACT_ID \
  --fn is_protocol_paused \
  --network mainnet

# Check an individual token.
soroban contract invoke \
  --id $TOKEN_CONTRACT_ID \
  --fn paused \
  --network mainnet

# List all registered tokens.
soroban contract invoke \
  --id $GOVERNANCE_CONTRACT_ID \
  --fn list_registered_tokens \
  --network mainnet
```

---

### Effects of a Pause

When a token is paused, the following operations are blocked and will panic:

| Operation | Blocked? |
|---|---|
| `mint_to` | ✅ Yes |
| `batch_mint_to` | ✅ Yes |
| `transfer` | ✅ Yes |
| `batch_transfer` | ✅ Yes |
| `transfer_from` | ✅ Yes |
| `retire` | ✅ Yes |
| `burn` (admin-only) | ❌ No — admin can still burn during a pause |
| Read-only queries (`balance`, `total_supply`, etc.) | ❌ No — reads are never blocked |

---

### Adding or Removing Registered Tokens

To add a newly deployed credit token to the governance registry:

```bash
soroban contract invoke \
  --id $GOVERNANCE_CONTRACT_ID \
  --fn register_token \
  --arg admin:$GOVERNANCE_ADMIN_KEY \
  --arg token:$NEW_TOKEN_CONTRACT_ID \
  --network mainnet
```

To remove a token (e.g., a retired project that no longer needs protocol-wide coverage):

```bash
soroban contract invoke \
  --id $GOVERNANCE_CONTRACT_ID \
  --fn deregister_token \
  --arg admin:$GOVERNANCE_ADMIN_KEY \
  --arg token:$OLD_TOKEN_CONTRACT_ID \
  --network mainnet
```

---

### Incident Response Checklist

When a potential security incident is detected:

- [ ] Assess severity: is this an active exploit requiring an immediate pause?
- [ ] Notify all governance members and the security contact via secure channel.
- [ ] If critical: trigger **Path 1** (admin direct pause) immediately.
- [ ] If non-critical: initiate **Path 2** (supermajority proposal).
- [ ] Investigate root cause (off-chain oracle logs, on-chain events, sensor data).
- [ ] Communicate transparently with the community (status page, Discord/Telegram).
- [ ] Prepare and test the fix in a staging environment.
- [ ] Submit a separate governance proposal to apply the fix (contract upgrade or parameter change).
- [ ] Once the fix is live and validated, trigger emergency_unpause via the appropriate path.
- [ ] Publish a post-mortem within 7 days.

---

### Key Contract Addresses

> Fill these in during deployment and keep this section up to date.

| Contract | Network | Address |
|---|---|---|
| `governance` | Testnet | `CXXX...` |
| `governance` | Mainnet | `CXXX...` |
| `credit_token` (project A) | Mainnet | `CXXX...` |

---

### Contact

Security issues: email `security@[your-domain]` (replace with real address before deploying to mainnet).
