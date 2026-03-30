# Known Issues and Accepted Risks: The Graph Protocol

## Accepted Architecture Risks

### KI-1: Graph Council upgrade authority
The Graph Council multisig can upgrade any protocol contract via proxy.
This is accepted during the transition period. Endgame is renouncing
upgradeability (precedent: L2GraphToken upgradeability already renounced).
Do NOT flag proxy upgradeability as a finding.

### KI-2: Centralized arbitrator
The Arbitrator role in DisputeManager is a single address appointed by
the Graph Council. This is accepted. Do NOT flag arbitrator centralization.

### KI-3: Delegation ratio not enforced in Horizon
Unlike legacy (16x cap), Horizon does not enforce a delegation ratio.
Over-delegation causes reward dilution but does not break protocol safety.
This is by design.

### KI-4: Data service trust
Each data service is fully trusted as the slashing authority for its
provisions. A malicious data service could slash all provisioned stake.
Service providers accept this risk when creating provisions. Do NOT flag
data service slashing authority as a finding — it is the core design.

## Previously Reported and Fixed

### KI-5: Rounding errors in staking math (Fixed, $290K bounty)
Two rounding errors with potential for loss of funds or unclaimed yield.
Reported by @GregadETH via Immunefi, January 2024. Patched.
Watch for: New rounding errors in Horizon delegation pool math,
especially share-to-token conversions.

### KI-6: Missing chainID in TAP allocation proof (Fixed, Critical)
TAP receipts were replayable across chains. Fixed by adding chainID.
OpenZeppelin audit C-01. Watch for: Similar replay vectors in
GraphTallyCollector RAV signatures.

### KI-7: Vesting contract interaction with Escrow (Fixed, Critical)
Vesting contract users couldn't call redeem() in Escrow due to function
call restrictions. Watch for: Similar proxy/forwarding issues with
vesting contracts and new Horizon functions.

### KI-8: Backwards-incompatible dispute upgrade (Fixed, High)
Disputes created before DisputeManager upgrade became unresolvable
because DisputeStatus enum changed. Watch for: Similar enum/struct
changes in upgrades that affect in-flight state.

### KI-9: Storage gap issues (Fixed)
Missing storage gaps in GraphTokenGateway (Code4rena M-244).
Watch for: Incorrect gap sizes in new Horizon contracts.

## Areas Requiring Extra Scrutiny

### Focus: Delegation pool math
The previous $290K bounty targeted rounding. Horizon introduces
per-data-service delegation pools with share/token math similar to
ERC-4626 vaults. Check for:
- First-depositor inflation attacks (donation attack on empty pool)
- Rounding direction consistency (always favor protocol, never user)
- Accumulated rounding errors over many operations
- Zero-share minting on small delegations

### Focus: Slashing-delegation interaction
Horizon allows (but doesn't initially enable) delegator slashing.
Even if disabled at launch, the code path exists. Check for:
- Correct provider-first ordering under all edge cases
- Behavior when slash amount exactly equals provider stake
- Concurrent slash calls in the same block
- Interaction between slashing and thawing delegations

### Focus: PaymentsEscrow race conditions
Payers can thaw escrow deposits while indexers attempt to collect. Check for:
- Thaw-then-collect ordering issues
- Partial collection during thawing
- Re-deposit after thaw to reset state
