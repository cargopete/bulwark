# Invariant Suite Generator: Forge, Medusa and Echidna

You write ONE invariant suite for the protocol described in AUDIT_CONTEXT.md. The pipeline runs
it three ways: Forge's invariant runner, Medusa and Echidna. The pipeline compiles it; **you do
not run forge build**.

## YOUR ONLY JOB: WRITE FILES

You have Read, Write, Edit, Glob and Grep. No Bash. Write the files and stop.

## Steps

1. Read `AUDIT_CONTEXT.md`: contracts, tokens, roles, deployed addresses, trust model.
2. Read `PROPERTIES.md`: every P-XX entry is a property to encode.
3. Read the remappings and `foundry.toml` shown below: the only valid import paths and pragma.
4. Read one existing test for its import style and deployment helpers.
5. Read `audit-workspace/recon/entry-points.json` for exact function signatures.
6. Write the five files below to the output directory, then stop.

## Layout (exactly these files and contract names)

Medusa and Echidna deploy `CryticTester` and never call `setUp()`, so all setup runs in its
constructor. Forge runs `BulwarkInvariants`, which deploys a `CryticTester` and asserts each
property, because Forge ignores a property that returns `false`.

| File | Contract | Contents |
|---|---|---|
| `BulwarkSetup.sol` | `abstract contract BulwarkSetup` | protocol references, actors, `_setup()` |
| `BulwarkTargets.sol` | `abstract contract BulwarkTargets is BulwarkSetup` | handlers, `target_*` |
| `BulwarkProperties.sol` | `abstract contract BulwarkProperties is BulwarkTargets` | `invariant_*` returning `bool` |
| `CryticTester.sol` | `contract CryticTester is BulwarkProperties` | `constructor() payable { _setup(); }` |
| `BulwarkInvariants.t.sol` | `contract BulwarkInvariants is Test` | Forge entry |

### Cheatcodes

Only `prank`, `startPrank`, `stopPrank`, `warp` and `roll`: all three fuzzers support these.
In the non-Forge files use the cheatcode address directly, not forge-std's `Test`:

```solidity
import {Vm} from "forge-std/Vm.sol";
abstract contract BulwarkSetup {
    Vm internal constant vm = Vm(0x7109709ECfa91a80626fF3989D68f67F5b1DD12D);
}
```

No `deal`, `bound`, `assume`, `expectRevert` or `store` in the suite; clamp inputs yourself
(`x = lo + (x % (hi - lo + 1))`).

### Actors

Three kinds, picked by a seed argument in each handler:

- **attacker** and **honest user** addresses, funded in `_setup()` through the protocol's own
  mint or deposit paths (local) or by pranking a real holder and transferring (fork);
- **privileged roles** (owner, operator, keeper) only for the routine actions the program treats
  as in scope: rebase, harvest, process a queue, update an oracle within its documented bounds.
  Never upgrades, never arbitrary parameter changes, never anything the trust model calls a
  governance decision.

### Handlers (`BulwarkTargets`)

- One `target_<action>(uint256 actorSeed, ...)` per state-changing entry point a real user or
  routine role can call. Clamp every amount to a realistic range and the caller's balance.
- `vm.prank(actor);` then call the real entry point directly. Let it revert; a revert is not a
  failure. Do not set state directly and do not skip checks the real entry point enforces.
- `target_warp(uint256 secs)` moving time by 1 second to 30 days with `vm.warp`, and `vm.roll`
  in step.
- Keep ghost variables (sums of deposits, withdrawals) when a property needs them, updated only
  after a call succeeds.

### Properties (`BulwarkProperties`)

- `function invariant_P<id>_<what>() public view returns (bool)` per PROPERTIES.md entry, e.g.
  `invariant_P2_backing`. One claim each. Use the P-number from PROPERTIES.md without the dash.
- Return `false` when the property is broken. Do not revert on purpose.
- Encode what the protocol promises, with its documented rounding tolerance, not an idealised
  version of it.

### Forge entry (`BulwarkInvariants.t.sol`)

```solidity
import "forge-std/Test.sol";
import {CryticTester} from "./CryticTester.sol";

contract BulwarkInvariants is Test {
    CryticTester internal tester;

    function setUp() public {
        tester = new CryticTester();
        targetContract(address(tester));
        // targetSelector with the target_* selectors only
    }

    function invariant_P2_backing() public view {
        assertTrue(tester.invariant_P2_backing(), "P-2 backing");
    }
    // one wrapper per property, same name
}
```

## Rules

1. Copy imports from existing tests exactly; wrong imports are compile failures.
2. Use the same pragma as the existing tests.
3. Do not inherit from the project's own handlers or test bases.
4. If a signature is unclear, read the source; never guess.
5. Contract and file names exactly as in the layout table.

## Context files

- `AUDIT_CONTEXT.md`: protocol overview and addresses
- `PROPERTIES.md`: the properties, your primary input
- `audit-workspace/recon/entry-points.json`: function signatures
- `audit-workspace/recon/storage-layouts.json`: state variable names
