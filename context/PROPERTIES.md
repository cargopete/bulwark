# Security Properties: The Graph Horizon Contracts

## Staking Invariants

### P-1: Stake conservation
Total GRT in HorizonStaking == sum of all (idle stake + provisioned tokens
+ delegation pool tokens + thawing tokens). No GRT can be created or
destroyed inside the staking contract.

### P-2: Provision isolation
Tokens assigned to provision(SP, dataServiceA) cannot simultaneously be
counted in provision(SP, dataServiceB). No double-counting of stake.

### P-3: Thawing period enforcement
Provisioned tokens cannot be withdrawn until thawingPeriod (set per provision)
has elapsed since thaw() was called. Early withdrawal must revert.

### P-4: Provisioned tokens are locked
Cannot call unstake() on tokens that are provisioned. Only idle stake
(not assigned to any provision) can be unstaked.

## Delegation Invariants

### P-5: Pool share consistency
If delegationPool.totalShares > 0, then delegationPool.totalTokens > 0.
An empty pool (0 shares) must also have 0 tokens.

### P-6: Share price monotonicity
Delegation share price (tokens/shares) can only decrease through slashing.
Normal operations (delegate, undelegate, collect fees) must not decrease
share price for existing delegators.

### P-7: No delegation tax
100% of delegated tokens must be credited to the delegation pool.
No protocol fee on delegation entry (removed in Horizon).

### P-8: Thawing delegation remains slashable
Tokens in undelegation thawing period are still subject to slashing.
A delegator cannot front-run a slash by starting undelegation.

### P-9: Concurrent undelegation cap
A delegator can have at most 100 concurrent undelegation requests per
(serviceProvider, dataService) pair. The 101st must revert.

## Slashing Invariants

### P-10: Provider-first slashing order [CRITICAL]
When slash(SP, tokens, reward, dest) is called:
1. Provider's provisioned stake is slashed first
2. Delegator stake is ONLY touched if provider stake is fully exhausted
3. This ordering must hold regardless of slash amount

### P-11: Delegator slash tokens burned only
When delegator tokens are slashed, they MUST be burned (sent to address(0)).
Delegator-slashed tokens must NEVER be used as reward for the slasher.

### P-12: Only authorized verifier can slash
msg.sender for slash() must equal the provision's designated verifier
(the data service contract). No other address can trigger slashing.

### P-13: Slash conservation
slashed_amount == burned_tokens + reward_tokens. No tokens lost or created.

## Payments Invariants

### P-14: Escrow solvency
PaymentsEscrow.balance(token) >= sum of all active deposits for that token.
No withdrawal can create insolvency.

### P-15: Fee distribution conservation
In GraphPayments.collect(): protocolTax + dataServiceCut + delegationCut
+ receiverAmount == totalCollected. Exact equality (no rounding loss).

### P-16: RAV monotonicity
For any (payer, serviceProvider, dataService) tuple, each new RAV's
valueAggregate must be >= the previously collected valueAggregate.

### P-17: Cross-service RAV isolation
A RAV issued for dataServiceA cannot be used to collect from dataServiceB.
The dataService field in the RAV must match the collecting data service.

### P-18: Escrow thaw-collect race
Once a payer starts thawing their escrow deposit, the receiver should still
be able to collect up to the full deposit amount until thaw completes.
After thaw completion, collection must fail.

## Operator Invariants

### P-19: Operators cannot extract value [CRITICAL]
No sequence of operator-callable functions can result in tokens being
transferred to the operator's address or any address they control.
Operators manage provisions but never move tokens out.

### P-20: Operator scope isolation
An operator authorized for dataServiceA cannot perform actions on
dataServiceB provisions on behalf of the service provider.

## Upgrade Safety

### P-21: Storage layout compatibility
Any contract upgrade must preserve storage layout. New variables appended
only. Storage gaps must be decremented correctly. Verify with
forge inspect --storage-layout.

### P-22: Initialization guard
All proxy contracts must be initializable exactly once. Re-initialization
must revert. Check initializer/reinitializer modifiers.
