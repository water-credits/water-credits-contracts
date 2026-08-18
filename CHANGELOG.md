# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Indexer-facing events across all contracts for off-chain state reconstruction:
  - `retirement_registry`: `ret_rec` on `record_retirement`, `auth_set` on `set_authorized_caller`, `init` on `initialize`
  - `project_registry`: `proj_reg` on `register`, `stat_chg` on `update_status`, `ownr_chg` on `update_owner`, `init` on `initialize`
  - `credit_factory`: `stat_chg` on `update_project_status`, `ownr_chg` on `update_project_owner`, `init` on `initialize`
  - `credit_token`: `approved` on `approve`, `adm_xfer` on `set_admin`, `init` on `initialize`
  - `governance` and `verification_oracle`: `adm_xfer` on `transfer_admin`, `init` on `initialize`
- `verification_oracle`: `OracleConfig::window_secs` makes the monitoring window a configurable parameter of the nutrient-removal formula instead of a hardcoded `3600`. Defaults to `3600` (unchanged behaviour), validated by `update_config` to `[60, 86400]` and exported as `MIN_WINDOW_SECS` / `MAX_WINDOW_SECS`. Deployments not on an exact hourly interval previously received systematically wrong credits — 2× for a 30-minute interval, 1/6× for a 6-hour interval — with no way to correct it short of a contract upgrade
- Oracle staking and slashing mechanism in `verification_oracle`
- `stake`, `unstake`, `claim_unstake`, `slash` functions
- Admin-only `slash` with reason codes (admin flag / fraud proof)
- Slashed funds sent to configurable treasury address
- Cooldown-based unstaking with configurable delay
- Min-stake enforcement on `add_oracle` and `submit_reading`
- Oracle must fully unstake before removal
- Events: `orc_stk`, `orc_unst`, `orc_slsh`
- Staking getters: `get_stake`, `get_slash_record`, `get_unstake_cooldown`, `get_treasury`, `get_staking_token`
- Emergency pause propagation across all contracts
- Batch transfer support in `credit_token`
- Allowance expiration in `credit_token`
- Transfer admin capability in `governance`
- Transfer admin capability in `verification_oracle`, enabling governance to hold admin authority over the oracle for proposal-driven `update_config` calls
- Historical verification results in `verification_oracle`
- Oracle count getter and oracle list in `verification_oracle`
- Oracle submission stats tracking in `verification_oracle`
- Reset window capability in `verification_oracle`
- Owner update capability in `project_registry` and `credit_factory`
- Retirement query by project in `retirement_registry`
- Batch mint in `credit_token`
- Transfer and burn event emissions in `credit_token`
- Paused/unpaused event emissions in `credit_token`
- Deployment script with Soroban deploy commands
- Math derivations documentation (`doc/MATH.md`)
- `slash_pct_bps`, `min_slash_amount`, and `max_slash_amount` fields on `OracleConfig` for proportional missed-reveal slashing

### Fixed

- Duplicate admin set in `governance` initialize
- Max supply cap enforcement in `credit_token` mint
- `verification_oracle`: per-oracle submission statistics now count only
  contributions to finalized windows, so resetting a pending window cannot
  inflate an oracle's reputation counters
- `verification_oracle`: `validate_sensor_reading` now rejects out-of-range `turbidity` and `temperature` readings, closing a gap that let a malicious or malfunctioning oracle submit negative values to disable the turbidity/temperature quality penalties
- `shared::generate_project_id` no longer folds the ledger timestamp into the SHA-256 preimage, so a project ID no longer depends on which ledger the registration transaction lands in. Off-chain systems can now pre-compute the ID before submitting; previously a one-ledger delay (fee bump, congestion) silently changed it

### Changed

- Extracted balance/supply storage helpers in `credit_token`
- Expanded spec documentation with oracle window lifecycle
- `penalize_non_revealers` now slashes a percentage of an oracle's stake (`stake * slash_pct_bps / 10_000`, clamped to `[min_slash_amount, max_slash_amount]`) instead of a flat `min(stake, min_stake)` amount, so oracles with larger stakes face proportionally larger penalties for missed reveals
- `update_config` now validates `slash_pct_bps <= 5000` (50% max) and `min_slash_amount <= max_slash_amount`
- **Breaking:** `shared::generate_project_id` dropped its `timestamp` parameter; the derivation is now `SHA-256(count || len(name) || name || len(methodology) || methodology || latitude || longitude || area_hectares)`. Already-registered projects keep their stored IDs; only IDs derived from this point on change. The registration timestamp is still recorded as `ProjectInfo.registration_date` / `ProjectEntry.registered_at`
- **Breaking:** `compute_finalization` takes a trailing `window_secs: u64` parameter; both in-contract call sites pass `config.window_secs`. Direct callers (tests, off-chain reimplementations) must pass `3600` to preserve the previous behaviour

### Testing

- Edge case tests for zero-flow and single-oracle readings
- Cross-contract integration tests

## [0.1.0] - 2026-06-07

### Added

- Soroban workspace with six contracts
- `credit_token` contract with mint, burn, transfer, and allowance
- `credit_factory` contract for credit issuance
- `verification_oracle` contract for flow verification
- `retirement_registry` contract for credit retirement tracking
- `project_registry` contract for project management
- `governance` contract for admin and policy management
- Cross-contract call support and integration tests
- Contributor onboarding files and GitHub templates
- Documentation and doc comments

[Unreleased]: https://github.com/ogaziedaniel80-droid/water-credits-contracts/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ogaziedaniel80-droid/water-credits-contracts/releases/tag/v0.1.0
