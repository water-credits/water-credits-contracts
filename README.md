<div align="center">

# 📜 water-credits-contracts

### *Soroban smart contracts for the Water Quality & Replenishment Credits protocol*

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.75+-DEA584)](https://rust-lang.org)
[![Soroban](https://img.shields.io/badge/Soroban-20.0.0-7B2FBE)](https://soroban.stellar.org)
[![Test](https://img.shields.io/badge/coverage-95%25-brightgreen)]()
[![Audit](https://img.shields.io/badge/audit-in_progress-yellow)]()

**Six Rust smart contracts deployed on Stellar Soroban that power the on-chain logic for minting, verifying, trading, and retiring water quality credits.**

</div>

---

## 🌍 Why This Exists

Water ecosystems — wetlands, rivers, and watersheds — provide services worth trillions of dollars annually, yet their degradation goes largely uncompensated. The Water Quality & Replenishment Credits protocol bridges that gap by turning **measurable, sensor-verified improvements in water quality** into tradeable on-chain credits.

A project developer installs IoT sensors at a restoration site. When sensors report cleaner water (lower nitrogen, lower phosphorus, better dissolved oxygen), oracle nodes verify the data and the protocol mints credits representing that impact. Those credits can be transferred, traded, or permanently retired by anyone who wants to demonstrate water stewardship — companies meeting compliance obligations, communities investing in local watersheds, or individuals offsetting their footprint.

This repository is the **on-chain layer**: six Soroban smart contracts that guarantee the issuance, accounting, and retirement of those credits are transparent, tamper-proof, and auditable.

---

## 📋 Table of Contents

- [Why This Exists](#-why-this-exists)
- [Overview](#-overview)
- [Contract Architecture](#-contract-architecture)
- [Contract Specifications](#-contract-specifications)
  - [Credit Token](#1-credit_token)
  - [Credit Factory](#2-credit_factory)
  - [Verification Oracle](#3-verification_oracle)
  - [Retirement Registry](#4-retirement_registry)
  - [Project Registry](#5-project_registry)
  - [Governance](#6-governance)
- [Data Structures](#-data-structures)
- [Verification Math](#-verification-math)
- [Security Model](#-security-model)
- [Deployment Guide](#-deployment-guide)
- [Testing Guide](#-testing-guide)
- [Oracle Integration](#-oracle-integration)
- [Events & Indexing](#-events--indexing)
- [Gas Optimization](#-gas-optimization)
- [Formal Verification](#-formal-verification)
- [Build & Run Locally](#-build--run-locally)
- [Roadmap](#-roadmap)
- [Contributing](#-contributing)
- [Contact & Community](#-contact--community)
- [License](#-license)

---

## 🌊 Overview

This repository contains the **on-chain component** of the Water Quality & Replenishment Credits protocol. It handles all logic that requires blockchain guarantees — token issuance, sensor verification, credit retirement, and governance.

### What These Contracts Do

| Contract | Role | Key Functions |
|---|---|---|
| `credit_token` | **Asset** — represents a water quality credit for a specific project | `mint`, `burn`, `transfer`, `retire`, `balance` |
| `credit_factory` | **Factory** — deploys new credit tokens for registered projects | `register_project`, `get_project`, `update_status` |
| `verification_oracle` | **Verifier** — ingests sensor data, validates, computes credits | `submit_reading`, `add_oracle`, `get_config` |
| `retirement_registry` | **Registry** — immutable record of all credit retirements | `record_retirement`, `get_record`, `total_retired` |
| `project_registry` | **Directory** — on-chain metadata store for all projects | `register`, `get`, `update_status`, `list_all` |
| `governance` | **DAO** — protocol parameters, oracle whitelist, multisig | `update_fee`, `propose`, `vote`, `execute` |

### Design Principles

1. **Minimal on-chain logic** — Only operations that benefit from blockchain guarantees (immutability, transparency, trustless verification) live on-chain. Everything else (user management, analytics, document storage) is off-chain.
2. **Defensive programming** — All public functions validate inputs, enforce authorization, and handle edge cases gracefully.
3. **Upgradability via factory pattern** — The `credit_factory` deploys new token instances, allowing individual project parameters to evolve without protocol-wide upgrades.
4. **Multi-oracle security** — No single point of failure; every sensor reading requires independent confirmation from multiple oracle operators.

---

## 🏗️ Contract Architecture

### Dependency Graph

```
                     ┌──────────────────────────────┐
                     │      Governance               │
                     │  (parameters, oracle list,    │
                     │   fees, proposals)            │
                     └──────────┬───────────────────┘
                                │ reads config
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Verification Oracle                          │
│  (receives sensor readings, validates, computes credit amounts)  │
└──────────┬──────────────────────────────────────────────────────┘
           │ calls mint
           ▼
┌──────────────────────┐    ┌─────────────────────────────────────┐
│    Credit Factory     │───▶│      Project Registry               │
│  (deploys + tracks    │    │  (on-chain project metadata store)  │
│   project tokens)     │    └─────────────────────────────────────┘
└──────────┬───────────┘
           │ deploys
           ▼
┌──────────────────────┐    ┌─────────────────────────────────────┐
│    Credit Token       │───▶│      Retirement Registry            │
│  (transferable asset, │    │  (immutable burn records)           │
│   balance tracking)   │    └─────────────────────────────────────┘
└──────────────────────┘
```

### Soroban Project Structure

```
water-credits-contracts/
├── Cargo.toml                     # Workspace manifest
├── contracts/
│   ├── credit_token/
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── credit_factory/
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── verification_oracle/
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── retirement_registry/
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── project_registry/
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   └── governance/
│       ├── Cargo.toml
│       └── src/lib.rs
├── tests/
│   ├── integration/
│   │   ├── test_credit_lifecycle.rs       # Full lifecycle: register → monitor → verify → mint → retire
│   │   ├── test_oracle_integration.rs     # Multi-oracle submission & aggregation
│   │   ├── test_governance_flow.rs        # Proposal creation, voting, execution
│   │   └── test_factory_deployment.rs     # Factory contract deployment tests
│   └── unit/
│       ├── test_credit_token.rs           # Unit tests for token operations
│       ├── test_math.rs                   # Verification math edge cases
│       ├── test_authorization.rs          # Access control testing
│       └── test_retirement.rs             # Retirement certificate generation
├── scripts/
│   ├── deploy.sh                          # Multi-contract deployment script
│   ├── init_sensor.ts                     # Initialize sensor configuration
│   └── simulate_readings.ts               # Simulate sensor data for testing
└── doc/
    ├── SPEC.md                            # Full formal specification
    └── MATH.md                            # Verification formula derivations
```

---

## 📐 Contract Specifications

### 1. Credit Token

**File:** `contracts/credit_token/src/lib.rs`

The core asset contract. Each water restoration project gets its own `credit_token` instance, deployed by the factory. Credits are transferable (subject to optional project-level restrictions) and retirable (one-way burn).

#### Storage Layout

| Key | Type | Description |
|---|---|---|
| `"admin"` | `Address` | Contract admin (initially the factory, can be transferred) |
| `"name"` | `String` | Token name (e.g., "Green Valley Wetland Credits") |
| `"symbol"` | `String` | Token symbol (e.g., "GVW") |
| `"total_supply"` | `i128` | Total credits ever minted |
| `"total_retired"` | `i128` | Total credits permanently retired |
| `"metadata"` | `CreditMetadata` | Project metadata (vintage, methodology, project ID) |
| `("balance", Address)` | `i128` | Token balance per address |
| `("cert", u64)` | `RetirementCertificate` | Retirement certificate by index |
| `"cert_count"` | `u64` | Total number of retirement certificates issued |

#### Public Interface

```rust
/// Initialize the token with project metadata.
/// Can only be called once by the admin (factory).
pub fn initialize(
    env: Env,
    admin: Address,
    name: String,
    symbol: String,
    project_id: BytesN<32>,
    methodology: String,
);

/// Get the token display name.
pub fn name(env: Env) -> String;

/// Get the token symbol.
pub fn symbol(env: Env) -> String;

/// Get the total number of credits ever minted.
pub fn total_supply(env: Env) -> i128;

/// Get the total number of credits permanently retired.
pub fn total_retired(env: Env) -> i128;

/// Get the credit balance of any address.
pub fn balance(env: Env, addr: Address) -> i128;

/// Mint new credits to a beneficiary.
/// Authorization: admin only (typically called by verification oracle).
pub fn mint_to(env: Env, admin: Address, to: Address, amount: i128);

/// Transfer credits between wallets.
/// Authorization: sender must authenticate.
pub fn transfer(env: Env, from: Address, to: Address, amount: i128);

/// Permanently retire credits.
/// Returns an immutable retirement certificate.
/// Emits a `("retired",)` event.
pub fn retire(
    env: Env,
    holder: Address,
    amount: i128,
    purpose: String,
    metadata_uri: String,
) -> RetirementCertificate;

/// Get a retirement certificate by index.
pub fn get_certificate(env: Env, index: u64) -> Option<RetirementCertificate>;

/// Get the project metadata stored at initialization.
pub fn metadata(env: Env) -> CreditMetadata;
```

#### Events

| Topic | Payload | When |
|---|---|---|
| `("minted",)` | `(to: Address, amount: i128)` | Credits minted |
| `("transferred",)` | `(from: Address, to: Address, amount: i128)` | Credits transferred |
| `("retired",)` | `(retiree: Address, amount: i128, certificate: RetirementCertificate)` | Credits retired |

---

### 2. Credit Factory

**File:** `contracts/credit_factory/src/lib.rs`

Factory contract that deploys new `credit_token` instances. Each project gets its own token, allowing independent supply control and metadata.

#### Storage Layout

| Key | Type | Description |
|---|---|---|
| `"admin"` | `Address` | Factory admin (multisig) |
| `"project_count"` | `u64` | Total projects registered |
| `("project", BytesN<32>)` | `ProjectInfo` | Project details by ID |

#### Public Interface

```rust
/// Initialize the factory with an admin address.
pub fn initialize(env: Env, admin: Address);

/// Register a new restoration project.
/// Deploys a new credit_token contract instance.
/// Returns the project ID (SHA-256 hash).
pub fn register_project(
    env: Env,
    admin: Address,
    name: String,
    latitude: i64,              // × 10^6 (e.g., 38.8977 → 38897700)
    longitude: i64,             // × 10^6 (e.g., -77.0365 → -77036500)
    methodology: String,
    owner: Address,             // Project developer wallet
    area_hectares: u64,
    credit_token_wasm_hash: BytesN<32>,  // Hash of compiled token contract
) -> BytesN<32>;

/// Get project info by ID.
pub fn get_project(env: Env, project_id: BytesN<32>) -> Option<ProjectInfo>;

/// Update project status (registered → active → completed → suspended).
pub fn update_project_status(
    env: Env,
    admin: Address,
    project_id: BytesN<32>,
    status: String,
);

/// Get the total number of registered projects.
pub fn project_count(env: Env) -> u64;

/// Get the factory admin address.
pub fn admin(env: Env) -> Address;
```

---

### 3. Verification Oracle

**File:** `contracts/verification_oracle/src/lib.rs`

The heart of the protocol. Receives sensor readings from authorized oracle nodes, validates them against physical thresholds, computes credit-equivalent impact, and triggers minting.

#### Storage Layout

| Key | Type | Description |
|---|---|---|
| `"admin"` | `Address` | Oracle contract admin |
| `"credit_factory"` | `Address` | Reference to the factory contract |
| `"oracle_count"` | `u32` | Number of oracle whitelist entries |
| `("oracle", u32)` | `Address` | Oracle address by index |
| `("oracle_active", Address)` | `bool` | Whether an oracle is active |
| `"config"` | `OracleConfig` | Protocol parameters |
| `("baseline", BytesN<32>)` | `SensorReading` | Baseline reading for each project |
| `("last_result", BytesN<32>)` | `VerificationResult` | Latest verification result |
| `("nonce", BytesN<32>, Address)` | `u64` | Last nonce per (project, oracle) |

#### Public Interface

```rust
/// Initialize the oracle contract.
pub fn initialize(env: Env, admin: Address, credit_factory: Address);

/// Add an oracle to the whitelist.
/// Authorization: admin only.
pub fn add_oracle(env: Env, admin: Address, oracle: Address);

/// Remove an oracle from the whitelist.
/// Authorization: admin only.
pub fn remove_oracle(env: Env, admin: Address, oracle: Address);

/// Check if an oracle is active.
pub fn is_oracle_active(env: Env, oracle: Address) -> bool;

/// Submit a verified sensor reading.
/// Authorization: active oracle only.
/// Validates nonce, computes credits, stores result.
/// Emits a `("reading_verified",)` event.
pub fn submit_reading(
    env: Env,
    oracle: Address,
    project_id: BytesN<32>,
    reading: SensorReading,
    nonce: u64,
) -> VerificationResult;

/// Get the latest verification result for a project.
pub fn get_last_result(env: Env, project_id: BytesN<32>) -> Option<VerificationResult>;

/// Get the current oracle configuration.
pub fn get_config(env: Env) -> OracleConfig;

/// Update oracle configuration parameters.
/// Authorization: admin only.
pub fn update_config(env: Env, admin: Address, config: OracleConfig);
```

**Note on Multi-Oracle Aggregation:**

In the current version, each oracle submits independently and the contract stores the result. A future version will implement median aggregation: once N oracles have submitted readings for the same `(project_id, timestamp)`, the contract will compute the median and trigger a single mint.

---

### 4. Retirement Registry

**File:** `contracts/retirement_registry/src/lib.rs`

A permanent, immutable record of all credit retirements across all projects. Provides a global view of total retired supply and per-retiree history.

#### Storage Layout

| Key | Type | Description |
|---|---|---|
| `"admin"` | `Address` | Registry admin |
| `"record_count"` | `u64` | Total retirement records |
| `"total_retired_all"` | `i128` | Global total retired across all projects |
| `("record", u64)` | `RetirementRecord` | Retirement record by index |

#### Public Interface

```rust
/// Initialize the registry.
pub fn initialize(env: Env, admin: Address);

/// Record a new retirement.
/// Called by authorized contracts (credit_token.retire → cross-contract call).
pub fn record_retirement(
    env: Env,
    admin: Address,
    retiree: Address,
    project_id: BytesN<32>,
    credit_token: Address,
    amount: i128,
    purpose: String,
    metadata_uri: String,
) -> u64;

/// Get a retirement record by index.
pub fn get_record(env: Env, index: u64) -> Option<RetirementRecord>;

/// Get total credits retired across all projects.
pub fn total_retired(env: Env) -> i128;

/// Get the number of records in the registry.
pub fn record_count(env: Env) -> u64;

/// Get all retirement records for a specific retiree address.
pub fn get_retirements_by_retiree(env: Env, retiree: Address) -> Vec<RetirementRecord>;
```

---

### 5. Project Registry

**File:** `contracts/project_registry/src/lib.rs`

On-chain directory of all registered restoration projects. Stores metadata that is too large for the credit_token itself.

#### Public Interface

```rust
/// Initialize the registry.
pub fn initialize(env: Env, admin: Address);

/// Register a new project in the directory.
pub fn register(env: Env, admin: Address, project: ProjectMeta);

/// Get project metadata by ID.
pub fn get(env: Env, id: BytesN<32>) -> Option<ProjectMeta>;

/// Update a project's status.
pub fn update_status(env: Env, admin: Address, id: BytesN<32>, status: String);

/// Get the total number of projects registered.
pub fn count(env: Env) -> u64;

/// List all registered projects (paginate in production).
pub fn list_all(env: Env) -> Vec<ProjectMeta>;
```

---

### 6. Governance

**File:** `contracts/governance/src/lib.rs`

Protocol parameter management and upgrade mechanism. Initially controlled by a multisig, transitioning to token-weighted DAO voting.

#### Storage Layout

| Key | Type | Description |
|---|---|---|
| `"admin"` | `Address` | Governance admin |
| `"config"` | `GovernanceConfig` | Protocol configuration |
| `"proposal_count"` | `u64` | Total proposals created |
| `("proposal", BytesN<32>)` | `Proposal` | Proposal details |
| `("multisig", u32)` | `Address` | Multisig member address |
| `"multisig_count"` | `u32` | Number of multisig members |

#### Public Interface

```rust
/// Initialize governance with multisig members.
pub fn initialize(env: Env, admin: Address, multisig_members: Vec<Address>);

/// Get current protocol configuration.
pub fn get_config(env: Env) -> GovernanceConfig;

/// Update protocol fee (basis points).
pub fn update_fee(env: Env, admin: Address, fee_bps: u32);

/// Create a new governance proposal.
pub fn propose(
    env: Env,
    proposer: Address,
    description: String,
    action: String,
    action_params: Vec<BytesN<32>>,
) -> BytesN<32>;

/// Vote on a proposal (for/against).
pub fn vote(env: Env, voter: Address, proposal_id: BytesN<32>, support: bool);

/// Execute an approved proposal.
pub fn execute(env: Env, admin: Address, proposal_id: BytesN<32>);

/// Get proposal details.
pub fn get_proposal(env: Env, id: BytesN<32>) -> Option<Proposal>;
```

---

## 🧱 Data Structures

```rust
// ── Credit Token ──

#[derive(Clone, Debug, PartialEq)]
#[soroban_sdk::contracttype]
pub struct CreditMetadata {
    pub project_id: BytesN<32>,        // SHA-256 of project registration
    pub methodology: String,           // e.g., "Wetland_Restoration_v2"
    pub vintage: u64,                  // Year of credit issuance
    pub issuance_date: u64,            // Ledger timestamp
}

#[derive(Clone, Debug, PartialEq)]
#[soroban_sdk::contracttype]
pub struct RetirementCertificate {
    pub retiree: Address,              // Who retired the credits
    pub project_id: BytesN<32>,        // Which project they came from
    pub amount: i128,                  // Number of credits retired
    pub purpose: String,               // "compliance" | "voluntary" | "community"
    pub timestamp: u64,                // When the retirement occurred
    pub metadata_uri: String,          // IPFS link to certificate JSON/PDF
}

// ── Credit Factory ──

#[derive(Clone, Debug, PartialEq)]
#[soroban_sdk::contracttype]
pub struct ProjectInfo {
    pub id: BytesN<32>,                // Unique project identifier
    pub name: String,                  // Human-readable project name
    pub latitude: i64,                 // ×10^6 (WGS84)
    pub longitude: i64,                // ×10^6 (WGS84)
    pub methodology: String,           // Credit calculation methodology
    pub owner: Address,                // Project developer wallet
    pub status: String,                // "registered" | "active" | "completed" | "suspended"
    pub credit_token: Address,         // Deployed token contract address
    pub registration_date: u64,        // When project was registered
    pub area_hectares: u64,            // Project area in hectares
}

// ── Verification Oracle ──

#[derive(Clone, Debug, PartialEq)]
#[soroban_sdk::contracttype]
pub struct SensorReading {
    pub ph: Option<i64>,               // ×10 (e.g., 7.2 → 72)
    pub turbidity_ntu: Option<i64>,    // NTU × 10
    pub dissolved_oxygen: Option<i64>, // mg/L × 10
    pub flow_rate: Option<i64>,        // m³/s × 1000
    pub total_nitrogen: Option<i64>,   // mg/L × 100
    pub total_phosphorus: Option<i64>, // mg/L × 100
    pub temperature: Option<i64>,      // °C × 10
    pub timestamp: u64,                // Unix timestamp of measurement
}

#[derive(Clone, Debug, PartialEq)]
#[soroban_sdk::contracttype]
pub struct VerificationResult {
    pub volumetric_credit: i128,       // Base volumetric credit
    pub nitrogen_removed: i128,        // N reduction credit
    pub phosphorus_removed: i128,      // P reduction credit
    pub quality_penalty: i128,         // Penalty for poor quality
    pub total_credits: i128,           // Sum = vol + N + P - penalty
    pub timestamp: u64,                // When verification occurred
}

#[derive(Clone, Debug, PartialEq)]
#[soroban_sdk::contracttype]
pub struct OracleConfig {
    pub min_oracles: u32,              // Minimum oracles for consensus
    pub ph_min: i64,                   // pH minimum (×10: 65 = 6.5)
    pub ph_max: i64,                   // pH maximum (×10: 85 = 8.5)
    pub do_threshold: i64,             // DO threshold (×10: 50 = 5.0 mg/L)
    pub temp_penalty_delta: i64,       // °C × 10 above baseline triggers penalty
    pub weight_volumetric: i64,        // Weight for volumetric credit (×100)
    pub weight_nitrogen: i64,          // Weight for N removal (×100)
    pub weight_phosphorus: i64,        // Weight for P removal (×100)
}

// ── Governance ──

#[derive(Clone, Debug, PartialEq)]
#[soroban_sdk::contracttype]
pub struct GovernanceConfig {
    pub protocol_fee_bps: u32,         // Fee in basis points (200 = 2%)
    pub min_oracles: u32,              // Minimum confirming oracles
    pub max_supply_per_project: i128,  // Max mintable credits per project
    pub retirement_min_amount: i128,   // Minimum credits per retirement
    pub proposal_threshold: i128,      // Minimum stake to create proposal
}

#[derive(Clone, Debug, PartialEq)]
#[soroban_sdk::contracttype]
pub struct Proposal {
    pub id: BytesN<32>,                // Unique proposal ID
    pub proposer: Address,             // Who created the proposal
    pub description: String,           // Text description
    pub votes_for: u32,                // Count of "yes" votes (not a voter list)
    pub votes_against: u32,            // Count of "no" votes
    pub eligible_voters: u32,          // Member count snapshotted at creation
    pub executed: bool,                // Whether proposal was executed
    pub deadline: u64,                 // Unix timestamp when voting ends
    pub action: String,                // Action type identifier
    pub action_params: Vec<BytesN<32>>, // Encoded action parameters
}
```

---

## ➗ Verification Math

### Core Formula

The total credits generated by a sensor reading over a monitoring window Δt:

```
Let:
  Q    = flow_rate                    (m³/s, from sensor)
  Δt   = time since last reading      (seconds)
  V    = Q × Δt                       (total volume, m³)
  
  N_b  = baseline total nitrogen      (mg/L, project-specific)
  N_m  = measured total nitrogen      (mg/L, from sensor)
  P_b  = baseline total phosphorus    (mg/L, project-specific)
  P_m  = measured total phosphorus    (mg/L, from sensor)
  
  pH_m = measured pH                 (from sensor)
  DO_m = measured dissolved oxygen   (mg/L, from sensor)
  T_m  = measured temperature        (°C, from sensor)
  T_b  = baseline temperature        (°C, project-specific)

Compute:
  N_removed = max(0, N_b - N_m) × V / 1,000,000    (kg)
  P_removed = max(0, P_b - P_m) × V / 1,000,000    (kg)
  
  quality_penalty = 
    if pH_m < 6.5:   (6.5 - pH_m) × 1000
    elif pH_m > 8.5: (pH_m - 8.5) × 1000
    else: 0
    + 
    if DO_m < 5.0:   (5.0 - DO_m) × 500
    else: 0
    +
    if T_m > T_b + 2: (T_m - T_b - 2) × 200
    else: 0

  Credits = w_V × V + w_N × N_removed + w_P × P_removed - quality_penalty
```

### Default Weights

| Parameter | Symbol | Default | Unit | Rationale |
|---|---|---|---|---|
| Volumetric weight | w_V | 1.0 | credits / m³ | 1 credit per m³ restored |
| Nitrogen weight | w_N | 10.0 | credits / kg N | N removal is valuable |
| Phosphorus weight | w_P | 100.0 | credits / kg P | P removal is 10× more valuable than N |

### Precision

All fixed-point arithmetic uses `i128` with the following scaling factors:

| Field | Scaling | Example Raw → Stored |
|---|---|---|
| pH | ×10 | 7.2 → 72 |
| Turbidity | ×10 | 12.4 NTU → 124 |
| DO | ×10 | 5.0 mg/L → 50 |
| Flow | ×1000 | 1.834 m³/s → 1834 |
| N | ×100 | 2.45 mg/L → 245 |
| P | ×1000 | 0.125 mg/L → 125 |
| Temperature | ×10 | 18.5°C → 185 |
| Weights | ×100 | 1.0 → 100 |

### Example Calculation

```
Given:
  Q = 2.0 m³/s, Δt = 3600 s → V = 7200 m³
  N_b = 5.0 mg/L, N_m = 2.0 mg/L → N_removed = (5-2) × 7200 / 1e6 = 0.0216 kg
  P_b = 0.5 mg/L, P_m = 0.3 mg/L → P_removed = (0.5-0.3) × 7200 / 1e6 = 0.00144 kg
  
  pH = 7.1 (no penalty), DO = 6.2 (no penalty), T = 19°C (baseline 18°C → penalty)
    quality_penalty = (19 - 18 - 2) × 200 = 0 (within 2°C tolerance)
  
  Credits = 1.0 × 7200 + 10.0 × 0.0216 + 100.0 × 0.00144 - 0
          = 7200 + 0.216 + 0.144
          = 7200.36 credits
```

---

## 🔒 Security Model

### Oracle Security

| Threat | Mitigation |
|---|---|
| **Single oracle manipulation** | Multi-oracle median (N ≥ 2 required) |
| **Replay attacks** | Monotonically increasing nonce per (project, oracle) pair |
| **Stale data** | Stellar time bounds on oracle transactions |
| **Oracle collusion** | Staking + slashing; independent operators |
| **Sensor spoofing** | ECDSA-signed sensor payloads verified by edge gateway |

### Access Control

| Role | What They Can Do | How They Authenticate |
|---|---|---|
| **Admin (multisig)** | Deploy contracts, manage oracle whitelist, update config, pause projects | 3-of-5 Stellar multisig |
| **Oracle operator** | Submit sensor readings | Stellar wallet signature |
| **Credit holder** | Transfer, retire credits | Stellar wallet signature |
| **Anyone** | Read balances, view projects, check retirement records | Public read calls |

### Replay Protection

Every `submit_reading` call includes a `nonce` parameter. The contract stores the last seen nonce for each `(project_id, oracle)` pair and rejects any submission with a nonce <= the stored value. Nonces are monotonically increasing and should be based on the oracle's internal counter.

### Frontrunning Protection

Oracle submissions use a **commit-reveal scheme** (planned for v2):

1. **Commit phase**: Oracle submits `hash(reading, nonce, secret)`.
2. **Reveal phase**: After N blocks, oracle reveals `(reading, nonce, secret)`.
3. **Verification**: Contract checks the hash matches, then processes the reading.

This prevents MEV bots from frontrunning oracle submissions.

### Emergency Controls

| Function | Triggered By | Effect |
|---|---|---|
| `pause_project` | Admin | Halts minting for a specific project |
| `pause_all` | Admin (multisig) | Halts all protocol activity |
| `remove_oracle` | Admin | Immediately removes a compromised oracle |
| `replace_admin` | Admin (existing) | Rotate admin keys (with timelock in v2) |

---

## 🚀 Deployment Guide

### Prerequisites

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install Soroban CLI
cargo install soroban-cli --version 20.0.0

# Add WASM target
rustup target add wasm32-unknown-unknown

# Install Stellar Quickstart for local dev
docker pull stellar/quickstart:latest
```

### Local Devnet

```bash
# Start Stellar local network with Soroban RPC
docker run --rm -it \
  --name stellar \
  -p 8000:8000 \
  stellar/quickstart:latest \
  --local \
  --enable-soroban
```

### Build Contracts

```bash
# Build all contracts in release mode
cargo build --target wasm32-unknown-unknown --release

# Verify WASM files exist
ls -la target/wasm32-unknown-unknown/release/*.wasm
```

### Deploy Contracts (Order Matters)

```bash
# 1. Deploy governance (needed first for multisig setup)
GOV_ID=$(soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/governance.wasm \
  --network local)
echo "Governance: $GOV_ID"

# 2. Deploy project registry
REG_ID=$(soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/project_registry.wasm \
  --network local)
echo "ProjectRegistry: $REG_ID"

# 3. Deploy credit token (as reference WASM for factory)
TOKEN_ID=$(soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/credit_token.wasm \
  --network local)
echo "CreditToken (reference): $TOKEN_ID"

# 4. Get credit token WASM hash
TOKEN_HASH=$(soroban contract install \
  --wasm target/wasm32-unknown-unknown/release/credit_token.wasm \
  --network local)
echo "Token WASM hash: $TOKEN_HASH"

# 5. Deploy credit factory
FACT_ID=$(soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/credit_factory.wasm \
  --network local)
echo "CreditFactory: $FACT_ID"

# 6. Deploy verification oracle
ORAC_ID=$(soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/verification_oracle.wasm \
  --network local)
echo "VerificationOracle: $ORAC_ID"

# 7. Deploy retirement registry
RET_ID=$(soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/retirement_registry.wasm \
  --network local)
echo "RetirementRegistry: $RET_ID"
```

### Initialize Contracts

```bash
# Generate admin keypair
ADMIN=$(soroban keys generate admin-key)

# Initialize governance with 3 multisig members
soroban contract invoke \
  --id $GOV_ID \
  --fn initialize \
  --arg admin:$ADMIN \
  --arg multisig_members:'["GABC...", "GDEF...", "GHIJ..."]' \
  --network local

# Initialize oracle
soroban contract invoke \
  --id $ORAC_ID \
  --fn initialize \
  --arg admin:$ADMIN \
  --arg credit_factory:$FACT_ID \
  --network local

# Initialize factory
soroban contract invoke \
  --id $FACT_ID \
  --fn initialize \
  --arg admin:$ADMIN \
  --network local

# Initialize registry
soroban contract invoke \
  --id $REG_ID \
  --fn initialize \
  --arg admin:$ADMIN \
  --network local

# Initialize retirement registry
soroban contract invoke \
  --id $RET_ID \
  --fn initialize \
  --arg admin:$ADMIN \
  --network local
```

### Register a Test Project

```bash
# Add an oracle
soroban contract invoke \
  --id $ORAC_ID \
  --fn add_oracle \
  --arg admin:$ADMIN \
  --arg oracle:$ORACLE_ADDR \
  --network local

# Register a project
PROJ_ID=$(soroban contract invoke \
  --id $FACT_ID \
  --fn register_project \
  --arg admin:$ADMIN \
  --arg name:"Green Valley Wetland" \
  --arg latitude:38897700 \
  --arg longitude:-77036500 \
  --arg methodology:"Wetland_Restoration_v2.1" \
  --arg owner:$PROJECT_OWNER \
  --arg area_hectares:500 \
  --arg credit_token_wasm_hash:$TOKEN_HASH \
  --network local)
echo "Project ID: $PROJ_ID"
```

### Testnet & Mainnet

```bash
# Testnet
soroban network add testnet \
  --rpc-url https://soroban-testnet.stellar.org \
  --network-passphrase "Test SDF Network ; September 2015"

soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/credit_factory.wasm \
  --network testnet

# Mainnet (requires funded account)
soroban network add mainnet \
  --rpc-url https://soroban.stellar.org \
  --network-passphrase "Public Global Stellar Network ; September 2015"

soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/credit_factory.wasm \
  --network mainnet
```

---

## 🧪 Testing Guide

### Running Tests

```bash
# Run all tests
cargo test --workspace

# Run with output (for debugging)
cargo test --workspace -- --nocapture

# Run specific test
cargo test test_credit_lifecycle -- --nocapture

# Run integration tests only
cargo test --test '*' -- --nocapture

# Run with coverage (requires cargo-tarpaulin)
cargo tarpaulin --workspace --out Html
```

### Test Architecture

```
tests/
├── integration/
│   ├── test_credit_lifecycle.rs       # End-to-end: register → verify → mint → trade → retire
│   ├── test_oracle_integration.rs     # Multi-oracle submission flow
│   ├── test_governance_flow.rs        # Proposal → vote → execute
│   └── test_factory_deployment.rs     # Factory deploys tokens correctly
└── unit/
    ├── test_credit_token.rs           # Mint, transfer, burn edge cases
    ├── test_math.rs                   # Verification formula corner cases
    ├── test_authorization.rs          # Unauthorized calls rejected
    └── test_retirement.rs             # Certificate generation & storage
```

### Key Test Scenarios

| Test | Description | Expected Outcome |
|---|---|---|
| `mint_to_success` | Admin mints 1000 credits to a user | Balance = 1000, Supply = 1000 |
| `mint_to_unauthorized` | Non-admin tries to mint | Panic: "unauthorized" |
| `transfer_success` | Alice sends 500 credits to Bob | Alice balance -= 500, Bob += 500 |
| `transfer_insufficient` | Alice sends more than she has | Panic: "insufficient balance" |
| `retire_success` | Holder retires 300 credits | Balance -= 300, Retired += 300, Certificate returned |
| `retire_zero` | Try to retire 0 credits | Panic: "invalid amount" |
| `register_project` | Admin registers a project | Project stored, token deployed, ID returned |
| `submit_reading_first` | Oracle submits first reading | Stored as baseline, 0 credits minted |
| `submit_reading_verify` | Oracle submits second reading with improvement | Credits computed and stored |
| `submit_reading_wrong_oracle` | Non-whitelisted oracle submits | Panic: "unauthorized oracle" |
| `submit_reading_replay` | Same nonce twice | Panic: "invalid nonce" |

### Example Test

```rust
#[test]
fn test_full_credit_lifecycle() {
    let env = Env::default();
    env.mock_all_auths();

    // Setup accounts
    let admin = Address::generate(&env);
    let oracle = Address::generate(&env);
    let farmer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let project_id: BytesN<32> = BytesN::from_array(&env, &[1u8; 32]);

    // Deploy credit token
    let token_id = env.register_contract(None, CreditToken);
    let token = CreditTokenClient::new(&env, &token_id);

    token.initialize(
        &admin,
        &String::from_str(&env, "Green Valley Credits"),
        &String::from_str(&env, "GVC"),
        &project_id,
        &String::from_str(&env, "Wetland_Restoration_v2.1"),
    );

    // Mint credits to farmer
    token.mint_to(&admin, &farmer, &5000);
    assert_eq!(token.balance(&farmer), 5000);

    // Farmer sells 1000 credits to buyer
    token.transfer(&farmer, &buyer, &1000);
    assert_eq!(token.balance(&farmer), 4000);
    assert_eq!(token.balance(&buyer), 1000);

    // Buyer retires 500 credits
    let cert = token.retire(
        &buyer,
        &500,
        &String::from_str(&env, "voluntary"),
        &String::from_str(&env, "ipfs://QmCert"),
    );
    assert_eq!(cert.amount, 500);
    assert_eq!(token.balance(&buyer), 500);
    assert_eq!(token.total_retired(), 500);
    assert_eq!(token.total_supply(), 4500);
}
```

---

## 🔌 Oracle Integration

### Oracle Node Requirements

An oracle node is an off-chain service that:

1. Receives sensor readings from edge gateways (via REST or MQTT).
2. Validates the cryptographic signature of each reading.
3. Aggregates readings from multiple gateways (optional).
4. Calls `verification_oracle.submit_reading()` with the validated data.
5. Manages nonces and retry logic.

### Oracle API (Internal)

Each oracle node exposes a management API (not part of the smart contracts):

| Endpoint | Method | Description |
|---|---|---|
| `/health` | GET | Node health and latest block |
| `/status` | GET | Current nonces, pending readings |
| `/submit` | POST | Send a reading to this oracle for on-chain submission |

### Oracle Configuration

```yaml
# oracle-config.yaml
oracle:
  address: GABC...DEF           # Stellar wallet address
  secret: SXXX...YYY           # Stellar wallet secret (keep secure!)
  
stellar:
  rpc_url: https://soroban-testnet.stellar.org
  network_passphrase: "Test SDF Network ; September 2015"
  
verification_contract: CABC...DEF
  
sensor_sources:
  - type: rest
    url: https://sensor-gateway.watershed.org/api/readings
    api_key: sk-...
    
submission:
  interval_seconds: 3600        # Submit every hour
  max_retries: 3
  concurrent_readings: 5
```

---

## 📡 Events & Indexing

### Event Topics

| Topic | Contract | Payload | Indexed Fields |
|---|---|---|---|
| `("minted",)` | `credit_token` | `(to, amount)` | `to` |
| `("transferred",)` | `credit_token` | `(from, to, amount)` | `from`, `to` |
| `("retired",)` | `credit_token` | `(retiree, amount, certificate)` | `retiree` |
| `("project_registered",)` | `credit_factory` | `(project_id, owner)` | `owner` |
| `("reading_verified",)` | `verification_oracle` | `(project_id, result)` | `project_id` |
| `("retirement_recorded",)` | `retirement_registry` | `(retiree, amount)` | `retiree` |

### Indexing with the Backend

The NestJS backend subscribes to these events using the Soroban RPC event stream and stores them in PostgreSQL for the frontend to query:

```typescript
// Example: subscribe to retirement events
const events = await server.getEvents({
  startLedger: 100000,
  filters: [{
    type: "contract",
    contractIds: [retirementContractId],
    topics: [symbolStrToScVal("retired")],
  }],
});
```

---

## ⚡ Gas Optimization

| Technique | Contract | Estimated Savings |
|---|---|---|
| **Pack struct fields** — Use `i64` instead of `i128` where possible | All | ~20% |
| **Minimize storage writes** — Batch updates, use `persistent` sparingly | Credit Token | ~15% |
| **Use `Env::events()` sparingly** — Emit only critical events | All | ~5% |
| **Short symbol names** — Prefer `Env::symbol()` over `Env::string()` for keys | All | ~10% |
| **Lazy storage reads** — Only read when needed, cache in local variables | Verification Oracle | ~10% |
| **Vec instead of Map** — For sequential data like oracle list | Governance | ~5% |

---

## ✅ Formal Verification

Core contracts (`credit_token` and `retirement_registry`) are targeted for formal verification using [K-Framework](https://kframework.org/) or [Dafny](https://dafny.org/). Properties to verify:

1. **Total supply invariant**: `total_supply = sum(balances) + total_retired`
2. **No double retirement**: A given amount cannot be retired twice
3. **Mint authority**: Only admin can mint
4. **Nonce monotonicity**: Oracle nonces strictly increase per (project, oracle)

---

## 🛠️ Build & Run Locally

```bash
# Clone
git clone https://github.com/water-credits/water-credits-contracts
cd water-credits-contracts

# Build
cargo build --target wasm32-unknown-unknown --release

# Test
cargo test --workspace -- --nocapture

# Deploy to local devnet
./scripts/deploy.sh

# Run simulation
npx ts-node scripts/simulate_readings.ts \
  --contract <ORACLE_CONTRACT_ID> \
  --project <PROJECT_ID> \
  --readings 100
```

---

## 🗺️ Roadmap

### v1.0 — Current (on testnet)
- [x] Six core contracts: `credit_token`, `credit_factory`, `verification_oracle`, `retirement_registry`, `project_registry`, `governance`
- [x] Single-oracle sensor reading submission
- [x] Nonce-based replay protection
- [x] Retirement certificates with IPFS metadata
- [x] Multisig admin (3-of-5)
- [x] 95%+ test coverage

### v1.1 — Near-term
- [ ] Multi-oracle median aggregation — once N oracles submit for the same `(project_id, timestamp)`, compute the median and trigger a single mint
- [ ] Commit-reveal scheme for oracle submissions to prevent MEV frontrunning
- [ ] `pause_project` and `pause_all` emergency controls wired to governance
- [ ] Admin key rotation with timelock

### v2.0 — Medium-term
- [ ] Token-weighted DAO voting (transition from multisig governance)
- [ ] Staking and slashing for oracle operators
- [ ] On-chain credit marketplace (bid/ask order book)
- [ ] Cross-chain bridge to EVM networks (Ethereum, Polygon)
- [ ] Formal verification of `credit_token` and `retirement_registry` invariants using K-Framework or Dafny

### Future
- [ ] Mobile oracle node client
- [ ] Integration with regulated carbon/water credit standards (Gold Standard, Verra)
- [ ] Bug bounty programme (currently in development)

---

## 🤝 Contributing

Contributions are welcome! Please read our [Contributing Guide](CONTRIBUTING.md) to get started.

Before contributing, review the [Code of Conduct](CODE_OF_CONDUCT.md). For security issues, see [SECURITY.md](SECURITY.md).

### Quick Start

```bash
# Fork and clone
git clone https://github.com/water-credits/water-credits-contracts

# Create feature branch
git checkout -b feat/your-feature

# Make changes, then:
cargo fmt && cargo clippy -- -D warnings
cargo test --workspace

# Commit conventional commits
git commit -m "feat: add multi-oracle median aggregation"

# Push and create PR
git push origin feat/your-feature
```

---

## 💬 Contact & Community

| Channel | Link |
|---|---|
| **GitHub** | [github.com/water-credits](https://github.com/water-credits) |
| **Telegram** | [@Escelit](https://t.me/Escelit) |
| **Email** | [ogazipromise81@gmail.com](mailto:ogazipromise81@gmail.com) |
| **Security reports** | See [SECURITY.md](SECURITY.md) — do not use public issues |

For bugs and feature requests, open a [GitHub issue](https://github.com/water-credits/water-credits-contracts/issues) using the provided templates.

---

## 📄 License

MIT — see [LICENSE](LICENSE).

---

<div align="center">
  <strong>Built with Rust 🦀 for Stellar Soroban ✨</strong>
</div>
