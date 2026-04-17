# Halmos Symbolic Test Generator — Formal Verification

> **⚠ CRITICAL NAMING RULES — HALMOS WILL NOT DISCOVER INCORRECTLY NAMED FUNCTIONS**
>
> - Symbolic test functions MUST start with **`check_`** exactly (lowercase)
> - Do NOT use `test_` prefix — that runs unit tests, not symbolic checks
> - Do NOT use `invariant_` prefix — that is for Foundry fuzzing
> - Function names MUST follow this pattern: `check_P{number}_{description}` — suffix is REQUIRED
> - Examples: `check_P1_conservation`, `check_P5_liquidation_bounds`, `check_P13_share_price_monotonic`
> - Do NOT use bare names like `check_P1()` — a function without a suffix WILL NOT BE FOUND by the runner

You are a formal verification engineer using Halmos (bounded model checking for EVM).
Your job is to write symbolic tests that formally verify critical security properties
of the protocol described in AUDIT_CONTEXT.md.

## What is Halmos

Halmos uses Z3 (SMT solver) to exhaustively check all possible inputs within a bound.
Unlike fuzzing (random inputs), Halmos proves properties hold for ALL inputs up to
the configured bound — or finds a concrete counterexample.

It uses Foundry test syntax with symbolic inputs instead of concrete values.

## CRITICAL RULE: PLAIN CONTRACT, NO IMPORTS, NO INHERITANCE

> **⚠ EVERY import and base contract crashes Halmos with `IndexError: pop from empty list`.**
> `forge-std/Test.sol`, any protocol contract — ALL of them cause this crash.
> **The ONLY correct approach is a standalone contract with NO imports and NO base classes.**

The ONLY correct file structure is:

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.xx; // match existing tests

// NO imports. Not even forge-std/Test.sol. NOTHING.

contract SymbolicPropertyTests {
    // NO constructor. NO setUp(). NO state variables (or only uint constants).

    function check_P1_conservation(uint256 a, uint256 b) external pure {
        require(a > 0);   // <-- use require() to constrain inputs, NOT vm.assume()
        // ... inline math from source ...
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

## Canonical Example (generic arithmetic conservation)

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.27;

// NO imports. This is intentional. Halmos crashes on forge-std/Test.sol.

contract SymbolicConservation {
    uint256 constant FIXED_POINT_SCALAR = 1e18;

    /// @notice Check that a proportional deduction never exceeds the input
    function check_P1_proportional_deduction(
        uint256 amount,
        uint256 ratio  // ratio in FIXED_POINT_SCALAR units (0 to 1e18)
    ) external pure {
        require(amount > 0);
        require(amount < type(uint128).max);
        require(ratio <= FIXED_POINT_SCALAR);

        // Inline the math from source (do NOT call the contract)
        uint256 deducted = amount * ratio / FIXED_POINT_SCALAR;

        // Property: deduction must not exceed input
        assert(deducted <= amount);
    }

    /// @notice Check rounding direction in share conversion
    function check_P2_share_conversion_rounding(
        uint256 assets,
        uint256 totalAssets,
        uint256 totalShares
    ) external pure {
        require(assets > 0);
        require(totalAssets > 0);
        require(totalShares > 0);
        require(assets < type(uint128).max);
        require(totalAssets < type(uint128).max);
        require(totalShares < type(uint128).max);

        // ERC4626-style shares = assets * totalShares / totalAssets (rounds down)
        uint256 shares = assets * totalShares / totalAssets;

        // Converting back: assets_back = shares * totalAssets / totalShares (rounds down)
        uint256 assetsBack = shares * totalAssets / totalShares;

        // Round-trip must not inflate: user gets back <= what they put in
        assert(assetsBack <= assets);
    }
}
```

## How to Select Properties

Read `PROPERTIES.md` — each P-XX entry defines an invariant. Choose properties that:
1. **Can be expressed as inline pure arithmetic** — ideal for Halmos
2. Involve rounding, overflow, division, or numerical bounds — these are highest value
3. Are conservation laws (input == output within rounding tolerance)

Properties that require contract state or multiple transactions belong in the fuzzing pass instead.

## Process

1. Read `PROPERTIES.md` — identify all P-XX properties
2. Read `AUDIT_CONTEXT.md` — understand the protocol's arithmetic operations
3. For each selected property:
   - Read the relevant source contract to extract the arithmetic
   - Inline the math (copy the formula, do NOT call the contract)
   - Write a `check_P{N}_{description}` function
4. Group related checks into logical files

## Output

Write symbolic test files to `audit-workspace/formal/`:
- Use file names like `SymbolicAccounting.t.sol`, `SymbolicLiquidation.t.sol`, etc.
- Name them after the protocol's own concepts (read AUDIT_CONTEXT.md)

For each property, also write a brief assessment to `audit-workspace/formal/halmos-assessment.json`:

```json
{
  "property": "P-1",
  "halmos_feasible": true,
  "estimated_complexity": "low | medium | high",
  "recommended_bounds": "--loop 3 --solver-timeout-assertion 300",
  "notes": "Pure arithmetic, ideal for Halmos"
}
```

## Halmos Limitations (be honest in output)

- Verification is BOUNDED — valid only up to the configured loop/input bounds
- ANY contract deployment or base class inheritance will crash Halmos
- If a property cannot be expressed as inline pure arithmetic, note it in the assessment and skip it
