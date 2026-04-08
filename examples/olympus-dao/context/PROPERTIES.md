# OlympusDAO — Invariants & Properties

## Treasury (TRSRY)

- **P-01**: `totalDebt[token] ≤ reserveBalance[token]` — debt can never exceed reserves held.
- **P-02**: Only addresses holding the `withdrawer` role (via ROLES) can call `withdrawReserves()`.
- **P-03**: `withdrawReserves(token, to, amount)` must reduce `reserveBalance[token]` by exactly `amount`.
- **P-04**: `getLoan()/repayLoan()` in ClearingHouse must never allow net asset extraction from TRSRY across a round-trip.

## Minter (MINTR)

- **P-05**: `mintOhm(to, amount)` can only be called by policies with explicit `MINTR.mintOhm` permission.
- **P-06**: `burnOhm(from, amount)` can only be called by policies with explicit `MINTR.burnOhm` permission.
- **P-07**: OHM total supply after any sequence of mint/burn must equal initial supply + net minted by authorised policies only.

## Roles (ROLES)

- **P-08**: `hasRole[role][addr]` can only be set to `true` by `RolesAdmin` or the kernel executor.
- **P-09**: No policy can grant itself a role it does not already hold.
- **P-10**: `revokeRole()` must set `hasRole[role][addr] = false` unconditionally.

## Kernel

- **P-11**: Only `executor` can call `executeAction()`.
- **P-12**: A module keycode can only be active for one contract at a time.
- **P-13**: After `_upgradeModule(newModule)`, all previous permissions for the old module keycode must be re-granted to the new module address only if explicitly re-permissioned.
- **P-14**: A policy that has been deactivated cannot call any permissioned module function.

## Price Oracle (PRICE)

- **P-15**: `getCurrentPrice()` must return a value within configurable deviation bounds vs. the Chainlink spot price.
- **P-16**: The moving average cannot be updated more frequently than one observation per minimum observation frequency.
- **P-17**: An attacker manipulating a single block cannot shift the moving average by more than `1/numObservations` of its total weight.

## Range (RBS)

- **P-18**: `Operator.operate()` can only be triggered by an address holding the `heart` role.
- **P-19**: Cushion/wall spread must respect `minimumTargetPrice` and `maximumTargetPrice` bounds.
- **P-20**: Swap amounts during cushion/wall operations must not exceed the configured capacity.

## Cooler Loans

- **P-21**: After `claimDefaulted(loanId)`, the collateral released to the lender must equal exactly `loan.collateral` — no more, no less.
- **P-22**: `repayLoan(loanId, repaid)` must reduce `loan.amount` by `repaid / loan.request.interest`-adjusted principal, never by more than outstanding.
- **P-23**: A borrower cannot receive their collateral back before fully repaying the loan amount plus accrued interest.
- **P-24**: `ClearingHouse.lendToCooler()` must only lend up to `FUND_AMOUNT` and must record a corresponding receivable in TRSRY (`addReceivables`).
- **P-25**: Total outstanding ClearingHouse receivables must not exceed the DAI held in the ClearingHouse fund.

## BondCallback

- **P-26**: `BondCallback.callback(id, inputAmount, outputAmount)` can only be called by the registered bond auctioneer for market `id`.
- **P-27**: Each bond market callback can only be processed once per payout (no replay).
- **P-28**: `ohm_out` credited to TRSRY via BondCallback must equal exactly the OHM minted in the same transaction.
