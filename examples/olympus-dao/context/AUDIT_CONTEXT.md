# OlympusDAO (olympus-v3) — Audit Context

## Protocol Overview

Olympus is a treasury-backed monetary system centred on OHM, a decentralised reserve currency.
The v3 architecture ("Bophades") uses a **modular kernel** (Default Framework) where pluggable
modules handle distinct protocol functions and stateless policies orchestrate them.

Immunefi bounty: up to **$3,333,333** for treasury extraction bugs (10% of funds at risk).
Current TVL ~$261k, paid in OHM. Deployed on 7 chains (Ethereum mainnet is primary).

## Architecture — Bophades Kernel

### Kernel (`src/Kernel.sol`)
Central registry. Modules register with a 5-byte keycode (e.g. `TRSRY`). Policies request
permissions to call specific module functions. The kernel executor can install/upgrade modules
and approve/revoke policies. **Module upgrades are live and race-condition-prone.**

### Modules (`src/modules/`)

| Keycode | Contract | Role |
|---------|----------|------|
| TRSRY   | OlympusTreasury | Holds all protocol assets; withdrawals gated by ROLES |
| MINTR   | OlympusMinter | Mints/burns OHM; only callable by approved policies |
| PRICE   | OlympusPrice | TWAP oracle for OHM; feeds into RBS decisions |
| RANGE   | OlympusRange | Range Bound Stability walls/cushions — upper/lower price bands |
| ROLES   | OlympusRoles | ACL — maps role bytes32 → address → bool |

### Policies (`src/policies/`)

| Contract | Role |
|----------|------|
| Operator | Executes RBS swaps; calls TRSRY/MINTR/RANGE/PRICE |
| Heart | Keeper — triggers periodic protocol actions (rebase, price update) |
| BondCallback | Handles bond market callbacks; credits TRSRY |
| TreasuryCustodian | Admin-facing TRSRY management (grants/revokes withdrawer roles) |
| RolesAdmin | Grants/revokes ROLES entries |
| Distributor | OHM rebase distribution |

### Cooler Loans (`src/external/cooler/`)

| Contract | Role |
|----------|------|
| Cooler | Per-user lending vault — gOHM collateral, DAI/USDC loans |
| CoolerFactory | Deploys Cooler instances |
| MonoCooler | Successor to ClearingHouse — protocol-side singleton lending vault; holds USDS; `treasuryBorrower` routes fund flows |

## Trust Model

- **Executor (multisig)**: Can install/upgrade modules, approve/revoke policies. Highest privilege.
- **guardian (ROLES)**: Can deactivate Operator emergency.
- **policy (ROLES)**: Operator, Heart, BondCallback — can call sensitive module functions.
- **heart (ROLES)**: Beats the Heart keeper.
- **cooler_overseer (ROLES)**: Can rebalance MonoCooler, defund it.
- **Users**: Interact with Cooler directly. No trusted role.

**Critical invariant**: Only the kernel executor can grant module install/upgrade. A policy that
gains executor-equivalent power breaks the entire system.

## Attack Surface

### High-value targets

1. **Treasury extraction via TRSRY** — `withdrawReserves()`, `getLoan()`, `repayLoan()` in
   ClearingHouse; any path that moves assets out of TRSRY without proper ROLES check.

2. **ROLES bypass** — `OlympusRoles` maps `hasRole[role][addr]`. If a policy can grant itself
   a role it shouldn't have, it can escalate to treasury withdrawal.

3. **Price oracle manipulation (PRICE)** — `OlympusPrice` uses a TWAP. If the observation array
   can be stuffed or the moving average manipulated cheaply, RBS decisions are corrupted.
   Operator executes large swaps based on PRICE output — wrong price → drained reserves.

4. **MonoCooler `treasuryBorrower` initialisation** — `setTreasuryBorrower()` only enforces
   admin role if `treasuryBorrower != address(0)`. First call is permissionless — race between
   deployment and initialisation lets attacker inject a malicious borrower, redirecting all USDS.

5. **Cooler liquidation math** — `Cooler.repayLoan()`, `claimDefaulted()`. Incorrect collateral
   accounting allows under-collateralised loans or collateral theft.

6. **Module upgrade race condition** — during `Kernel._upgradeModule()`, the old module is
   deactivated and new one installed. If a policy holds stale module references, the window
   between deactivation and re-permissioning is exploitable.

6. **BondCallback reentrancy / double-credit** — `BondCallback.callback()` credits TRSRY.
   Reentrancy or replay could double-credit reserves, inflating OHM backing incorrectly.

7. **Cross-chain consistency** — same contracts on Arbitrum, Base, etc. May have different
   liquidity/oracle conditions; a bug cheaper to exploit on a low-liquidity chain.

## Economic Parameters

- OHM backing ratio: ~1 DAI equivalent per OHM (floor).
- Cooler loans: LTV typically 70% gOHM collateral → DAI loan; interest accrues per second.
- RBS cushion/wall prices set relative to moving average; spread configurable by guardian.
- Bond markets: external (Bond Protocol); BondCallback bridges payouts back to TRSRY.
- Distributor rebases: rewards minted by MINTR on each epoch (Heart beat).

## Deployment

- Mainnet: primary. Fork block for PoC should be recent mainnet.
- Also: Arbitrum, Base, Fantom, Polygon, Avalanche, BNB Chain.
- Contract addresses: see `deploy/` directory in repo or `deployments.json`.

## Previous Audits

Read all reports before writing PoC. Any bug already reported is ineligible.
Audit list: https://docs.olympusdao.finance/main/security/audits

Known auditors: Sherlock, Code4rena, Omniscia, Electisec (formerly yAudit), Spearbit.
Focus on what was NOT found — novel cross-module interactions are the gap.
