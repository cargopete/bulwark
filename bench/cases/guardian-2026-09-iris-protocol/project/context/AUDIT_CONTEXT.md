# Audit context: iris.credit Iris Protocol

The repository README at the audited commit. In scope:

- `src/Pod.sol`
- `src/adapters/AaveV3Adapter.sol`
- `src/adapters/MorphoBlueAdapter.sol`
- `src/blm/Blm.sol`
- `src/blm/WhitelistBlm.sol`

---

# Iris

Iris is a non-custodial fixed-rate, fixed-term lending protocol for the Ethereum Virtual Machine.
Rather than run its own rate market, it overlays fixed terms on variable-rate venues such as Morpho Blue and Aave V3, isolating each loan's collateral and debt in a single-use pod.
A solver signs an off-chain quote and a borrower accepts it, fixing the borrower's rate to maturity while the venue keeps charging a floating rate; the solver posts a bond that covers the spread between the two legs and, in return, earns that spread plus the collateral's venue yield, net of a protocol fee.
Loans that pass maturity and their overdue period, and bonds that can no longer cover the floating leg, become permissionlessly liquidatable, and positions can be refinanced across whitelisted venues.
Governance, expected to be a timelock, enables venues, bond lock modules (BLMs), and market parameters.

## Documentation

A detailed description of the protocol is available at [docs.iris.credit](https://docs.iris.credit).
An illustrated overview is available in [this explainer thread](https://x.com/iris_credit/status/2052010260915982537).

## Developers

Compilation, testing and formatting are done with [Foundry](https://book.getfoundry.sh/getting-started/installation).
If of interest, [BaseTest.t.sol](./test/BaseTest.t.sol) contains a re-usable testing setup and useful helpers.

## Audits

Audits can be found in the [audits](./audits/) folder.
The [auditor guide](./audits/AUDITOR_GUIDE.md) maps the audit scope and the critical areas.

## Licenses

The primary license is the Business Source License 1.1 (BUSL-1.1), see [LICENSE](./LICENSE); it applies to [`src/Iris.sol`](./src/Iris.sol).
All other files are licensed under GPL-2.0-or-later (as indicated in their SPDX headers), see [LICENSE-SECONDARY](./LICENSE-SECONDARY): `src/Pod.sol`, `src/adapters`, `src/blm`, `src/interfaces`, `src/libraries`, `script`, and `test`.
