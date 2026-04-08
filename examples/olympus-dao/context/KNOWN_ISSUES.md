# OlympusDAO — Known Issues & Accepted Risks

## Out of Scope (per Immunefi rules)

- Bugs in third-party oracle contracts (Chainlink, Band) — not Olympus-controlled.
- Bugs in Bond Protocol contracts — out of scope.
- Economic attacks requiring >$10M capital to execute (not practically exploitable).
- Any issue already disclosed in a previous audit report (see https://docs.olympusdao.finance/main/security/audits).
- Governance/multisig key compromise — trusted actor assumption.
- Mainnet testing — PoC must use a local Foundry fork only.
- Front-running of non-atomic transactions where the expected behaviour is already documented.

## Accepted Protocol Risks

- **Guardian pause risk**: The guardian multisig can deactivate Operator. This is intentional — it is the emergency brake. Finding that guardian can pause the protocol is not a valid submission.
- **Executor upgrade power**: The executor multisig can swap modules. This is a known centralisation trade-off. Bugs that require executor cooperation are not in scope.
- **Heart keeper centralization**: The Heart can be run by any `heart`-role address. Liveness depends on keeper uptime. This is documented and accepted.
- **OHM price volatility on payout**: Immunefi pays in OHM. Price risk on payout is not a protocol bug.
- **Low TVL ceiling**: Current TVL ~$261k limits max payout for economic exploits. This is a factual constraint, not an issue.

## Previously Audited / Fixed

- reentrancy in early Cooler implementations (fixed in v2)
- integer overflow in BondCallback pre-Solidity 0.8 (no longer applicable)
- stale price in PRICE module single-observation edge case (fixed; minimum 2 observations enforced)

## Notes for Agents

- The `Kernel.executeAction()` function handles 5 action types: `InstallModule`, `UpgradeModule`,
  `ActivatePolicy`, `DeactivatePolicy`, `ChangeExecutor`. Focus on edge cases around
  `UpgradeModule` — it is the most complex and least tested transition.
- `ClearingHouse` holds a large DAI balance earmarked for Cooler loans. It is the largest
  single-contract fund concentration outside the core TRSRY.
- Cross-chain deployments have identical bytecode but different oracle feeds; a PRICE bug
  might be unexploitable on mainnet but exploitable on a thin-liquidity chain.
