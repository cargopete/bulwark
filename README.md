# Bulwark

A multi-pass smart contract audit pipeline that produces bug bounty leads. A Rust CLI drives
Claude Code sessions, Slither, Forge, Medusa, Echidna and Halmos inside a Docker container
against one target repository, and hands back findings that have either been demonstrated
on a fork or are plainly marked as not demonstrated.

Bulwark is a lead generator, not a report writer. Immunefi bans AI-generated and
automated-scanner reports, so every finding it emits is re-derived and proved by hand before
anything is submitted. The submission drafts it writes are scaffolding for that: measured facts
in plain text, model-written text quoted and marked for rewriting.

How well it does is measured, not asserted: see [Benchmark](#benchmark).

## Quick start

A target is a project directory holding `bulwark.toml`, optionally `program.toml` (the bounty
programme) and `context/` (protocol notes). Runs happen in the container.

```bash
# 1. Describe the target
mkdir -p ~/targets/acme && cd ~/targets/acme
bulwark init --repo https://github.com/acme/contracts.git .   # writes bulwark.toml
bulwark program import https://immunefi.com/bug-bounty/acme/scope/   # writes program.toml

# 2. Build and run
cd ~/bulwark
docker compose build
BULWARK_PROJECT=~/targets/acme docker compose run -d --name bulwark-acme \
    -e ANTHROPIC_API_KEY= audit-env bulwark run

# 3. Watch it
BULWARK_PROJECT=~/targets/acme docker compose up -d dashboard   # http://localhost:8484
docker logs -f bulwark-acme
```

Authentication is either a Claude login (the host's `~/.claude/.credentials.json` is mounted
and kept in sync both ways) or `ANTHROPIC_API_KEY`. Pass `-e ANTHROPIC_API_KEY=` to force the
login. The account's claude.ai connectors are disabled inside the container, because agents read
untrusted code with a shell.

For fork-mode PoCs set `BULWARK_FORK_UPSTREAM` (and `BULWARK_FORK_UPSTREAM_TOKEN` if the RPC
needs a bearer token) on the host. The entrypoint fronts it with a local anvil pinned at
`passes.poc.fork_block`, so the token never reaches a PoC.

## Commands

```bash
bulwark run                        # all six passes
bulwark run --pass 1-3             # a range
bulwark run --resume 4             # continue from a pass, keeping earlier output
bulwark run --pass 2 --agent red   # one agent

bulwark status                     # which passes completed
bulwark findings                   # final findings, best estimated reward first
bulwark findings --rejected        # what the scope gate, dedup and PoC gate threw away, and why
bulwark report                     # rebuild final-report.md/json and the submission drafts
bulwark dashboard                  # live run health on :8484
bulwark program import <url>       # Immunefi scope page -> program.toml
bulwark bench score|table          # benchmark scoring, see bench/README.md
bulwark doctor                     # tool availability
```

## Pipeline

```
Pass 1  Reconnaissance       deployed-source check, build, Slither, scv-scan, derived context
Pass 2  Agents               RED / BLUE / GOLD plus one hunter per component, evidence-gated
        Scope gate           drops findings outside the programme's assets, impacts and rules
        Known-issue filter   drops duplicates of published audits and known issues
Pass 3  PoC gate             fork-native Foundry PoC per finding, graded on measured impact
Pass 4  Fuzzing              one generated suite through Forge, Medusa and Echidna
Pass 5  Formal               Halmos on the real contracts, with arithmetic models as a second layer
Pass 6  Adversarial review   a fresh session challenges everything, then the report is assembled
        Submission drafts    one Immunefi-shaped draft per final finding
```

A pass that fails stops the run, and a pass that ran nothing fails: an empty result from a
broken pass reads as "the code is clean", which is worse than no run.

### Pass 1: Reconnaissance

- **Deployed source.** For each programme asset on the fork's chain, reads the EIP-1967
  implementation slot at `fork_block`, fetches its verified source from Sourcify (or Etherscan
  when `ETHERSCAN_API_KEY` is set) and compares it file by file with the pinned commit. If the
  asset's own contract differs, the run stops and names the files. Verified sources are kept in
  `recon/deployed-source/`.
- Builds every package in scope (`pre_build_cmd` first if set), runs Slither without the
  target's own triage database, maps entry points, storage layouts, access control and
  arithmetic, and runs the `/tob-scv-scan` skill.
- **Derived context.** A target without its own `AUDIT_CONTEXT.md` or `PROPERTIES.md` gets them
  written from its code and docs, so later passes never run against nothing.

### Pass 2: Agents

Three generalists (RED attacker, BLUE property verifier, GOLD economist) and one hunter per
`[[passes.agents.components]]` entry run in parallel, `max_parallel` at a time. Each must leave
evidence of work: files read in full, hypotheses tested, Forge tests run. A session short of
evidence is resumed with the gap named. Scratch tests go under the project's own test directory
(read from `foundry.toml`), where forge actually collects them. The previous run's Pass 2 output
is moved outside the audit directory first, so no agent reads another run's conclusions.

Findings are merged and deduplicated, variant analysis runs on High and Critical, and then:

- the **scope gate** maps each finding to one programme asset and one impact row and applies the
  programme's exclusions (rule-based, then a classifier);
- the **known-issue filter** matches each finding against `known/` (published audits,
  KNOWN_ISSUES.md) and drops duplicates.

### Pass 3: PoC gate

Each surviving finding gets a Foundry test, on a fork of the deployed contracts at the pinned
block by default. A test proves impact only by logging a nonzero `bulwark:attacker_gain` or
`bulwark:victim_loss`. Passing without one is inconclusive. A cheatcode linter flags `deal`,
`prank` or `store` on anything but a fresh attacker, and attack capital is declared in the test.
Every PoC writes `pocs/<id>.poc.json` with mode, block, command, capital, results and lint
flags. A test forge did not collect is recorded as a harness fault, not retried as a bad PoC.

### Pass 4: Fuzzing

The model writes one suite in a fixed layout: Setup, multi-actor TargetFunctions (attacker,
honest user, privileged roles limited to routine in-scope actions), `invariant_*` properties, a
`CryticTester` for Medusa and Echidna and a Forge entry. Compile errors go back for up to two
repair rounds. Forge, Medusa and Echidna then run it, on fork state when a fork is configured,
with corpora kept in the workspace so reruns resume. Each broken invariant is classified as a
wrong invariant, a handler precondition or a real violation, and only real violations become
findings, with the call sequence as the PoC seed.

### Pass 5: Formal verification

Property IDs come from the target's `PROPERTIES.md`, in whatever shape it uses (`P-3`, `P-T2`).
The `real` layer deploys the in-scope contracts in a Halmos test under the project's test
directory and checks each property against production bytecode. The `model` layer keeps
import-free copies of the arithmetic as a sanity check. A bounded VERIFIED clears nothing. Only
a counterexample on a real contract becomes a finding. A run killed by the time limit reports
TIMEOUT with the number of candidate paths it had not yet refuted.

### Pass 6: Adversarial review

A fresh session reads every pass's output and challenges it: severity, reinstatement of
discarded findings, compound attacks, blind spots, properties it doubts. Severity may only move
to a row in the programme's impact table. The report goes to `final-report.md` and `.json`, and
both, with the submission drafts, are copied to `./reports/` on the host.

### Submission drafts

`audit-workspace/submissions/<id>.md`, one per final finding, in Immunefi's order: Brief,
Vulnerability Details, Impact, Risk Breakdown, Recommendation, References, Proof of Concept.

- **Plain text:** facts the run measured. The impact row, the reward ceiling, declared capital,
  cheatcode flags to resolve, and GitHub permalinks at the audited commit.
- **Quoted under `[rewrite: model-written]`:** anything a model wrote.
- **Marked `[missing]`:** a section the run could not fill.
- **Opens with NOT READY:** a finding whose PoC did not demonstrate impact.

## Benchmark

`bench/` holds contests and private audits whose judged findings were published after the
agents' training cutoff, so a hit cannot come from memory. A bench run is blind: the clone is
collapsed to one snapshot commit, and web tools, `curl`, `wget`, `gh` and `git fetch` are denied.
Each known High and Medium is traced through every stage, so a miss says which pass lost it.
Results are in [`bench/RESULTS.md`](bench/RESULTS.md); how to run and add cases is in
[`bench/README.md`](bench/README.md).

## Configuration

`bulwark.toml`:

```toml
[target]
repo   = "https://github.com/acme/contracts.git"
branch = "main"
commit = "<sha>"                 # the audited commit; pin it
scope  = ["."]                   # package directories, each with a foundry.toml
core_contracts = ["Vault", "Router"]
math_sensitive = ["Vault"]
pre_build_cmd  = "bash /home/auditor/project/prebuild.sh"   # optional

[budget]
max_usd = 200                    # hard cap on Claude spend per run; omit for no cap

[passes.recon]
scv_scan        = true
deployed_source = "stop"         # stop | warn | off

[passes.agents]
agents       = ["red", "blue", "gold"]
max_turns    = 400
timeout_minutes = 240
max_parallel = 4
variant_analysis = true

[[passes.agents.components]]     # one hunter each
name  = "vault"
files = ["src/Vault.sol", "src/VaultStorage.sol"]

[passes.poc]
max_retries  = 2
fp_check     = true
fork_rpc_env = "BULWARK_FORK_RPC"
fork_block   = 21000000

[passes.fuzzing]
invariant_runs  = 256
invariant_depth = 50
medusa_timeout  = 3600
echidna_limit   = 500_000

[passes.formal]
solver_timeout    = 300          # seconds
loop_bound        = 5
target_properties = []           # empty = every property in PROPERTIES.md

[models]                         # per-stage override; Opus 5.5 by default
review = "claude-fable-5-1"
```

`program.toml` (written by `bulwark program import`, then edited):

```toml
[program]
platform = "immunefi"
slug = "acme"
max_payout_usd = 1000000
reward_formula = "pct_of_funds_at_risk"   # or "fixed"
reward_pct = 10
requires_poc = true

[program.rewards]                # per-severity ceilings, imported from the programme page
Critical = 1000000
High = 50000
Medium = 10000

[[program.assets]]
name = "Vault"                   # contract name as in source
address = "0x..."
chain_id = 1

[[program.impacts]]
severity = "Critical"
text = "Direct theft of any user funds, whether at-rest or in-motion"

[program.out_of_scope]
program_specific = ["Attacks requiring the owner key"]
```

Context files in `<project>/context/` are copied into the audit directory at start:
`AUDIT_CONTEXT.md` (protocol, trust model, value flows), `PROPERTIES.md` (falsifiable
invariants), `KNOWN_ISSUES.md` (accepted and disclosed issues). Missing ones are derived in
Pass 1. `ATTACK_PATTERNS.md` always comes from the install.

## Environment variables

| Variable | Meaning |
|---|---|
| `BULWARK_PROJECT` | Host path of the target project, mounted read-only |
| `ANTHROPIC_API_KEY` | API key auth; empty forces the Claude login |
| `BULWARK_FORK_UPSTREAM`, `BULWARK_FORK_UPSTREAM_TOKEN` | Archive RPC behind the fork proxy |
| `ETHERSCAN_API_KEY` | Fallback for the deployed-source check when Sourcify has no match |
| `BULWARK_BENCH=1` | Blind bench mode: snapshot history, deny web and network commands |
| `BULWARK_LOG` | Tracing filter, e.g. `bulwark=debug` |

## Output

```
audit-workspace/
├── recon/            entry points, layouts, Slither, scv-scan, deployed-source.json
├── findings/         raw per agent, coverage, merged, rejected.json, known-duplicates.json, agent-tests/
├── pocs/             <id>.t.sol, <id>.poc.json, validated / discarded / unverifiable
├── fuzzing/          invariant-tests/, campaign results, tool-runs.json, invariant-classification.json
├── formal/           models, verification-summary.json (real and model layers)
├── review/           adversarial-review.json
├── submissions/      <id>.md drafts
├── final-report.md / final-report.json
├── cost.json         per-session spend
└── pipeline-status.json
```

## Development

```bash
cargo test
cargo clippy --all-targets
cargo fmt
```

CI runs lint and tests, a release build and the Docker build on every PR.

## License

[MIT](LICENSE)
