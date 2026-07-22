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

### Fixed

- Duplicate admin set in `governance` initialize
- Max supply cap enforcement in `credit_token` mint

### Changed

- Extracted balance/supply storage helpers in `credit_token`
- Expanded spec documentation with oracle window lifecycle

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
