# Water Credits Smart Contracts — Formal Specification

## 1. Overview

Six Soroban smart contracts implement the on-chain logic for the Water Quality &
Replenishment Credits protocol. Each contract handles a distinct responsibility
and communicates with the others via cross-contract calls.

| Contract | Responsibility |
|---|---|
| `credit_token` | Per-project fungible credit asset |
| `credit_factory` | Deploys and indexes project credit tokens |
| `verification_oracle` | Aggregates sensor readings, computes and mints credits |
| `retirement_registry` | Immutable global retirement ledger |
| `project_registry` | On-chain project metadata directory |
| `governance` | Protocol parameters and multisig DAO |

---

## 2. Contract Specifications

### 2.1 credit_token

Each water restoration project has its own `credit_token` instance deployed by
the factory. Credits are transferable and retirable.

#### Public interface

| Function | Auth | Description |
|---|---|---|
| `initialize(admin, name, symbol, project_id, methodology)` | None (once) | Set up token |
| `mint_to(minter, to, amount)` | minter or admin | Mint credits, respects MaxSupply cap |
| `batch_mint_to(minter, recipients, amounts)` | minter or admin | Mint to multiple addresses atomically |
| `burn(admin, from, amount)` | admin | Destroy credits without retirement record |
| `transfer(from, to, amount)` | from | Move credits between wallets |
| `transfer_from(spender, from, to, amount)` | spender | Move credits via allowance |
| `approve(from, spender, amount, expiration_ledger)` | from | Grant allowance |
| `retire(holder, amount, purpose, metadata_uri)` | holder | Permanently retire credits → certificate |
| `set_admin(admin, new_admin)` | admin | Rotate admin key |
| `set_minter(admin, minter)` | admin | Delegate minting authority |
| `set_retirement_registry(admin, registry)` | admin | Link global retirement ledger |
| `set_max_supply(admin, max)` | admin | Set per-project credit ceiling (0 = uncapped) |
| `pause(admin)` | admin | Halt all mutable operations |
| `unpause(admin)` | admin | Resume operations |
| `balance(addr)` | — | Query balance |
| `total_supply()` | — | Current circulating credits: ever minted minus burned and retired |
| `total_retired()` | — | Total credits permanently retired |
| `total_burned()` | — | Total credits destroyed via admin `burn()` (no retirement record) |
| `max_supply()` | — | Current supply ceiling |
| `paused()` | — | Whether the contract is paused |
| `allowance(from, spender)` | — | Current approved amount |
| `name()` / `symbol()` / `decimals()` | — | Token metadata |
| `metadata()` | — | Project credit metadata |
| `get_certificate(index)` | — | Retrieve retirement certificate |

#### Pause semantics

While paused, `mint_to`, `batch_mint_to`, `transfer`, `transfer_from`, and
`retire` all panic with `"contract is paused"`. Read-only queries remain
available. The pause does not persist across upgrades — re-initialization would
clear it.

#### Supply cap semantics

`set_max_supply(admin, max)` sets the ceiling. `max = 0` means uncapped. Both
`mint_to` and `batch_mint_to` check `total_supply + amount > max` **before**
writing any state, so partial batch mints cannot occur.

---

### 2.2 verification_oracle

The oracle contract collects sensor readings from whitelisted oracle nodes,
aggregates them using median statistics, computes credit-equivalent impact,
and optionally triggers an auto-mint to the project beneficiary.

#### Oracle window lifecycle: commit-reveal

Readings are submitted through a **commit-reveal scheme**. This is the only
submission path — there is no plaintext single-call entry point. A well
capitalized actor watching the mempool who could see plaintext readings before
they land could observe two honest submissions and frontrun the third with an
outlier value chosen to shift the median, manufacturing credits that don't
correspond to real sensor data. Commit-reveal removes that window: every
committed value is opaque (a hash) until the reveal phase, by which point the
committing oracle can no longer react to what others submitted.

A **window** is a single aggregation round for one project and moves through
three phases:

```
  COMMIT                      REVEAL                        FINALIZED
┌──────────────────────┐    ┌───────────────────────────┐  ┌───────────────────────────┐
│  WindowState          │    │  WindowState                │  │  WindowState                │
│  phase: Commit        │ →  │  phase: Reveal              │→ │  phase: Finalized           │
│  submissions: []      │    │  submissions: [s1,s2,...]  │  │  submissions: [s1,s2,s3]  │
│  finalized: false     │    │  finalized: false           │  │  finalized: true           │
└──────────────────────┘    └───────────────────────────┘  └───────────────────────────┘
       ↑                              ↑                               ↑
  admin: open_window            oracle: commit_reading         len(submissions) >= min_oracles
                                 (stores SHA-256 hash only)     → compute median → emit event
                                                                 → store LastResult
                                                                 → optional auto-mint
```

State transitions:

1. **No window** — `get_last_result` returns `None`, `window_submission_count`
   returns 0, `get_window_phase` returns `None`.
2. **Open (Commit phase)** — Admin calls `open_window(admin, project_id)`.
   Fails if a non-finalized window is already active for that project.
3. **Commit** — Each whitelisted oracle calls
   `commit_reading(oracle, project_id, nonce, commitment)`, storing the
   32-byte SHA-256 commitment under `DataKey::Commitment(project_id, oracle)`
   (see "Commitment encoding" below). Requires `min_stake` (if configured),
   an unused nonce for `(project_id, oracle)`, the window to be in the Commit
   phase, and the oracle to not have already committed this window.
4. **Begin reveal** — Once `commit_phase_secs` have elapsed since the window
   opened, anyone can call `begin_reveal_phase(project_id)`. This transitions
   `phase` to `Reveal` and records the current ledger sequence number as
   `reveal_opened_ledger` — the anchor for the ledger-denominated reveal
   window below.
5. **Reveal** — Each committed oracle calls
   `reveal_reading(oracle, project_id, params)` with the plaintext reading,
   nonce, and secret. The contract recomputes the commitment and panics with
   `"hash mismatch: revealed values do not match commitment"` if it doesn't
   match the one stored at commit time. The reveal is only accepted while
   `reveal_opened_ledger + min_reveal_ledgers <= current_ledger <=
   reveal_opened_ledger + max_reveal_ledgers`; outside that window the call
   panics (`"reveal submitted before the reveal window opened"` or
   `"reveal window has closed"`) — this check runs inside `reveal_reading`
   itself, so a late reveal is rejected immediately, not just once someone
   later calls `finalize_window`. Each accepted reveal increments
   `OracleSubmitCount`/`TotalSubmissions` and is appended to
   `window.submissions`.
6. **Finalized window** — As soon as `submissions.len() >= config.min_oracles`
   (checked automatically at the end of each `reveal_reading`, and also by an
   explicit `finalize_window(project_id)` call once
   `max_reveal_ledgers` has elapsed), the contract computes median sensor
   values, evaluates the credit formula, stores a `VerificationResult` under
   `LastResult(project_id)`, marks `finalized = true` and `phase = Finalized`,
   emits a `("rdng_vrfy",)` event, and clears the `Commitment`/`OracleRevealed`
   markers for every whitelisted oracle. `finalize_window` additionally
   penalizes (see "Missed reveals" below) any oracle that committed but never
   revealed. Further calls against a finalized window panic with
   `"window already finalized"`.
7. **Reset** — Admin calls `reset_window(admin, project_id)`. Any pending
   `Commitment`/`OracleRevealed` entries for every whitelisted oracle are
   cleared (an oracle may have committed without revealing when the reset
   happens), and a fresh empty `WindowState` replaces the old one, back in the
   `Commit` phase. Oracle nonces are **not** reset. `reset_window` works on a
   window in any non-finalized phase (Commit or Reveal).

#### Commitment encoding

`commitment = SHA-256(nonce || ph || turbidity || dissolved_oxygen ||
flow_rate || temperature || total_nitrogen || total_phosphorus || secret)`,
computed by the exported `sha256_commitment(...)` function (also usable
directly by off-chain oracle node software and tests, so nobody has to
reimplement the byte layout). Byte layout:

| Field | Width | Encoding |
|---|---|---|
| `nonce` | 8 bytes | `u64`, big-endian |
| `ph` | 8 bytes | `i64`, big-endian |
| `turbidity` | 8 bytes | `i64`, big-endian |
| `dissolved_oxygen` | 8 bytes | `i64`, big-endian |
| `flow_rate` | 8 bytes | `i64`, big-endian |
| `temperature` | 8 bytes | `i64`, big-endian |
| `total_nitrogen` | 8 bytes | `i64`, big-endian |
| `total_phosphorus` | 8 bytes | `i64`, big-endian |
| `secret` | 32 bytes | raw `BytesN<32>` (called `salt` in the contract's `RevealParams`/`CommitInfo` types — same concept as "secret" in the commit-reveal literature) |

Fields are concatenated with no separators or padding — each is a
fixed-width, big-endian integer (via `to_be_bytes()`), so the encoding is
unambiguous and collision-resistant: no two distinct `(nonce, ph, ...,
secret)` tuples produce the same byte string. Sensor values use the same
fixed-point scale factors as everywhere else in the contract (see MATH.md).
The oracle picks `secret` itself (32 random bytes) and must remember it until
the reveal — losing it means the commitment can never be revealed and the
oracle's stake is slashed for a missed reveal once `max_reveal_ledgers`
passes.

#### Missed reveals

If `finalize_window` runs and an oracle's `Commitment` entry exists but its
`OracleRevealed` entry does not, the oracle is charged a missed reveal:
`OracleMissedReveals(oracle)` is incremented, up to `min_stake` is slashed
from its stake to the treasury, a `SlashReason { reason: 3, .. }` record is
stored, an `("orc_mr",)` event is emitted, and the stale commitment is
removed from storage. There is no separate grace period — a commitment not
revealed within `max_reveal_ledgers` is simply forfeited the next time
`finalize_window` runs (whether or not `min_oracles` reveals were reached).

#### Nonce replay protection

Each (project, oracle) pair has a monotonically-increasing nonce stored under
`OracleNonce(project_id, oracle)`. On each `commit_reading` call the contract
checks `nonce == stored + 1` and records it immediately (not deferred to
reveal). `reveal_reading` cross-checks that its `params.nonce` matches the
nonce recorded at commit time, panicking with `"nonce mismatch with
commitment"` otherwise. This prevents replay of old readings. Nonces are
independent across projects — an oracle can use the same nonce for different
projects.

#### Submission statistics

The contract records:
- `OracleSubmitCount(oracle)` — total accepted reveals by this oracle.
- `TotalSubmissions` — global total across all oracles.

These are incremented on each accepted `reveal_reading` call, regardless of
whether the window finalizes.

#### Credit calculation (summary)

Given medians of all sensor fields across the `min_oracles` submissions:

```
N_removed = max(0, baseline_N - med_N) * med_flow * 3600 / 1_000_000   (kg)
P_removed = max(0, baseline_P - med_P) * med_flow * 3600 / 1_000_000   (kg)

quality_penalty = 0..8000 bps based on pH, turbidity, DO, temperature

volumetric_credit = med_flow * 100 / 1000

gross = N_removed * credit_per_kg_n + P_removed * credit_per_kg_p + volumetric_credit
total = gross * (10_000 - quality_penalty) / 10_000
```

All sensor values are fixed-point integers (see MATH.md for scale factors).

#### Oracle staking and slashing

Oracles must stake tokens as collateral before being whitelisted. Staked
tokens are held by the oracle contract and can be slashed by admin if the
oracle submits fraudulent readings.

**Staking lifecycle:**

1. **Stake** — Oracle calls `stake(amount)`, which pulls tokens from the
   oracle via `transfer_from` on the configured staking token contract.
   Stake accumulates across multiple calls. Any pending unstake request
   is cancelled.
2. **Unstake** — Oracle calls `unstake(amount)`. If the oracle is active,
   the remaining stake must stay at or above `min_stake`. A cooldown
   timer (`unstake_cooldown_secs`) begins.
3. **Claim** — After the cooldown elapses, the oracle calls
   `claim_unstake()` to receive the unstaked tokens back via `transfer`.

**Slashing:**

- Admin calls `slash(admin, oracle, amount, reason)` to penalize an oracle.
- Reason codes: `1` = admin flag, `2` = fraud proof.
- Slashed funds are transferred to the treasury address.
- Slashing does not auto-remove the oracle from the whitelist; admin can
  separately call `remove_oracle`.

**Enforcement points:**

- `add_oracle` requires `stake >= min_stake` when `min_stake > 0`.
- `remove_oracle` requires `stake == 0` (oracle must unstake first).
- `commit_reading` requires `stake >= min_stake` when `min_stake > 0`.
- `reveal_reading` requires `stake >= min_stake` when `min_stake > 0`.

#### Public interface additions (this version)

| Function | Auth | Description |
|---|---|---|
| `initialize(admin, staking_token, treasury)` | None (once) | Set up oracle contract with staking token and treasury |
| `open_window(admin, project_id)` | admin | Open a new commit-reveal window (Commit phase) |
| `commit_reading(oracle, project_id, nonce, commitment)` | oracle | Store a SHA-256 commitment during the Commit phase |
| `begin_reveal_phase(project_id)` | anyone | Transition Commit → Reveal once `commit_phase_secs` has elapsed |
| `reveal_reading(oracle, project_id, params)` | oracle | Reveal the plaintext reading; verified against the stored commitment |
| `finalize_window(project_id)` | anyone | Finalize after `max_reveal_ledgers`, penalizing non-revealers |
| `get_window_phase(project_id)` | — | Current phase (`Commit`/`Reveal`/`Finalized`) of a project's window |
| `reset_window(admin, project_id)` | admin | Clear pending commitments/reveals so oracles can restart the round |
| `window_submission_count(project_id)` | — | Current accepted-reveal count in the open window |
| `oracle_submit_count(oracle)` | — | Lifetime accepted-reveal count for an oracle |
| `total_submissions()` | — | Global lifetime accepted-reveal count |
| `oracle_missed_reveals(oracle)` | — | Lifetime missed-reveal count for an oracle |
| `stake(oracle, amount)` | oracle | Lock tokens as collateral |
| `unstake(oracle, amount)` | oracle | Begin cooldown withdrawal |
| `claim_unstake(oracle)` | oracle | Withdraw tokens after cooldown |
| `slash(caller, oracle, amount, reason)` | admin | Penalize oracle, send funds to treasury |
| `get_stake(oracle)` | — | Current stake info (amount, unstake request) |
| `get_slash_record(oracle)` | — | Most recent slash record |
| `get_unstake_cooldown()` | — | Cooldown period in seconds |
| `get_treasury()` | — | Treasury address |
| `get_staking_token()` | — | Staking token contract address |

---

### 2.3 retirement_registry

Immutable append-only ledger of all credit retirements across all projects.

#### Indexes

Records are indexed by two secondary indexes for efficient retrieval:

- `RetireeRecords(Address)` → `Vec<u64>` of record IDs for a given retiree.
- `ProjectRecords(BytesN<32>)` → `Vec<u64>` of record IDs for a given project.

Both indexes are updated atomically with the record write in
`record_retirement`.

#### Public interface

| Function | Auth | Description |
|---|---|---|
| `initialize(admin)` | None (once) | Set up registry |
| `record_retirement(caller, retiree, project_id, amount, purpose, metadata_uri)` | admin or authorized | Append record, update indexes |
| `set_authorized_caller(admin, caller, authorized)` | admin | Whitelist a contract address |
| `get_record(id)` | — | Fetch record by sequential ID |
| `total_retired()` | — | Global sum of retired credits |
| `record_count()` | — | Total number of records |
| `get_retirements_by_retiree(retiree)` | — | All records for an address |
| `get_retirements_by_project(project_id)` | — | All records for a project |

---

### 2.4 project_registry

On-chain metadata directory. Projects are registered by the admin and can be
queried or listed by any caller.

#### Public interface additions (this version)

| Function | Auth | Description |
|---|---|---|
| `update_owner(caller, project_id, new_owner)` | admin or current owner | Transfer project ownership |

---

### 2.5 credit_factory

Deploys new `credit_token` instances and maintains a project index.

#### Public interface additions (this version)

| Function | Auth | Description |
|---|---|---|
| `update_project_owner(caller, project_id, new_owner)` | admin or current owner | Transfer project ownership in factory index |

---

### 2.6 governance

DAO for protocol parameter management. Members propose, vote, and execute
changes after a timelock. Voting is majority-based with a configurable
approval threshold.

**Proposal execution.** Each `Proposal` carries a list of `GovernanceAction`
entries (`target: Address`, `function: Symbol`, `args: Vec<Val>`). On
`execute()`, once the timelock has elapsed, `governance` dispatches each
action in order:

- `function == "emergency_pause"` / `"emergency_unpause"` are handled as
  built-in protocol actions (pause/unpause every token in `RegisteredTokens`);
  `target` is ignored for these.
- Any other `function` is invoked generically via `e.invoke_contract(target,
  function, args)`.

If any action panics, the whole `execute()` call reverts (standard Soroban
transaction semantics) and the proposal remains `Approved` for retry. The
proposal is only marked `Executed` after every action in the list succeeds —
the status write happens after the dispatch loop, not before it.

**Authorization: how governance acts as admin of other contracts.** A
generic action such as `verification_oracle::update_config(admin, config)`
requires `admin.require_auth()` to succeed for the target's stored admin
address. `governance` does not have a special bypass for this — instead it
relies on Soroban's invoker auto-authorization: when a contract calls
`require_auth()` on an address equal to *its own* contract address during a
call it initiated, that check passes without a separate signature. So the
delegation pattern is:

1. The target contract must expose a `transfer_admin(admin, new_admin)`
   function (see `verification_oracle::transfer_admin`).
2. The existing admin calls `transfer_admin(admin, <governance_contract_address>)`,
   making the governance contract itself the target's admin.
3. From then on, any `GovernanceAction` whose `args` include the governance
   contract's own address as the `admin` parameter will auto-authorize when
   dispatched from `execute()`.

This is opt-in per contract — a target only comes under DAO control once its
admin is explicitly transferred to the governance contract address. Contracts
that never transfer admin to governance remain solely under their original
admin's control.

---

## 3. Access Control Summary

| Role | Who | Capabilities |
|---|---|---|
| Admin | Contract deployer / multisig | Pause/unpause, set max supply, oracle whitelist, project status, config updates |
| Minter | Designated address (typically oracle) | `mint_to`, `batch_mint_to` |
| Oracle | Whitelisted oracle nodes | `submit_reading` |
| Project owner | Registered developer wallet | `update_owner` / `update_project_owner` |
| Credit holder | Any address with credits | `transfer`, `approve`, `retire` |
| Anyone | Public | All read-only queries |

---

## 4. Storage Layout Summary

### credit_token

| Key | Type | Notes |
|---|---|---|
| `Admin` | `Address` | Contract admin |
| `Minter` | `Address` | Optional minting delegate |
| `RetirementRegistry` | `Address` | Optional linked registry |
| `TotalSupply` | `i128` | Current circulating supply: ever minted minus burned and retired |
| `TotalRetired` | `i128` | Ever retired |
| `TotalBurned` | `i128` | Ever burned via admin `burn()` (initialized to 0) |
| `MaxSupply` | `i128` | 0 = uncapped |
| `Paused` | `bool` | Emergency halt flag |
| `Name` / `Symbol` / `Decimals` | string/u32 | Token metadata |
| `Metadata` | `CreditMetadata` | Project metadata at init |
| `Balance(Address)` | `i128` | Per-address balance |
| `Allowance(Address, Address)` | `i128` | Spender allowance |
| `Cert(u64)` | `RetirementCertificate` | Indexed certificates |
| `CertCount` | `u64` | Certificate counter |

### verification_oracle

| Key | Type | Notes |
|---|---|---|
| `Admin` | `Address` | Contract admin |
| `OracleActive(Address)` | `bool` | Whitelist entry |
| `OracleCount` | `u32` | Whitelist size |
| `Config` | `OracleConfig` | Protocol parameters |
| `OracleNonce(BytesN<32>, Address)` | `u64` | Last accepted nonce per (project, oracle) |
| `OracleSubmitted(BytesN<32>, Address)` | `bool` | Dedup: oracle × window |
| `OracleSubmitCount(Address)` | `u64` | Lifetime submission count |
| `TotalSubmissions` | `u64` | Protocol-wide submission count |
| `WindowState(BytesN<32>)` | `WindowState` | Open/finalized window |
| `LastResult(BytesN<32>)` | `VerificationResult` | Latest finalized result |
| `ProjectConfig(BytesN<32>)` | `ProjectConfig` | Auto-mint config |
| `OracleStake(Address)` | `StakeInfo` | Oracle stake amount and unstake request |
| `OracleSlashed(Address)` | `SlashReason` | Most recent slash record |

### retirement_registry

| Key | Type | Notes |
|---|---|---|
| `Admin` | `Address` | Registry admin |
| `RecordCount` | `u64` | Total records |
| `TotalRetired` | `i128` | Global sum |
| `Record(u64)` | `RetirementRecord` | Record by ID |
| `RetireeRecords(Address)` | `Vec<u64>` | Index by retiree |
| `ProjectRecords(BytesN<32>)` | `Vec<u64>` | Index by project |
| `AuthorizedCaller(Address)` | `bool` | Authorized contract |

---

## 5. Invariants

The following properties must hold at all times:

1. **Supply conservation**: `total_supply + total_retired + total_burned == ever_minted`
   where `ever_minted` is the cumulative sum of all `mint_to` / `batch_mint_to` calls,
   `total_retired` counts credits destroyed via `retire()` (retirement record issued),
   and `total_burned` counts credits destroyed via admin `burn()` (no retirement record).
   Equivalently: `total_supply == ever_minted - total_retired - total_burned`.
2. **No over-mint**: `total_supply <= max_supply` (when max_supply > 0)
3. **Nonce monotonicity**: `OracleNonce[project_id, oracle]` never decreases
4. **Window finality**: A finalized window's `finalized = true` is never reverted
   (reset_window only operates on non-finalized windows)
5. **Retirement immutability**: Records in `retirement_registry` are
   append-only; no record is ever modified or deleted
6. **Deduplication**: An oracle cannot submit twice to the same open window
   for the same project
7. **Stake conservation**: The sum of all `OracleStake.amount` values plus
   all tokens held by the contract equals the total tokens transferred in
   via `stake` minus tokens transferred out via `unstake`/`slash`
8. **Min stake enforcement**: An active oracle always has
   `OracleStake.amount >= config.min_stake` (when `min_stake > 0`)
9. **Slash bounds**: `slash` panics if `amount > oracle.stake.amount`

---

## 6. Events

| Event topic | Contract | Payload | When |
|---|---|---|---|
| `minted` | `credit_token` | `(to, amount)` | Per mint (including batch) |
| `xfer` | `credit_token` | `(from, to, amount)` | Transfer |
| `retired` | `credit_token` | `(holder, amount, certificate)` | Retire |
| `burned` | `credit_token` | `(from, amount, total_burned)` | Admin burn; `total_burned` is the running accumulator after this operation |
| `proj_reg` | `credit_factory` | `(project_id,)` | Project registered |
| `rdng_vrfy` | `verification_oracle` | `(project_id, result)` | Window finalized |
| `orc_stk` | `verification_oracle` | `(oracle, amount)` | Oracle stakes tokens |
| `orc_unst` | `verification_oracle` | `(oracle, amount)` | Oracle requests unstake |
| `orc_slsh` | `verification_oracle` | `(oracle, amount, reason)` | Oracle stake slashed |
| `prop_crt` | `governance` | `(proposal_id, proposer)` | Proposal created |
| `vote_cst` | `governance` | `(proposal_id, voter, approve)` | Vote cast |
| `prop_exe` | `governance` | `(proposal_id,)` | Proposal executed |
| `memb_add` | `governance` | `(member,)` | Member added |
| `memb_rmv` | `governance` | `(member,)` | Member removed |
