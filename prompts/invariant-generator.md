# Invariant Test Generator — Foundry Fuzzing

> **⚠ CRITICAL NAMING RULES — FOUNDRY WILL SILENTLY IGNORE TESTS WITH WRONG NAMES**
>
> - Invariant test functions MUST start with **`invariant_`** exactly (lowercase)
> - Test contracts MUST inherit from `Test` (from `forge-std/Test.sol`)
> - Test contracts MUST be named with `Invariant` in the name for easy discovery
> - Do NOT use `test_` prefix — that runs unit tests, not invariant tests
> - Do NOT use `check_` prefix — that is for Halmos symbolic tests
> - Example: `function invariant_P1_stake_conservation() public view { ... }`

You are a Foundry invariant test engineer. Your job is to generate invariant tests
that fuzz the Graph Protocol contracts against the security properties in PROPERTIES.md.

## Rules

1. Every test MUST compile. Non-compiling tests are useless.
2. Use Foundry's native invariant testing framework (`invariant_` prefix functions).
3. Write handler contracts that expose the fuzzable actions (delegate, undelegate, slash, collect, etc.).
4. Each invariant function asserts one property. Name them clearly: `invariant_P1_stake_conservation()`.
5. Use `targetContract()` and `targetSelector()` to scope fuzzing to relevant functions.

## Foundry Invariant Test Structure

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.27;

import "forge-std/Test.sol";

// Handler exposes fuzzable actions
contract StakingHandler is Test {
    HorizonStaking staking;
    IERC20 grt;

    constructor(HorizonStaking _staking, IERC20 _grt) {
        staking = _staking;
        grt = _grt;
    }

    // Fuzzable actions — fuzzer calls these with random params
    function handler_delegate(uint256 amount) external {
        amount = bound(amount, 1e18, 1000000e18);  // Reasonable range
        deal(address(grt), msg.sender, amount);
        grt.approve(address(staking), amount);
        staking.delegate(sp, dataService, amount, 0);
    }

    function handler_undelegate(uint256 shares) external {
        // bound to available shares
        staking.undelegate(sp, dataService, shares);
    }
}

// Invariant test contract
contract InvariantStakingTest is Test {
    StakingHandler handler;

    function setUp() public {
        // Deploy or fork contracts
        // Create handler
        handler = new StakingHandler(staking, grt);
        targetContract(address(handler));
    }

    // P-1: Stake conservation
    function invariant_P1_stake_conservation() public view {
        uint256 contractBalance = grt.balanceOf(address(staking));
        uint256 accountedTotal = staking.getTotalStaked();
        assertEq(contractBalance, accountedTotal, "P-1: stake not conserved");
    }

    // P-5: Pool share consistency
    function invariant_P5_share_consistency() public view {
        (uint256 shares, uint256 tokens) = staking.getDelegationPool(sp, dataService);
        if (shares > 0) {
            assertGt(tokens, 0, "P-5: shares > 0 but tokens == 0");
        }
    }
}
```

## Properties to Generate Tests For

Generate invariant tests for these properties (read PROPERTIES.md for full descriptions):

### Must-have (generate these)

| Property | Invariant | Handler Actions |
|----------|-----------|-----------------|
| P-1 | `grt.balanceOf(staking) == totalAccountedStake` | stake, unstake, slash, delegate, undelegate |
| P-5 | `shares > 0 implies tokens > 0` | delegate, undelegate, slash |
| P-6 | `sharePrice` never decreases except via slash | delegate, undelegate, collectFees |
| P-10 | Provider stake decreases before delegator pool | slash with varying amounts |
| P-14 | `grt.balanceOf(escrow) >= sumOfDeposits` | deposit, thaw, withdraw, collect |
| P-15 | `protocolTax + cuts + receiver == total` | collect with varying amounts and fee rates |
| P-19 | Operator GRT balance never increases | all operator-callable functions |

### Graph-Specific Fuzzing Targets

| Target | What to Fuzz | Assertion |
|--------|-------------|-----------|
| Delegation cycling | Rapid delegate/undelegate sequences | Share price drift < threshold |
| Slash during thaw | Interleave thaw() and slash() | P-10 holds regardless of ordering |
| Escrow race | Interleave thaw/deposit/collect on escrow | P-14 solvency holds |
| Multi-provision | Create provisions across data services, slash | No cross-service interference |
| Operator sequences | All operator actions in random order | No value extraction |

## Before You Start — COMPILATION IS MANDATORY

1. Read `remappings.txt` in the project root — these are the ONLY valid import paths
2. Read `foundry.toml` — check `src`, `test`, `libs`, `solc` settings
3. Read at least 2-3 existing test files in the `test/` directory — copy their exact import style, pragma version, and deployment patterns
4. Read `PROPERTIES.md` — the invariants you're testing
5. Read `audit-workspace/recon/entry-points.json` — function signatures for handlers
6. Read `audit-workspace/recon/storage-layouts.json` — understand state structure

**CRITICAL**: Your tests MUST compile with `forge build`. To ensure this:
- Copy import paths exactly from existing tests — do NOT guess import paths
- Use the same pragma solidity version as existing tests
- Use the same base contracts and deployment helpers as existing tests
- After writing each file, run `forge build` to verify it compiles before moving on
- If it fails, read the error, fix the imports, and try again

## Output

Write invariant test files to `audit-workspace/fuzzing/invariant-tests/`.

Create separate files for each domain:
- `InvariantStaking.t.sol` — P-1, P-4
- `InvariantDelegation.t.sol` — P-5, P-6, P-7, P-9
- `InvariantSlashing.t.sol` — P-10, P-11, P-13
- `InvariantPayments.t.sol` — P-14, P-15
- `InvariantOperator.t.sol` — P-19, P-20
- `Handler.sol` — shared handler contract with all fuzzable actions

Use `deal()` for token setup, `vm.prank()` for callers.
If a contract interface is unclear, read the source first — do not guess function signatures.
