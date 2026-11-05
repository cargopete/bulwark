# Audit context: BattleChain Confidence Pools

The contest README and protocol README at the audited commit, as contestants had them. In scope: `src/ConfidencePool.sol` and `src/ConfidencePoolFactory.sol`.

---

# BattleChain Confidence Pools

### Prize Pool

- Total Pool - 7.25 ETH
- H/M - 6 ETH
- Low - 1.25 ETH

- Starts: July 9th, 2026
- Ends: July 16th, 2026

- nSLOC: 589

[//]: # (contest-details-open)

## About the Project

Confidence Pools let sponsors bootstrap third-party confidence around an active Safe Harbor agreement on BattleChain. Stakers deposit capital, sponsors top up an optional bonus, and each pool settles based on a moderator-flagged outcome or an expiry backstop. The pool acts as an on-chain confidence mechanism: stakers economically signal their belief that the in-scope contracts will survive the agreement term without being corrupted, and are rewarded from the bonus pool if they do.

- A UUPS-upgradeable `ConfidencePoolFactory` deploys non-upgradeable `ConfidencePool` clones (one or many per agreement) and gates stake tokens behind an owner-controlled allowlist.
- Each clone holds all stake and bonus funds and commits to its own scope (a flat list of BattleChain accounts), locked permanently once the registry leaves pre-attack staging.
- Registry state is read live from `IBattleChainSafeHarborRegistry.getAttackRegistry()` and never cached.
- Resolution paths: SURVIVED (stake + k=2 time-weighted bonus share), EXPIRED (same payout via the expiry backstop), and CORRUPTED (good-faith attacker bounty, or bad-faith full-pool sweep to the recovery address).
- The bonus is split with a k=2 time-weighted formula that crushes late entrants within the observed risk window.

> **Auditors:** design decisions, trust assumptions, and known/intentional behavior (and why several common findings are false positives) are documented in `docs/DESIGN.md`. Check a suspected finding against it before reporting.

[Documentation](docs/DESIGN.md)
[BattleChain Safe Harbor Contracts](https://github.com/Cyfrin/battlechain-safe-harbor-contracts)
[BattleChain Docs](https://docs.battlechain.com)

## Actors

There are 5 main actors in this protocol:

**1. Factory Owner**

RESPONSIBILITIES:

- deploys the factory and pool implementation,
- manages the Safe Harbor registry and default moderator addresses,
- controls the stake-token allowlist,
- can pause / unpause the factory,
- authorizes UUPS upgrades.

LIMITATIONS:

- two-step ownership transfer (`Ownable2Step`),
- cannot move funds held by pool clones.

**2. Pool Sponsor (Pool Owner)**

RESPONSIBILITIES:

- creates a pool via the factory with an initial scope and an allowlisted stake token,
- controls `recoveryAddress` (CORRUPTED sweep destination),
- controls `expiry` (only until the first stake),
- controls pool scope (only until the registry leaves pre-attack staging),
- can pause / unpause the pool's stake/bonus paths.

LIMITATIONS:

- cannot alter scope once locked,
- cannot alter `expiry` after the first stake,
- pause does not affect resolution/withdrawal paths.

**3. Moderator (Protocol DAO)**

RESPONSIBILITIES:

- flags the pool outcome (SURVIVED / CORRUPTED / EXPIRED) via `flagOutcome`,
- may re-flag a correction before the first claim,
- names the whitehat attacker for good-faith CORRUPTED.

LIMITATIONS:

- set by the factory at clone time and immutable per-pool,
- correction window closes on first claim (finality is value-movement),
- 180-day grace fallback exists as defense against moderator unavailability.

**4. Staker**

RESPONSIBILITIES:

- deposits stake via `stake` (counts toward the bonus formula immediately),
- can fully exit via `withdraw` while the registry is in any pre-attack state,
- claims stake + k=2 time-weighted bonus share via `claimSurvived` / `claimExpired`.

LIMITATIONS:

- withdrawals permanently disabled from `UNDER_ATTACK` onward,
- withdrawing forfeits any bonus claim,
- no owner- or moderator-defined rights.

**5. Bonus Contributor / Attacker**

RESPONSIBILITIES:

- Bonus Contributor: can contribute to the bonus pool permissionlessly via `contributeBonus` (no claim rights),
- Attacker (good-faith CORRUPTED only): named by the moderator, has 180 days to claim the bounty via `claimAttackerBounty`.

LIMITATIONS:

- bonus contributors gain no claim on stake or bonus,
- attacker bounty capped at the snapshot of total staked + total bonus.

[//]: # (contest-details-close)

[//]: # (scope-open)

## Scope (contracts)

```
src/
├── ConfidencePool.sol
├── ConfidencePoolFactory.sol
```

BattleChain Safe Harbor interfaces (`IAgreement`, `IAttackRegistry`, `IBattleChainSafeHarborRegistry`) are pulled in as a git submodule at `lib/battlechain-safe-harbor-contracts` and are out of scope (dependency).

## Compatibilities

```
Compatibilities:

  Blockchains:
      - BattleChain (EVM-compatible L2)
  Solidity:
      - 0.8.26 (via-IR, optimizer 200 runs)
  Tokens:
      - ERC20 (standard only; fee-on-transfer and rebasing tokens are NOT supported)
```

[//]: # (scope-close)


[//]: # (getting-started-open)

## Setup

Build:

```
git clone https://github.com/CodeHawks-Contests/2026-07-bc-confidence-pools.git

forge install

forge build
```

Tests:

```
forge test

# fork tests (requires a BattleChain RPC)
forge test --match-path 'test/fork/*' --rpc-url battlechain_testnet
```

RPC endpoints:

`foundry.toml` maps the `battlechain_testnet` / `battlechain_mainnet` aliases to the
`BATTLECHAIN_TESTNET_RPC` / `BATTLECHAIN_MAINNET_RPC` env vars (see `.env.example`).
The fork tests under `test/fork/` resolve `battlechain_testnet` to `BATTLECHAIN_TESTNET_RPC`.

```
BATTLECHAIN_TESTNET_RPC=https://testnet.battlechain.com
BATTLECHAIN_MAINNET_RPC=https://mainnet.battlechain.com
```

[//]: # (getting-started-close)

[//]: # (known-issues-open)

## Known Issues

Known/intentional behaviors and trust assumptions are documented in `docs/DESIGN.md`, which also explains why several common findings are false positives. Check a suspected finding against it before reporting.

[//]: # (known-issues-close)


---

# BattleChain Confidence Pools

Confidence Pools let sponsors bootstrap third-party confidence around an active Safe Harbor agreement: stakers deposit, sponsors top up a bonus, and the pool settles based on a moderator-flagged outcome or an expiry backstop.

Bonus liquidity is optional at the protocol level but economically required for rational staker participation.

> **Auditors:** design decisions, trust assumptions, and known/intentional behavior (and why several common findings are false positives) are documented in [`docs/DESIGN.md`](docs/DESIGN.md). Check a suspected finding against it before reporting.

## Architecture

- `ConfidencePoolFactory` is UUPS-upgradeable. It creates non-upgradeable `ConfidencePool` clones (one or many per agreement) and gates stake tokens behind an owner-controlled allowlist.
- `ConfidencePool` clones hold all stake and bonus funds. Each clone commits to its own scope (a flat list of BattleChain accounts, a subset of the agreement's BattleChain scope), locked on the first interaction that observes the registry leaving pre-attack staging (`NOT_DEPLOYED` or `NEW_DEPLOYMENT`). The pool insures BattleChain-deployed contracts only — the underlying agreement may cover other chains off-chain, but the on-chain attack registry and the pool's scope validation are both BattleChain-native.
- Registry state is read live from `IBattleChainSafeHarborRegistry.getAttackRegistry()`; the pool never caches it.

## BattleChain interfaces

Pulled in as a git submodule at `lib/battlechain-safe-harbor-contracts` (source: `https://github.com/Cyfrin/battlechain-safe-harbor-contracts`) and imported via the `@battlechain/` remapping. The pool reads upstream registry state live through these.

- `@battlechain/interface/IAgreement.sol`
- `@battlechain/interface/IAttackRegistry.sol`
- `@battlechain/interface/IBattleChainSafeHarborRegistry.sol`

## Registry-state vocabulary

**Important for auditors:** the active-risk states `UNDER_ATTACK` and `PROMOTION_REQUESTED` mean the in-scope contracts are *legally attackable by whitehats* — the agreement is live and exposed, its normal operating mode. They do **not** mean a breach is in progress. Only the terminal `CORRUPTED` state is evidence that funds were lost. Several design choices below follow from this and are common sources of false-positive findings: resolving EXPIRED while the agreement is still attackable is the correct "it survived the term" payout (not an escape), and deposits are deliberately allowed during `UNDER_ATTACK` (voluntary risk capital that the k=2 weighting crushes to ~zero bonus, not a trap).

## Lifecycle

1. **Deploy.** Sponsor creates the pool via the factory with an initial scope and a stake token from the allowlist.
2. **Stake.** Stakers deposit until `expiry`. Stake counts toward the bonus formula immediately — no maturation cliff. Stakers can exit until the registry reaches `UNDER_ATTACK` (see step 4). Bonus contributions are permissionless.
3. **Risk window.** `riskWindowStart` is sealed on first observation of `UNDER_ATTACK` or `PROMOTION_REQUESTED` (active-risk states only). `riskWindowEnd` is sealed on first observation of `PRODUCTION` or `CORRUPTED`. Both happen lazily on the next pool interaction, or eagerly via permissionless `pokeRiskWindow()`. If the registry transitions straight from `NEW_DEPLOYMENT` to a terminal state without anyone observing an active-risk state, `riskWindowStart` never seals — see "no observed risk" below.
4. **Withdraw window.** Stakers can fully exit via `withdraw()` while the registry is in any pre-attack state (`NOT_DEPLOYED`, `NEW_DEPLOYMENT`, or `ATTACK_REQUESTED`). Withdrawals are permanently disabled from `UNDER_ATTACK` onward. Withdrawing forfeits any bonus claim.
5. **Resolution.** The moderator calls `flagOutcome(...)`, or `claimExpired()` mechanically resolves the pool after `expiry`.

## Resolution paths

- **SURVIVED** — moderator flags when the registry is in either terminal state: `PRODUCTION` (agreement-level survival) or `CORRUPTED` if the breach fell outside this pool's committed scope. `claimExpired()` also auto-resolves to SURVIVED when the registry is `PRODUCTION` at first post-expiry call. Stakers claim stake + a k=2 time-weighted share of the bonus (see "Bonus distribution").
- **CORRUPTED, bad-faith** — moderator flags with `goodFaith=false`. Full pool (stake + bonus) sweeps to `recoveryAddress` via `claimCorrupted()`.
- **CORRUPTED, good-faith** — moderator flags with `goodFaith=true` and names the whitehat attacker, who has 180 days (`CORRUPTED_CLAIM_WINDOW`) to claim up to `snapshotTotalStaked + snapshotTotalBonus` via `claimAttackerBounty()`. After the window, anyone can call `sweepUnclaimedCorrupted()` to send unclaimed funds to `recoveryAddress`.
- **EXPIRED** — `claimExpired()` after `expiry` with no terminal registry state. Stakers claim stake + a k=2 time-weighted share of the bonus.
- **CORRUPTED backstop** — if the moderator never acts and the registry is `CORRUPTED` AND `riskWindowStart` was observed, anyone can finalize as bad-faith CORRUPTED via `claimExpired()` after `expiry + MODERATOR_CORRUPTED_GRACE` (180 days). Trust-model fallback only. Without an observed risk window, the auto-resolution falls through to EXPIRED (see "No observed risk").

`sweepUnclaimedBonus()` recovers any excess over the remaining stakers' entitlement reserve (k=2 rounding dust, non-claimers' forfeited shares, post-resolution donations) to `recoveryAddress`. Repeat-callable — runs whenever there's something above the reserve. `claimCorrupted()` and `sweepUnclaimedCorrupted()` are similarly repeat-callable for CORRUPTED-path donation recovery.

**No observed risk.** If `riskWindowStart` is zero at resolution (registry skipped active-risk states or no one poked during them), the k=2 formula has nothing to weight, so `_bonusShare` returns zero. This affects only the per-staker bonus split under SURVIVED/EXPIRED: stakers recover principal and the unclaimable bonus sweeps to `recoveryAddress`. It does *not* gate CORRUPTED — a terminal `CORRUPTED` registry proves upstream risk regardless of local observation, so the moderator may still flag it on their off-chain in-scope judgement (sweeping the pool whole, bonus included), **provided no `claimExpired` call has resolved the pool first**. The permissionless `claimExpired` backstop lacks that judgement, so it stays conservative and falls through to EXPIRED; that resolution latches finality, so the moderator must flag a known in-scope breach before any post-expiry `claimExpired` (see DESIGN.md "no-risk-window CORRUPTED race").

## Bonus distribution

The bonus pool is split using a k=2 time-weighted formula: each deposit contributes `amount × (T − entryTime)²` to its staker's score, summed per-deposit (top-ups don't blend timestamps — that would lose Jensen gap and let users farm share by splitting across addresses). `T` is `riskWindowEnd` for SURVIVED resolutions or `expiry` for EXPIRED. Squaring crushes late entrants *within the observed risk window*: a staker arriving in the final minutes of an attack earns a vanishing share even with a large position. (Deposits made before the window seals all floor to `riskWindowStart` and are amount-weighted among themselves — pre-risk time is not at-risk time; see DESIGN.md.) Per-user and global sums maintain the score in O(1) per claim — no per-deposit iteration needed at resolve time.

`T` is pinned to the *first observed* terminal moment, not the resolving call's block timestamp, so callers can't shift distribution by varying when they trigger resolution. Effective entry time clamps to `riskWindowStart` — pre-risk capital earns no time weight.

## Trust assumptions

- **Sponsor (pool owner)** controls `recoveryAddress` (CORRUPTED sweep destination — receives the full pool including stakers' principal under bad-faith CORRUPTED, and only excess/dust under SURVIVED/EXPIRED/good-faith), `expiry` (until the first stake), and pool scope (until the registry leaves pre-attack staging — i.e. `NOT_DEPLOYED` or `NEW_DEPLOYMENT`). All staker-relevant state is on-chain — verify pool parameters match expectations before depositing.
- **Moderator** is the protocol DAO, set by the factory at clone time, immutable per-pool. The 180-day grace fallback exists as defense against permanent moderator unavailability.
- **Scope lock isolates stakers from agreement-level changes.** Once the registry leaves pre-attack staging, sponsor changes to the underlying agreement — adding accounts, narrowing scope, swapping the agreement's recovery address — don't alter what this pool covers. Stakers are exposed to exactly what they signed up for at deposit time.

## Build and test

```bash
forge install
forge build
forge test
```

### Fork tests against live BattleChain testnet

Two fork tests live under [test/fork/](test/fork/), both gated on `BATTLECHAIN_TESTNET_RPC`:

- [BattleChainInterfaceDrift.fork.t.sol](test/fork/BattleChainInterfaceDrift.fork.t.sol) — verifies our vendored BattleChain interfaces still structurally match the live deployed contracts (function selectors, enum ordinals, back-pointer integrity).
- [BattleChainFactoryIntegration.fork.t.sol](test/fork/BattleChainFactoryIntegration.fork.t.sol) — end-to-end smoke test: deploys our factory into the forked EVM pointed at the real SafeHarborRegistry, creates a pool against a real on-testnet agreement, and confirms the in-scope and out-of-scope paths both behave correctly.

```bash
BATTLECHAIN_TESTNET_RPC=https://testnet.battlechain.com forge test --match-path 'test/fork/*'
```

Both files pin to a deterministic block; bump `PIN_BLOCK` if upstream state drifts. Useful as a pre-release smoke check until the vendored interfaces are replaced by a submodule import.

## Deploy

```bash
SAFE_HARBOR_REGISTRY=0x... \
DEFAULT_MODERATOR=0x... \
INITIAL_STAKE_TOKENS=0x...,0x... \
forge script script/Deploy.s.sol --rpc-url <RPC> --broadcast
```

- `INITIAL_STAKE_TOKENS` is optional but recommended: the factory's stake-token allowlist starts empty, so without it `createPool` reverts `StakeTokenNotAllowed` until the owner runs `setStakeTokenAllowed` post-deploy.
- `DEPLOY_MOCK_MODERATOR=true` replaces `DEFAULT_MODERATOR` with a freshly-deployed permissionless moderator. Testnet only.

## Key parameters

- `_MIN_EXPIRY_LEAD = 30 days` — minimum lead time on `expiry` at deploy.
- `CORRUPTED_CLAIM_WINDOW = 180 days` — good-faith attacker bounty window.
- `MODERATOR_CORRUPTED_GRACE = 180 days` — moderator-failure backstop on `claimExpired`.
- Pause scope is limited to inflows (`stake`, `contributeBonus`). `withdraw` and all resolution/claim/sweep paths are exempt — the owner cannot trap stakers in the pool or freeze settlements.
