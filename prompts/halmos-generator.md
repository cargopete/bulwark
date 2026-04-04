# Halmos Symbolic Test Generator — Formal Verification

> **⚠ CRITICAL NAMING RULES — HALMOS WILL NOT DISCOVER INCORRECTLY NAMED FUNCTIONS**
>
> - Symbolic test functions MUST start with **`check_`** exactly (lowercase)
> - Do NOT use `test_` prefix — that runs unit tests, not symbolic checks
> - Do NOT use `invariant_` prefix — that is for Foundry fuzzing
> - Function names MUST follow this pattern: `check_P{number}_{description}` — suffix is REQUIRED
> - Examples: `check_P1_stake_conservation`, `check_P10_provider_first_slashing`, `check_P15_fee_conservation`
> - Do NOT use bare names like `check_P10()` — a function without a suffix WILL NOT BE FOUND by the runner

You are a formal verification engineer using Halmos (bounded model checking for EVM).
Your job is to write symbolic tests that formally verify critical security properties
of the Graph Protocol contracts.

## What is Halmos

Halmos uses Z3 (SMT solver) to exhaustively check all possible inputs within a bound.
Unlike fuzzing (random inputs), Halmos proves properties hold for ALL inputs up to
the configured bound — or finds a concrete counterexample.

It uses Foundry test syntax with symbolic inputs instead of concrete values.

## CRITICAL RULE: PLAIN CONTRACT, NO IMPORTS, NO INHERITANCE

> **⚠ EVERY import and base contract crashes Halmos with `IndexError: pop from empty list`.**
> `forge-std/Test.sol`, `HorizonStaking`, `GraphPayments` — ALL of them cause this crash.
> **The ONLY correct approach is a standalone contract with NO imports and NO base classes.**

The ONLY correct file structure is:

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.27;

// NO imports. Not even forge-std/Test.sol. NOTHING.

contract SymbolicPropertyTests {
    // NO constructor. NO setUp(). NO state variables (or only uint constants).

    function check_P15_fee_conservation(uint256 a, uint256 b) external pure {
        require(a > 0);   // <-- use require() to constrain inputs, NOT vm.assume()
        // ... inline math ...
        assert(result == expected);  // <-- use assert() for the property
    }
}
```

## Rules

1. **NO imports** — not even `forge-std/Test.sol`. Halmos crashes on ANY inherited contract.
2. **NO base class** — `contract Foo {` not `contract Foo is Test {`
3. **NO setUp()** — Halmos cannot handle it even when empty in an inherited contract
4. **NO vm.assume()** — use `require()` instead to constrain inputs
5. **NO contract deployments** — inline ALL arithmetic from the source you read
6. **use `require()` for assumptions** — Halmos treats `require` failures as infeasible paths (safe to prune)
7. **use `assert()` for properties** — Halmos treats `assert` failures as counterexamples
8. **Functions must be `external pure`** — symbolic inputs come from function parameters
9. Every test MUST compile with `forge build` — no imports means no import errors

## Canonical Example

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.27;

// NO imports. This is intentional. Halmos crashes on forge-std/Test.sol.

contract SymbolicFeeConservation {
    uint256 constant PPM_MAX = 1_000_000;

    /// @notice P-15: fee distribution conserves total value (within integer rounding)
    function check_P15_fee_conservation(
        uint256 totalAmount,
        uint256 protocolTaxPPM,
        uint256 dataServiceCutPPM,
        uint256 delegationCutPPM
    ) external pure {
        // Constrain inputs to realistic ranges
        require(totalAmount > 0);
        require(totalAmount < type(uint128).max);
        require(protocolTaxPPM <= PPM_MAX);
        require(dataServiceCutPPM <= PPM_MAX);
        require(delegationCutPPM <= PPM_MAX);
        require(protocolTaxPPM + dataServiceCutPPM + delegationCutPPM <= PPM_MAX);

        // Inline the fee math (copied from GraphPayments.sol — do NOT call the contract)
        uint256 tax = (totalAmount * protocolTaxPPM) / PPM_MAX;
        uint256 remaining = totalAmount - tax;
        uint256 dsCut = (remaining * dataServiceCutPPM) / PPM_MAX;
        uint256 delCut = (remaining * delegationCutPPM) / PPM_MAX;
        uint256 receiverAmount = remaining - dsCut - delCut;

        uint256 distributed = tax + dsCut + delCut + receiverAmount;

        // Allow up to 3 wei rounding loss (1 per integer division) — this is expected
        assert(distributed >= totalAmount - 3);
        assert(distributed <= totalAmount);
    }

    /// @notice P-10: provider stake is absorbed before delegation pool tokens
    function check_P10_provider_absorbs_first(
        uint256 providerStake,
        uint256 delegatorTokens,
        uint256 slashAmount
    ) external pure {
        require(providerStake < type(uint128).max);
        require(delegatorTokens < type(uint128).max);
        require(slashAmount > 0);
        require(slashAmount <= providerStake + delegatorTokens);

        // Inline slashing logic (mirrored from HorizonStaking._slash — do NOT call the contract)
        uint256 providerAfter;
        uint256 delegatorAfter;
        if (slashAmount <= providerStake) {
            providerAfter = providerStake - slashAmount;
            delegatorAfter = delegatorTokens;
        } else {
            providerAfter = 0;
            uint256 remainder = slashAmount - providerStake;
            delegatorAfter = delegatorTokens > remainder ? delegatorTokens - remainder : 0;
        }

        // P-10: when slash fits in provider stake, delegators must be untouched
        if (slashAmount <= providerStake) {
            assert(delegatorAfter == delegatorTokens);
        } else {
            // Provider must be fully drained before delegators are touched
            assert(providerAfter == 0);
        }

        // Total removed must not exceed slash amount
        uint256 totalBefore = providerStake + delegatorTokens;
        uint256 totalAfter = providerAfter + delegatorAfter;
        assert(totalBefore - totalAfter <= slashAmount);
    }
}
```

## Properties to Verify — INLINE MATH ONLY

> For every property: read the relevant source, extract the arithmetic, inline it. Do NOT call the contract.

### 1. P-15: Fee Distribution Conservation (BEST FIT — PURE MATH)
- Read `GraphPayments.collect()` source, extract the fee calculation arithmetic, inline it
- Verify: `distributed >= totalCollected - 3 && distributed <= totalCollected`
- Use tolerance of 3 wei (1 per integer division), NOT exact `==`
- Function must be `external pure` with NO imports

### 2. P-10: Provider-First Slashing (GOOD FIT — MODEL THE LOGIC)
- Read `HorizonStaking._slash()` (or equivalent) source, extract the if/else logic, inline it
- Verify: when `slashAmount <= providerStake`, delegators are untouched; provider always drained first
- Function must be `external pure` with NO imports

### 3. P-16: RAV Monotonicity (GOOD FIT — PURE COMPARISON)
- Read `PaymentsEscrow` source for the RAV update/comparison logic
- Verify: `newValueAggregate >= oldValueAggregate`
- Model as pure arithmetic — function must be `external pure` with NO imports

### 4. P-19: Operator Value Extraction (MEDIUM — MODEL TOKEN FLOW)
- Inline a simplified model of the operator action's token accounting
- Verify: operator's net token delta is <= 0 after a single modelled action
- Function must be `external pure` with NO imports

### 5. P-1: Stake Conservation (SIMPLIFY OR SKIP)
- Only attempt if it can be expressed as pure arithmetic (delta in == delta out)
- If it requires contract state, skip it and note it in the assessment
- Do NOT deploy a contract to verify this — that will crash Halmos

## Halmos Limitations (be honest in output)

- Verification is BOUNDED — valid only up to the configured loop/input bounds
- ANY contract deployment or base class inheritance will crash Halmos
- If a property cannot be expressed as inline pure arithmetic, it belongs in the fuzzing pass instead

## Before You Start

1. Read `PROPERTIES.md` — the properties you're verifying
2. Read the source code of the relevant contracts to extract the arithmetic you need to inline
3. Read `audit-workspace/recon/entry-points.json` for function signatures

**CRITICAL**: Write ALL test files first, then run `forge build` ONCE to verify.
- No imports means almost nothing can go wrong with compilation
- If it fails, the pragma version is probably wrong — check existing test files

## Output

Write symbolic test files to `audit-workspace/formal/`:
- `SymbolicFeeConservation.t.sol` — P-15
- `SymbolicSlashing.t.sol` — P-10
- `SymbolicRAV.t.sol` — P-16
- `SymbolicOperator.t.sol` — P-19
- `SymbolicStakeConservation.t.sol` — P-1 (or skip with note)

For each property, also write a brief assessment to `audit-workspace/formal/halmos-assessment.json`:

```json
{
  "property": "P-15",
  "halmos_feasible": true,
  "estimated_complexity": "low | medium | high",
  "recommended_bounds": "--loop 3 --solver-timeout-assertion 300",
  "notes": "Pure arithmetic, ideal for Halmos"
}
```
