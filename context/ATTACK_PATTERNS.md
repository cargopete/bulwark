# Attack Patterns Database

Known vulnerability patterns derived from previous Graph Protocol audits, bounty payouts,
and public findings. Agents MUST search for **variants** of these patterns in the current code.

---

## AP-1: Delegation Pool Rounding Exploitation

**Source**: KI-5 — $290,000 Immunefi bounty payout

**Pattern**: Integer division in share-to-token or token-to-share conversions rounds in a
direction favourable to the user rather than the protocol. An attacker cycles
delegate/undelegate operations to accumulate rounding profits.

**What to look for**:
- Any `tokensToShares()` or `sharesToTokens()` conversion
- Division operations where the remainder is silently discarded
- `mulDiv` usage without explicit rounding direction
- Missing minimum delegation amounts that would make rounding unprofitable
- Share price manipulation via donation (first-depositor attack on empty pools)

**Accumulation model**: Calculate profit per operation, multiply by 1,000 / 10,000 / 100,000
repetitions, subtract gas costs. If net profitable at any scale, this is a finding.

---

## AP-2: Missing Chain ID in Signature Verification

**Source**: KI-6 — Previously fixed in GraphTallyCollector

**Pattern**: Signed messages (EIP-712 or otherwise) omit the chain ID from the domain
separator or signed payload, allowing replay across chains (e.g., mainnet signature
replayed on Arbitrum).

**What to look for**:
- EIP-712 domain separators — verify `chainId` is included
- Any `ecrecover` or signature verification — check all signed fields
- RAV (Receipt Aggregate Voucher) signatures specifically
- Allocation ID signatures
- Any message signed off-chain and verified on-chain

---

## AP-3: Proxy Forwarding to Unintended Implementation

**Source**: KI-7 — Vesting contract could forward calls to wrong implementation

**Pattern**: Proxy contracts that use `delegatecall` to forward execution may target the
wrong implementation address if the storage slot for the implementation is not properly
protected, or if a proxy can be pointed at an arbitrary address.

**What to look for**:
- `delegatecall` targets — are they from trusted storage slots?
- Implementation address setters — who can call them?
- Initialisation sequences — can implementation be set before/during init?
- Transparent proxy vs UUPS — which pattern, and is the admin slot safe?
- Storage collisions between proxy and implementation

---

## AP-4: Backwards-Incompatible Enum Changes

**Source**: KI-8 — Enum member reordering broke existing storage values

**Pattern**: Adding or reordering members in a Solidity `enum` changes the integer values
of existing members. If the enum is stored in contract storage (directly or as part of a
struct), upgrading the implementation silently reinterprets all stored values.

**What to look for**:
- Any `enum` definitions in upgradeable contracts
- Compare current enum member ordering against prior versions
- Enums stored in mappings or arrays (higher blast radius)
- New enum members added anywhere except the END of the definition

---

## AP-5: Storage Gap Miscalculation in Upgradeable Contracts

**Source**: Code4rena M-244, common across proxy patterns

**Pattern**: Upgradeable contracts use `__gap` storage arrays to reserve space for future
variables. If the gap size is miscalculated, adding new state variables in an upgrade
collides with storage in child contracts.

**What to look for**:
- `uint256[N] private __gap` declarations — verify N + used_slots == expected total
- Inheritance chains — verify gaps account for all parent contracts
- New state variables added in the contract body ABOVE the gap declaration
- Multiple inheritance paths to the same base (diamond storage collision)

---

## AP-6: Slashing Race Condition with Thawing

**Source**: Design analysis of HorizonStaking slash/thaw interaction

**Pattern**: A service provider sees an incoming slash transaction in the mempool and
front-runs it by initiating thaw on their provision. If thawing tokens are excluded from
the slashable balance, the provider reduces their loss at the expense of delegators.

**What to look for**:
- Does `slash()` include thawing tokens in the slashable balance?
- Can `thaw()` be called in the same block as `slash()`?
- Is there a thawing period enforced before tokens become unslashable?
- Provider-first ordering (P-10) — does the provider absorb losses before delegators?
- Concurrent slash operations in the same block

---

## AP-7: Escrow Thaw-Collect Race

**Source**: Design analysis of PaymentsEscrow

**Pattern**: A payer initiates thaw on escrowed funds while a collector attempts to collect
payment. Depending on ordering, the collector may find the funds already withdrawn, or the
payer may lose funds they expected to recover.

**What to look for**:
- State transitions during thawing — what is the escrow state between thaw and withdraw?
- Can `collect()` succeed during the thawing period?
- Can a payer re-deposit after initiating thaw to reset the thaw timer?
- Block timestamp manipulation effects on thaw period
- Partial collection during thawing

---

## AP-8: RAV Replay Across Data Services

**Source**: Design analysis of GraphTallyCollector

**Pattern**: A RAV (Receipt Aggregate Voucher) signed for one data service is replayed
against a different data service that accepts the same collector.

**What to look for**:
- RAV signed fields — does the signature cover the data service address?
- Can the same RAV be submitted to multiple data services?
- valueAggregate monotonicity (P-16) — is it enforced per (payer, SP, dataService)?
- Signature verification — does it verify all relevant fields including service address?

---

## AP-9: Operator Privilege Escalation

**Source**: Property P-19, P-20 security analysis

**Pattern**: An operator, who should only be able to manage provisions on behalf of a
service provider, finds a sequence of function calls that allows them to extract tokens
or gain control beyond their intended scope.

**What to look for**:
- All functions callable by operators — enumerate the complete list
- Can an operator set themselves as a beneficiary anywhere?
- Can an operator create provisions that redirect rewards?
- Operator scope isolation — can actions on one data service affect another (P-20)?
- Can an operator manipulate delegation pool state?

---

## AP-10: First-Depositor / Inflation Attack on Pools

**Source**: Common DeFi pattern, relevant to delegation pools

**Pattern**: The first depositor in a share-based pool deposits a tiny amount, then
donates a large amount directly to the pool contract. Subsequent depositors receive
fewer shares than expected due to the inflated share price, and the first depositor
extracts the difference.

**What to look for**:
- Empty pool state — what happens on the first delegation?
- Minimum delegation amounts — do they prevent economically viable inflation attacks?
- Direct token transfers to the pool contract — does the accounting handle them?
- Share price calculation when total shares or total tokens is very small
