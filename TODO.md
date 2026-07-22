# Admin Transfer: Propose-Then-Accept Pattern

## Progress Tracking

### 1. credit_token (`contracts/credit_token/src/lib.rs`)
- [x] Add `PendingAdmin` and `AdminTransferExpiration` to DataKey
- [x] Already has event constants (`EVENT_ADMIN_PROPOSED`, `EVENT_ADMIN_ACCEPTED`, `EVENT_ADMIN_CANCELLED`)
- [x] Already has `ADMIN_TRANSFER_TTL` constant
- [x] Already has `propose_transfer_admin`, `accept_admin`, `cancel_transfer_admin`, `pending_admin`
- [x] Already has admin transfer tests

### 2. governance (`contracts/governance/src/lib.rs`)
- [x] Already has propose-then-accept pattern

### 3. verification_oracle (`contracts/verification_oracle/src/lib.rs`)
- [x] Already has propose-then-accept pattern

### 4. retirement_registry (`contracts/retirement_registry/src/lib.rs`)
- [x] Already has propose-then-accept pattern

### 5. project_registry (`contracts/project_registry/src/lib.rs`)
- [x] Already has propose-then-accept pattern

### 6. credit_factory (`contracts/credit_factory/src/lib.rs`)
- [x] Add `PendingAdmin` and `AdminTransferExpiration` to DataKey
- [x] Add event constants (`EVENT_ADMIN_PROPOSED`, `EVENT_ADMIN_ACCEPTED`, `EVENT_ADMIN_CANCELLED`)
- [x] Add `ADMIN_TRANSFER_TTL` constant
- [x] Add `propose_transfer_admin`, `accept_admin`, `cancel_transfer_admin`, `pending_admin` functions
- [x] Add admin transfer tests

### 7. Build & Test
- [ ] Build all contracts (blocked: missing MSVC linker `link.exe` — install Visual Studio Build Tools with C++ option)
- [ ] Run full test suite

