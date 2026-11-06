# Roadmap

Bulwark's goal is bounty-grade leads: findings demonstrated on a fork against deployed code, in
scope, not already known, with a draft a person can turn into a report. Progress is tracked in
#11 and measured by the benchmark in `bench/`.

## Where it stands (November 2026)

| Area | State |
|---|---|
| Programme config and scope gate (#2) | Built. Closes once a bench case shows an out-of-scope finding dropped with a logged reason. |
| Fork-native PoCs, impact markers, cheatcode linter (#3) | Done. |
| Known-issue ingestion and duplicate suppression (#4) | Built. Closes once the bench reports a match rate against published audits. |
| Benchmark harness (#5) | Built; nine blind cases running. Closes when recall, precision and cost per true positive are reported over them. |
| Fuzzing and formal on real code (#6) | Done. Forge, Medusa and Echidna run one suite; Halmos runs on the real contracts. |
| Submission drafts and reward-sorted findings (#9) | Done. |
| Deployed source vs pinned commit (#40) | Done; Pass 1 stops on a mismatch. |

## Open work

- **#5 Benchmark.** Finish the first full pass over the nine cases, then use the per-stage recall
  to choose the next fix. Four more cases need hand setup before they can join: Firelight
  (an out-of-scope subtree imports a removed OpenZeppelin `SafeMath`), Brava (stack-too-deep
  under via-IR), Interfold (pnpm workspace, an earlier audit inside the package) and Kerne (the
  deployed and fixed code sit beside the audited snapshot).
- **#7 `bulwark diff`.** Upgrade and diff mode: storage, access-control, initializer and
  differential checks on a proxy upgrade pair.
- **#8 Watch list.** Scheduled runs on scope changes and upgrades of watched programmes.
- **#10 Toolchain and reporting.** Pinned tool versions in one place, SARIF output, schema CI.

## Known limitations

- **Fuzz findings skip the PoC gate.** Pass 3 runs before Pass 4, so a real violation found by
  the fuzzers reaches Pass 6 and the report as inconclusive, carrying its call sequence, without
  a graded PoC.
- **Halmos cannot fork.** The real layer deploys the contracts and mocks their external
  dependencies (oracles, routers, registries), so a property that depends on live external state
  is only as good as the mock.
- **Agent depth.** Component hunters often finish in a few minutes when the component is small
  or mostly behind roles. The evidence gate forces reading and testing but not persistence; the
  benchmark is meant to show whether that costs recall.
- **Funds at risk are in token units.** PoCs log gains and losses in tokens; USD pricing at the
  pinned block is not implemented, so a draft's reward estimate falls back to the programme's
  per-severity ceiling.
- **Subscription limits.** Runs on a Claude login share the account's usage limits; a long
  benchmark can be slowed by them.

## Faults found in live runs, and what fixed them

Every one of these produced a clean-looking result from a pass that had not done its job, which
is why a pass that runs nothing now fails the run.

| Fault | Seen on | Fix |
|---|---|---|
| PoCs and invariant tests written outside `foundry.toml`'s test directory; forge ran none | Origin | #42: the test directory is read from `foundry.toml` everywhere |
| Pass 4 reported done with zero tests run or compiled | Origin, SSV | #41: zero tests fails the pass, after two compile-repair rounds |
| Targets without their own context got The Graph's PROPERTIES.md and audit context | every new target | Pass 1 derives the context from the target |
| Halmos read the solver timeout as milliseconds, so every past TIMEOUT was the bug | Onyx, Origin, Ondo | the timeout is passed in seconds |
| Halmos could not deploy project contracts under forge's dynamic test linking | Ondo | dynamic test linking is off for Halmos |
| Halmos timeouts labelled ERROR because the log mentions `Panic(0x01)` | Onyx | #38: classified by the `timeout` exit code, candidate paths counted |
| Agents read a previous run's findings | Ondo | #39: Pass 2 moves the previous output outside the audit directory |
| An hour of agent work on code the proxy no longer routes to | SSV | #40: deployed-source check in Pass 1 |
| Medusa built nothing (compile target relative to its config); Medusa and Echidna raced on `forge clean` | Ondo dev run | absolute target; the fuzzers run one after the other |
| Every target without its own PROPERTIES.md got The Graph's, and every target The Graph's attack patterns, from the pipeline's staging after the entrypoint was fixed | Alchemix, all nine bench cases | #52: context only from the target's own `context/` |
| Medusa killed by the wall-clock cap mid-campaign and reported as having run nothing; Echidna refused the project's own tests for unlinked libraries | Alchemix | #52: the cap allows for the compile and sends SIGINT; the fuzzers compile the suite alone |
| Halmos ignores SIGTERM, so a 16-minute cap held the formal pass for six hours; the generator's own trial runs kept a core busy after its session ended | StableSwap bench | #54: `timeout -k`, stray Halmos killed before the pass's own run |
