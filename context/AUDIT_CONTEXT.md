# Audit Context: The Graph Protocol (Horizon)

## Protocol Overview
The Graph is a decentralized indexing protocol for querying blockchain data.
Indexers stake GRT to serve queries, Delegators stake GRT behind Indexers,
and Curators signal which subgraphs are valuable. Horizon (Dec 2025) replaced
the monolithic staking model with modular "provisions" per data service.

## Deployment
- **Chain**: Arbitrum One (primary), Arbitrum Sepolia (testnet)
- **Token**: GRT (L2GraphToken) — 0x9623063377AD1B27544C965cCd7342f7EA7e88C7
- **Proxy pattern**: OpenZeppelin TransparentUpgradeableProxy
- **Controller**: 0x0a8491544221dd212964fbb96487467291b2C97e (owns all contracts)
- **L1 contracts**: Deprecated as of December 2024

## In-Scope Contracts (Horizon)
| Contract | Package | Purpose |
|---|---|---|
| HorizonStaking | horizon | Provisions, delegation, slashing |
| GraphPayments | horizon | Fee distribution (protocol tax → data service → delegation → receiver) |
| PaymentsEscrow | horizon | Pre-collateralized escrow for payer→receiver payments |
| GraphTallyCollector | horizon | TAP-based RAV verification and collection |
| SubgraphService | subgraph-service | First data service — allocations, POI, fee collection |
| DisputeManager | subgraph-service | Dispute resolution with flexible slashing |
| L2Curation | subgraph-service | Bonding curve for curation signals |
| RewardsManager | horizon | Inflationary reward distribution |

## Trust Model
- **Graph Council (Governor)**: Can upgrade ANY contract, change parameters,
  appoint arbitrator. This is accepted — endgame is renouncing upgradeability.
- **Data services**: Each is the slashing authority for its provisions.
  SubgraphService is currently the only data service.
- **Operators**: Can manage provisions on behalf of service providers but
  CANNOT withdraw or transfer tokens. Critical invariant.
- **GraphTally/Gateway**: Signs off-chain payment receipts (RAVs). Trusted
  to not double-sign, but on-chain verification prevents replay.
- **Arbitrator**: Resolves disputes. Appointed by Graph Council.

## Economic Parameters
- Protocol tax: Governance-configured (applied via GraphPayments)
- Slashing cap: Up to 10% of provider's stake (Arbitration Charter)
- Delegation tax: REMOVED in Horizon (was 0.5%)
- Max concurrent undelegation requests: 100 per delegator per (SP, dataService)
- GRT issuance: ~1.05% annualized (Q2 2025)
