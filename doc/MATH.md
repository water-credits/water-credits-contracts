# Verification Formula Derivations

This document provides full derivations of every credit-calculation formula used in
`contracts/verification_oracle/src/lib.rs`. All formulas are derived directly from
the source code, so the numbers here can be checked against the contract line-by-line.

---

## Table of Contents

1. [Input Encoding](#1-input-encoding)
2. [Multi-Oracle Median Aggregation](#2-multi-oracle-median-aggregation)
3. [Nutrient Removal Formulas](#3-nutrient-removal-formulas)
   - [Nitrogen Removal](#31-nitrogen-removal)
   - [Phosphorus Removal](#32-phosphorus-removal)
4. [Volumetric Credit Formula](#4-volumetric-credit-formula)
5. [Quality Penalty Calculation](#5-quality-penalty-calculation)
   - [pH Penalty](#51-ph-penalty)
   - [Turbidity Penalty](#52-turbidity-penalty)
   - [Dissolved-Oxygen Penalty](#53-dissolved-oxygen-penalty)
   - [Temperature Penalty](#54-temperature-penalty)
   - [Penalty Cap](#55-penalty-cap)
6. [Total Credits](#6-total-credits)
7. [Default Configuration Values](#7-default-configuration-values)
8. [Worked Examples](#8-worked-examples)
   - [Example A — Healthy System, Good Removal](#example-a--healthy-system-good-removal)
   - [Example B — All Quality Thresholds Breached](#example-b--all-quality-thresholds-breached)
   - [Example C — N and P Above Baseline (No Removal Credits)](#example-c--n-and-p-above-baseline-no-removal-credits)
   - [Example D — Zero Flow](#example-d--zero-flow)
   - [Example E — Even Oracle Count, Median Averaging](#example-e--even-oracle-count-median-averaging)

---

## 1. Input Encoding

All sensor readings are passed into `reveal_reading` (after a `commit_reading`
commitment) as **raw integers**. The caller (oracle node software) is
responsible for encoding physical values before submission.

| Field | Rust param | Physical unit | Encoding | Example |
|---|---|---|---|---|
| pH | `ph` | dimensionless | integer × 100 | 7.00 → `700` |
| Turbidity | `turbidity` | NTU | integer × 10 | 12.4 NTU → `124` |
| Dissolved oxygen | `dissolved_oxygen` | mg/L | integer × 10 | 8.0 mg/L → `80` |
| Flow rate | `flow_rate` | L/s | raw integer | 500 L/s → `500` |
| Temperature | `temperature` | °C | integer × 10 | 25.0 °C → `250` |
| Total nitrogen | `total_nitrogen` | mg/L | raw integer | 8 mg/L → `8` |
| Total phosphorus | `total_phosphorus` | mg/L | raw integer | 1 mg/L → `1` |

> **Note:** The contract never de-scales these values. Every formula shown below
> operates on the already-encoded integers so the scaling factors carry through.

### 1.1 Entry Validation

Every call to `submit_reading` and `reveal_reading` validates the raw sensor fields
before they can enter median aggregation. A reading that fails validation panics
(reverting the transaction) rather than being accepted:

| Field | Valid range |
|---|---|
| `ph` | `[0, 1400]` (pH 0.0 – 14.0, encoded ×100) |
| `flow_rate` | `≥ 0` |
| `total_nitrogen` | `≥ 0` |
| `total_phosphorus` | `≥ 0` |
| `dissolved_oxygen` | `≥ 0` |

These bounds reject structurally-valid-but-physically-impossible `i64` values (e.g. a
malfunctioning sensor reporting negative nitrogen), which would otherwise inflate
`baseline − med_n` in [§3](#3-nutrient-removal-formulas) and mint credits far beyond
any physically plausible removal. `turbidity` and `temperature` are not bounded at
entry — only the quality-penalty thresholds in [§5](#5-quality-penalty-calculation)
apply to them.

---

## 2. Multi-Oracle Median Aggregation

A **window** collects one submission per whitelisted oracle for a given project. Once
the number of submissions reaches `min_oracles` (default **3**), the window is
finalised and the contract computes the element-wise **median** of each field across
all submissions.

### Median algorithm (from `median_i64`)

1. Copy the `n` values from the Soroban `Vec<i64>` into a local `[i64; 10]`
   stack array (bounded by `max_oracles = 10`).
2. Insertion-sort the local array in-place (zero Soroban host calls; at most
   45 local comparisons for `n = 10`).
3. If the count is **even**: `median = (arr[n/2 - 1] + arr[n/2]) / 2`  
   (Rust `i64` integer division — truncates toward zero).
4. If the count is **odd**: `median = arr[n/2]`

All subsequent calculations use only the per-field median values:

```
med_ph, med_turb, med_do, med_temp, med_flow, med_n, med_p
```

---

## 3. Nutrient Removal Formulas

Nutrient removal credits reward the system for lowering nitrogen and phosphorus
concentrations below hardcoded baselines. The window is assumed to span **3 600 seconds**
(one hour) — a fixed constant baked into the contract.

### 3.1 Nitrogen Removal

**Baseline:** `baseline_n = 10` (mg/L, raw integer matching the `total_nitrogen` encoding)

```
if med_n < baseline_n:
    n_removal_kg = (baseline_n − med_n) × med_flow × 3600 / 1 000 000
else:
    n_removal_kg = 0
```

#### Unit derivation

```
(mg/L) × (L/s) × (s) / (mg/kg)
= mg × s / (L × s / L) / mg/kg
= mg/L × L/s × s ÷ (10⁶ mg/kg)
= (baseline_n − med_n) [mg/L] × med_flow [L/s] × 3600 [s] ÷ 1 000 000 [mg/kg]
= result in kg
```

`n_removal_kg` is an `i128` representing **kilograms of nitrogen removed** over the window.

### 3.2 Phosphorus Removal

Identical in structure to nitrogen, but with a lower baseline reflecting typical
freshwater targets.

**Baseline:** `baseline_p = 2` (mg/L, same encoding as `total_phosphorus`)

```
if med_p < baseline_p:
    p_removal_kg = (baseline_p − med_p) × med_flow × 3600 / 1 000 000
else:
    p_removal_kg = 0
```

`p_removal_kg` is an `i128` representing **kilograms of phosphorus removed** over the window.

---

## 4. Volumetric Credit Formula

Volumetric credits reward throughput — the volume of water flowing through the
restored system regardless of nutrient concentrations.

```
if med_flow > 0:
    volumetric_credit = med_flow × 100 / 1000
else:
    volumetric_credit = 0
```

#### Simplification

```
volumetric_credit = med_flow / 10
```

Because `med_flow` is in **L/s**, dividing by 10 converts to **dL/s** — but the
contract treats the result as a dimensionless credit unit. One credit is issued for
every 10 L/s of flow through the project site during the measurement window.

---

## 5. Quality Penalty Calculation

The penalty is expressed in **basis points (bps)**, where 10 000 bps = 100 %. It
reduces the gross credit total proportionally (see [Section 6](#6-total-credits)).

The penalty is computed as a sum of **discrete step penalties** — there is no
continuous scaling. Each threshold breach adds a fixed number of basis points.

| Condition | Basis points added |
|---|---|
| pH out of acceptable range | +2 000 |
| Turbidity too high | +2 000 |
| Dissolved oxygen too low | +2 000 |
| Temperature too high | +1 000 |
| **Maximum total** | **8 000** |

### 5.1 pH Penalty

The acceptable pH range is `[quality_threshold_ph, quality_threshold_ph_max]`  
(default: `[600, 700]` → physical range **6.00 – 7.00**). Both bounds are
independently configurable `OracleConfig` fields — the upper bound is no longer
hardcoded to `quality_threshold_ph + 100`, since a fixed +100 offset doesn't make
physical sense for every choice of `quality_threshold_ph`.

```
if med_ph < quality_threshold_ph OR med_ph > quality_threshold_ph_max:
    penalty += 2000
```

A reading is penalised when pH falls outside the window. No partial penalty exists;
the full 2 000 bps is applied for any breach.

### 5.2 Turbidity Penalty

```
if med_turb > quality_threshold_turbidity:   # default threshold: 50 (= 5.0 NTU)
    penalty += 2000
```

High turbidity indicates suspended sediment or algal matter that degrades water
quality. Values at or below the threshold receive no penalty.

### 5.3 Dissolved-Oxygen Penalty

```
if med_do < quality_threshold_do:   # default threshold: 50 (= 5.0 mg/L)
    penalty += 2000
```

Low DO indicates hypoxic conditions harmful to aquatic life.

### 5.4 Temperature Penalty

```
if med_temp > quality_threshold_temp:   # default threshold: 300 (= 30.0 °C)
    penalty += 1000
```

Temperature receives half the penalty weight of the other parameters because
seasonal variation is inherent and less controllable.

### 5.5 Penalty Cap

```
if penalty > 8000:
    penalty = 8000
```

The maximum deduction is 80 % of gross credits. Even in the worst-case scenario (all
four conditions breached, total would be 7 000 bps), the cap is never reached with
the default weights — but it provides a ceiling if future conditions or parameter
changes would otherwise exceed it.

---

## 6. Total Credits

```
n_credit   = n_removal_kg  × credit_per_kg_n    # default: ×10
p_credit   = p_removal_kg  × credit_per_kg_p    # default: ×20
gross      = n_credit + p_credit + volumetric_credit
total      = max(0, gross × (10000 − penalty) / 10000)
```

All arithmetic uses `i128` (Rust signed 128-bit integers) with integer division.
Because the final division rounds toward zero, **fractional credits are truncated,
never rounded up**. This ensures the on-chain supply can never exceed the true
calculated entitlement.

If `gross = 0` (zero flow, nutrients above baseline, etc.) and `penalty > 0`, the
multiplication still yields `0 × anything = 0`, so `total_credits` is 0. `total` is
also explicitly floored at 0 (`.max(0)`): `gross` is normally non-negative, but an
admin-configured negative `credit_per_kg_n` / `credit_per_kg_p` can legitimately
drive it negative, and the floor guarantees `total_credits` can never be
misread as a negative balance regardless of configuration.

### Overflow handling

Every multiplication in §3, §4, and §6 (`baseline − med`, `× med_flow`, `× 3600`,
`× credit_per_kg_*`, the final `× (10000 − penalty)`, and the `n_credit + p_credit +
volumetric_credit` addition) uses `checked_mul` / `checked_add` and panics with a
descriptive message on overflow, instead of silently wrapping in `i128`. This matters
because `med_flow` (and an admin-set per-project `baseline_n`/`baseline_p`) are only
bounded to be non-negative `i64` values — a value near `i64::MAX` combined with a
large baseline can overflow the `× 3600` step before the division, and an
unchecked wraparound there would silently mint a corrupted (and potentially
enormous) credit amount rather than reverting the transaction.

---

## 7. Default Configuration Values

These values are set in `VerificationOracle::initialize` and can be changed by the
admin via `update_config`.

| `OracleConfig` field | Default | Meaning |
|---|---|---|
| `min_oracles` | `3` | Readings required before window finalises |
| `max_oracles` | `10` | Hard cap on whitelisted oracles |
| `quality_threshold_ph` | `600` | Lower end of acceptable pH range (= 6.00) |
| `quality_threshold_ph_max` | `700` | Upper end of acceptable pH range (= 7.00) |
| `quality_threshold_turbidity` | `50` | Max acceptable turbidity (= 5.0 NTU) |
| `quality_threshold_do` | `50` | Min acceptable dissolved oxygen (= 5.0 mg/L) |
| `quality_threshold_temp` | `300` | Max acceptable temperature (= 30.0 °C) |
| `credit_per_kg_n` | `10` | Credits awarded per kg of N removed |
| `credit_per_kg_p` | `20` | Credits awarded per kg of P removed |

---

## 8. Worked Examples

The examples below show the complete calculation for a three-oracle window. For
brevity, all three oracles submit identical values so the median equals the input.

---

### Example A — Healthy System, Good Removal

**Sensor readings (all oracles identical):**

| Field | Raw value | Physical value |
|---|---|---|
| `ph` | `700` | 7.00 |
| `turbidity` | `10` | 1.0 NTU |
| `dissolved_oxygen` | `80` | 8.0 mg/L |
| `flow_rate` | `500` | 500 L/s |
| `temperature` | `250` | 25.0 °C |
| `total_nitrogen` | `8` | 8 mg/L |
| `total_phosphorus` | `1` | 1 mg/L |

**Step 1 — Median** (all oracles identical, median = input):
```
med_ph=700, med_turb=10, med_do=80, med_temp=250, med_flow=500, med_n=8, med_p=1
```

**Step 2 — Nitrogen removal:**
```
baseline_n = 10
med_n (8) < baseline_n (10)  → removal occurs

n_removal_kg = (10 − 8) × 500 × 3600 / 1_000_000
             = 2 × 500 × 3600 / 1_000_000
             = 3_600_000 / 1_000_000
             = 3  (kg)
```

**Step 3 — Phosphorus removal:**
```
baseline_p = 2
med_p (1) < baseline_p (2)  → removal occurs

p_removal_kg = (2 − 1) × 500 × 3600 / 1_000_000
             = 1 × 500 × 3600 / 1_000_000
             = 1_800_000 / 1_000_000
             = 1  (kg)
```

**Step 4 — Volumetric credit:**
```
volumetric_credit = 500 × 100 / 1000 = 50
```

**Step 5 — Quality penalty:**
```
pH 700 is within [600, 700]                 → +0
turbidity 10 ≤ 50                           → +0
dissolved_oxygen 80 ≥ 50                    → +0
temperature 250 ≤ 300                       → +0
penalty = 0 bps
```

**Step 6 — Total credits:**
```
n_credit   = 3 × 10  = 30
p_credit   = 1 × 20  = 20
gross      = 30 + 20 + 50 = 100
total      = 100 × (10000 − 0) / 10000 = 100
```

**Result:** `total_credits = 100`

---

### Example B — All Quality Thresholds Breached

**Sensor readings:**

| Field | Raw value | Physical value |
|---|---|---|
| `ph` | `300` | 3.00 (acidic, far below range) |
| `turbidity` | `200` | 20.0 NTU (high) |
| `dissolved_oxygen` | `10` | 1.0 mg/L (hypoxic) |
| `flow_rate` | `500` | 500 L/s |
| `temperature` | `350` | 35.0 °C (above 30.0 threshold) |
| `total_nitrogen` | `8` | 8 mg/L |
| `total_phosphorus` | `1` | 1 mg/L |

**Step 1 — Median:** same as input.

**Step 2 — Nitrogen removal:**
```
n_removal_kg = (10 − 8) × 500 × 3600 / 1_000_000 = 3  (kg)
```

**Step 3 — Phosphorus removal:**
```
p_removal_kg = (2 − 1) × 500 × 3600 / 1_000_000 = 1  (kg)
```

**Step 4 — Volumetric credit:**
```
volumetric_credit = 500 × 100 / 1000 = 50
```

**Step 5 — Quality penalty:**
```
pH 300 < 600                                → +2000
turbidity 200 > 50                          → +2000
dissolved_oxygen 10 < 50                    → +2000
temperature 350 > 300                       → +1000
penalty = 7000 bps  (below 8000 cap)
```

**Step 6 — Total credits:**
```
n_credit  = 3 × 10  = 30
p_credit  = 1 × 20  = 20
gross     = 30 + 20 + 50 = 100
total     = 100 × (10000 − 7000) / 10000
          = 100 × 3000 / 10000
          = 300_000 / 10000
          = 30
```

**Result:** `total_credits = 30` (70 % reduction from quality penalty)

---

### Example C — N and P Above Baseline (No Removal Credits)

**Sensor readings:**

| Field | Raw value | Physical value |
|---|---|---|
| `ph` | `700` | 7.00 |
| `turbidity` | `10` | 1.0 NTU |
| `dissolved_oxygen` | `80` | 8.0 mg/L |
| `flow_rate` | `500` | 500 L/s |
| `temperature` | `250` | 25.0 °C |
| `total_nitrogen` | `15` | 15 mg/L (above baseline of 10) |
| `total_phosphorus` | `5` | 5 mg/L (above baseline of 2) |

**Step 2 — Nitrogen removal:**
```
med_n (15) ≥ baseline_n (10)  → no removal
n_removal_kg = 0
```

**Step 3 — Phosphorus removal:**
```
med_p (5) ≥ baseline_p (2)  → no removal
p_removal_kg = 0
```

**Step 4 — Volumetric credit:**
```
volumetric_credit = 500 × 100 / 1000 = 50
```

**Step 5 — Quality penalty:** `0 bps` (all readings in range).

**Step 6 — Total credits:**
```
gross  = 0 + 0 + 50 = 50
total  = 50 × 10000 / 10000 = 50
```

**Result:** `total_credits = 50` (volumetric only; no nutrient credit)

---

### Example D — Zero Flow

**Sensor readings:**

| Field | Raw value | Physical value |
|---|---|---|
| `flow_rate` | `0` | 0 L/s |
| `total_nitrogen` | `2` | 2 mg/L (below baseline) |
| `total_phosphorus` | `0` | 0 mg/L (below baseline) |
| All others | within range | — |

**Step 2 — Nitrogen removal:**
```
n_removal_kg = (10 − 2) × 0 × 3600 / 1_000_000 = 0
```

**Step 3 — Phosphorus removal:**
```
p_removal_kg = (2 − 0) × 0 × 3600 / 1_000_000 = 0
```

**Step 4 — Volumetric credit:**
```
med_flow = 0  →  volumetric_credit = 0
```

**Step 6 — Total credits:**
```
gross = 0 + 0 + 0 = 0
total = 0 × anything / 10000 = 0
```

**Result:** `total_credits = 0`

Zero flow means no water passed through the system during the window; no credits are
issued regardless of nutrient concentrations.

---

### Example E — Even Oracle Count, Median Averaging

This example shows how the contract handles an even number of oracle submissions
when `min_oracles` is set to `2`.

**Oracle 1:** `flow_rate = 400`  
**Oracle 2:** `flow_rate = 600`

**Median calculation:**
```
sorted = [400, 600]
len = 2 (even)
median = (sorted[0] + sorted[1]) / 2 = (400 + 600) / 2 = 500
```

**Volumetric credit:**
```
volumetric_credit = 500 × 100 / 1000 = 50
```

This matches the assertion in `test_median_with_even_number_of_oracles_uses_lower_middle`
in the test suite.

---

## Cross-References

| Formula | Source location |
|---|---|
| `median_i64` | `contracts/verification_oracle/src/lib.rs` — function `median_i64` |
| Nitrogen removal | `finalize_reveals`, lines under `// N removal: baseline 10 mg/L` |
| Phosphorus removal | `finalize_reveals`, lines under `// P removal: baseline 2 mg/L` |
| Quality penalty | `finalize_reveals`, lines under `// Quality penalty (basis points: 0-10000)` |
| Volumetric credit | `finalize_reveals`, lines under `// Volumetric credit based on flow` |
| Total credits | `finalize_reveals`, lines under `// Apply quality penalty` |
| Default config | `VerificationOracle::initialize`, `OracleConfig { ... }` literal |
