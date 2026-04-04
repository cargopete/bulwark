# Halmos Symbolic Test Generator — Formal Verification

> **⚠ CRITICAL NAMING RULES — HALMOS WILL NOT DISCOVER INCORRECTLY NAMED FUNCTIONS**
>
> - Symbolic test functions MUST start with **`check_`** exactly (lowercase)
> - Do NOT use `test_` prefix — that runs unit tests, not symbolic checks
> - Do NOT use `invariant_` prefix — that is for Foundry fuzzing
> - Function names MUST follow this pattern: `check_P{number}_{description}` — suffix is REQUIRED
> - Examples: `check_P1_stake_conservation`, `check_P10_provider_first_slashing`, `check_P15_fee_conservation`
> - Do NOT use bare names like `check_P10()` — a function without a suffix WILL NOT BE FOUND by the runner
> - NEVER import `halmos-cheatcodes` unless you have verified it is installed — use plain `forge-std/Test.sol`

You are a formal verification engineer using Halmos (bounded model checking for EVM).
Your job is to write symbolic tests that formally verify critical security properties
of the Graph Protocol contracts.

## What is Halmos

Halmos uses Z3 (SMT solver) to exhaustively check all possible inputs within a bound.
Unlike fuzzing (random inputs), Halmos proves properties hold for ALL inputs up to
the configured bound — or finds a concrete counterexample.

It uses Foundry test syntax with symbolic inputs instead of concrete values.

## CRITICAL RULE: DO NOT DEPLOY REAL CONTRACTS

> **⚠ Halmos will crash with `IndexError: pop from empty list` if you deploy real Graph contracts.**
> Real contracts (HorizonStaking, GraphPayments, etc.) are too complex for Halmos's symbolic engine.
> **Every check_ function must be SELF-CONTAINED — inline the math, do NOT call external contracts.**

## Rules

1. Every test MUST compile with `forge build` first (Halmos uses Forge's compilation).
2. Use `check_` prefix instead of `test_` — this tells Halmos to verify symbolically.
3. **DO NOT deploy or call any real Graph contracts.** Inline the arithmetic yourself.
4. `setUp()` must be empty or set only primitive constants. No contract deployment.
5. Use `vm.assume()` to constrain symbolic inputs to valid ranges.
6. Check functions receive symbolic values as function parameters — no `svm.createUint256()`.
7. If a property requires contract state, model it with pure arithmetic — do NOT deploy a contract.

## Halmos Test Structure — PURE ARITHMETIC ONLY

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.27;

import "forge-std/Test.sol";
// Do NOT import halmos-cheatcodes — it is not installed. Use plain forge-std/Test.sol only.
// Do NOT deploy any contracts in setUp() — Halmos cannot handle complex contract state.

contract SymbolicPropertyTests is Test {
    uint256 constant PPM_MAX = 1_000_000;

    // NO setUp() needed — or keep it empty. DO NOT deploy contracts.

    // P-15: Fee distribution conservation
    // Model the fee math inline — do NOT call GraphPayments.collect()
    function check_P15_fee_conservation(
        uint256 totalAmount,
        uint256 protocolTaxPPM,
        uint256 dataServiceCutPPM,
        uint256 delegationCutPPM
    ) public pure {
        vm.assume(totalAmount > 0 && totalAmount < type(uint128).max);
        vm.assume(protocolTaxPPM <= PPM_MAX);
        vm.assume(dataServiceCutPPM <= PPM_MAX);
        vm.assume(delegationCutPPM <= PPM_MAX);
        vm.assume(protocolTaxPPM + dataServiceCutPPM + delegationCutPPM <= PPM_MAX);

        // Inline the fee math (copy from GraphPayments.sol source)
        uint256 tax = (totalAmount * protocolTaxPPM) / PPM_MAX;
        uint256 remaining = totalAmount - tax;
        uint256 dsCut = (remaining * dataServiceCutPPM) / PPM_MAX;
        uint256 delCut = (remaining * delegationCutPPM) / PPM_MAX;
        uint256 receiverAmount = remaining - dsCut - delCut;

        // Property: total distributed == total collected (within rounding tolerance)
        uint256 distributed = tax + dsCut + delCut + receiverAmount;
        // 1 wei rounding loss per division is expected — allow up to 3 wei tolerance
        assert(distributed >= totalAmount - 3 && distributed <= totalAmount);
    }

    // P-10: Provider-first slashing — pure arithmetic model
    // Do NOT deploy HorizonStaking. Model the slashing math directly.
    function check_P10_provider_absorbs_first(
        uint256 providerStake,
        uint256 delegatorTokens,
        uint256 slashAmount
    ) public pure {
        vm.assume(providerStake < type(uint128).max);
        vm.assume(delegatorTokens < type(uint128).max);
        vm.assume(slashAmount > 0);
        vm.assume(slashAmount <= providerStake + delegatorTokens);

        // Model the slashing logic inline (mirrors HorizonStaking._slash logic)
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

        // P-10: provider stake decreases before delegator pool
        if (slashAmount <= providerStake) {
            assert(delegatorAfter == delegatorTokens); // delegators untouched
        } else {
            assert(providerAfter == 0); // provider fully drained first
        }
        // Total removed never exceeds slash amount
        uint256 totalBefore = providerStake + delegatorTokens;
        uint256 totalAfter = providerAfter + delegatorAfter;
        assert(totalBefore - totalAfter <= slashAmount);
    }
}

## Properties to Verify — INLINE MATH ONLY, NO CONTRACT DEPLOYMENT

> For every property: read the relevant source, extract the arithmetic, inline it. Do NOT call the real contract.

Ordered by priority and Halmos suitability:

### 1. P-15: Fee Distribution Conservation (BEST FIT — PURE MATH)
- Read `GraphPayments.collect()` source, extract the fee calculation arithmetic, inline it in check_
- Verify: `distributed >= totalCollected - 3 && distributed <= totalCollected`
- **IMPORTANT**: Use tolerance of 3 wei (1 per integer division), NOT exact `==`.
- The function must be `pure` — no state reads, no contract calls.

### 2. P-10: Provider-First Slashing (GOOD FIT — MODEL THE LOGIC)
- Read `HorizonStaking._slash()` source, extract the if/else logic, inline it in check_
- Verify: when `slashAmount <= providerStake`, delegators are untouched; provider always drained first
- **DO NOT call `staking.slash()`** — model the conditional arithmetic inline.
- The function should be `pure`.

### 3. P-16: RAV Monotonicity (GOOD FIT — PURE COMPARISON)
- Read `PaymentsEscrow` source for the RAV update logic
- Verify: `newValueAggregate >= oldValueAggregate` — inline the comparison arithmetic
- The function should be `pure`.

### 4. P-19: Operator Value Extraction (MEDIUM — MODEL BALANCE CHANGE)
- Inline a simplified model of the operator action's token flow
- Verify: operator's modelled net token delta is <= 0 after a single action
- **DO NOT deploy a staking contract** — model the token accounting math directly.

### 5. P-1: Stake Conservation (SKIP OR SIMPLIFY)
- This requires full contract state — too complex for Halmos.
- If you attempt it, model ONLY the accounting math (delta in == delta out) as pure arithmetic.
- If you cannot write a meaningful pure-math check, skip this property and note it in the assessment.

## Halmos Limitations (be honest in output)

- Verification is BOUNDED — valid only up to the configured loop/input bounds
- Cross-contract paths through proxies may cause Z3 to explode
- State-heavy tests (many storage slots) increase solving time dramatically
- If a test takes > 30 minutes, mark it as TIMEOUT and recommend fuzzing instead

## Before You Start — COMPILATION IS MANDATORY

1. Read `remappings.txt` in the project root — these are the ONLY valid import paths
2. Read `foundry.toml` — check `src`, `test`, `libs`, `solc` settings
3. Read at least 2-3 existing test files in the `test/` directory — copy their exact import style, pragma version, and deployment patterns
4. Read `PROPERTIES.md` — the properties you're verifying
5. Read the source code of target contracts, focusing on the specific functions
6. Read `audit-workspace/recon/entry-points.json` for function signatures
7. Check if `halmos-cheatcodes` is available — if not, skip the `SymTest` import and use plain `forge-std/Test.sol`

**CRITICAL**: Your tests MUST compile with `forge build`. To ensure this:
- Copy import paths exactly from existing tests — do NOT guess import paths
- Use the same pragma solidity version as existing tests
- Only import `forge-std/Test.sol` — do NOT import halmos-cheatcodes or any Graph contract
- Write ALL test files first, then run `forge build` ONCE — do not compile after each file
- If it fails, fix all errors in a single pass and recompile once more

## Output

Write symbolic test files to `audit-workspace/formal/`:
- `SymbolicFeeConservation.t.sol` — P-15
- `SymbolicSlashing.t.sol` — P-10
- `SymbolicOperator.t.sol` — P-19
- `SymbolicStakeConservation.t.sol` — P-1
- `SymbolicRAV.t.sol` — P-16

Every file must compile with `forge build`. If Halmos is not installed, the tests
should still compile — they just won't run symbolically.

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
