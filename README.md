# Bulwark

Multi-pass, multi-agent smart contract audit pipeline.
Rust CLI + Docker container with Slither, Forge, Halmos, Claude Code, and 70 AI audit skills.

> **Status**: Full 6-pass pipeline runs end-to-end in ~45 minutes on The Graph Protocol contracts.
> All 6 passes operational. Critical finding (slash front-run P-10 violation) discovered, PoC
> validated, and formally verified — end-to-end with no human in the loop.

## Quick Start

### 1. Set your API key

```bash
# .env (gitignored)
ANTHROPIC_API_KEY=sk-ant-...
```

### 2. Build and run

```bash
docker compose build
docker compose up
```

The pipeline starts automatically. To interact with the container while it runs:

```bash
docker exec -it bulwark bash
```

### 3. Manual control inside the container

```bash
bulwark run                       # Full 6-pass pipeline
bulwark run --pass 1              # Recon only (no AI, no auth needed)
bulwark run --pass 1-3            # Recon + Agents + PoC Gate
bulwark run --pass 2 --agent red  # Single agent

bulwark status                    # Which passes completed
bulwark findings                  # List findings with severity
bulwark findings --severity high
bulwark report                    # Regenerate final report
bulwark doctor                    # Check tool availability
```

## Pipeline

```
Pass 1: Reconnaissance ──────── Deterministic (Slither, Forge, Rust)         ~1 min
  │
Pass 2: Multi-Agent Analysis ── 3× parallel Claude (RED/BLUE/GOLD)           ~16 min
  │
Pass 3: PoC Gate ────────────── "No PoC, no finding" validation (Forge)      ~5–15 min
  │
Pass 4: Fuzzing Campaign ────── Foundry invariant tests (AI-generated)       ~8 min
  │
Pass 5: Formal Verification ─── Halmos bounded model checking                ~12 min
  │
Pass 6: Adversarial Review ──── Fresh Claude session challenges all           ~3 min
  │
  └──► final-report.md + final-report.json
```

### Pass 1: Reconnaissance

Deterministic, no AI required. Produces structured JSON consumed by all later passes.

- Compiles all in-scope packages (`forge build`)
- Slither static analysis — H/M/L severity triage
- Maps all external/public state-changing entry points
- Extracts storage layouts via `forge inspect --json`
- Builds inheritance/dependency graph
- Enumerates access control modifiers and roles
- Inventories arithmetic operations (division, multiplication)
- Identifies proxy patterns
- Optional AI-assisted vulnerability scan (`/tob-scv-scan`)

**On Graph Protocol:** 55 entry points across 5 contracts, H:28 M:75 L:48 from Slither.

### Pass 2: Multi-Agent Adversarial Analysis

Three independent Claude Code sessions run in parallel. Agents cannot see each other's output.
Each reads Pass 1 recon data, context files, and source code.

| Agent | Persona | Focus |
|-------|---------|-------|
| RED | Attacker | Exploits that steal funds. Rewarded per critical finding. |
| BLUE | Systematic verifier | Verify/refute each property in PROPERTIES.md. |
| GOLD | DeFi economist | Rounding errors, MEV, flash loans. Numbers required. |

After completion, findings are merged and deduplicated. Severity disagreements are tracked.
Variant analysis (`/tob-variant-analysis`) runs on all high/critical findings to find similar patterns.

### Pass 3: PoC Gate

Every finding must earn a validated proof-of-concept or it is discarded.

1. False-positive check (`/tob-fp-check`) filters obvious FPs before spending PoC budget
2. Claude generates a Foundry test (`[PASS]` = attack succeeded convention)
3. `forge build` — must compile; errors fed back for retry (up to 2 retries)
4. `forge test` — `[PASS]` = validated, `[FAIL]` = inconclusive
5. Inconclusive findings get one more retry with full test output as feedback
6. Findings that exhaust retries are discarded; inconclusive High/Critical capped to Medium

### Pass 4: Fuzzing Campaign

Claude (Sonnet) generates Foundry invariant tests from PROPERTIES.md. No bash access — Claude
writes files only, the pipeline handles compilation. Generated files are sanitised (unicode
quotes replaced with ASCII) before compilation to prevent Solidity parse errors.

Tests run with `forge test --match-test invariant_` at 10,000 fuzz runs each.
Missing remappings are auto-detected from build errors and patched before compilation.
Medusa and Echidna integration is wired up but not yet installed in the container.

**On Graph Protocol:** 5 test files, 28 invariant functions generated, 14 passing.

### Pass 5: Formal Verification

Claude (Sonnet) generates symbolic tests for critical properties. Tests run in an isolated
directory (import-free pure arithmetic contracts) so Halmos doesn't fight the full project.

Halmos runs bounded model checking (`--loop 5`, `--solver-timeout 300s`) per property:

| Result | Meaning |
|--------|---------|
| VERIFIED | No counterexample exists within the loop bound |
| VIOLATED | Concrete counterexample found — genuine bug |
| TIMEOUT | Solver budget exhausted — property may hold but Z3 couldn't close it |

**On Graph Protocol:** P-10, P-15, P-16, P-19 VERIFIED. P-1 TIMEOUT (cross-multiplication
arithmetic exceeds Z3's budget at this bound — not a finding).

### Pass 6: Adversarial Review

A fresh Claude session with no prior context reads all findings from passes 2–5 and challenges
them. Produces severity upgrades, compound attack scenarios, blind spot analysis, and the final
`final-report.md` + `final-report.json`.

## Results on The Graph Protocol (Horizon)

| Finding | Severity | Source | Status |
|---------|----------|--------|--------|
| Service provider front-runs slash by thawing tokens (P-10 violation) | Critical | Pass 2 | PoC validated |

The slash front-run allows a service provider to call `thaw()` before a slash transaction lands,
reducing their slashable provision and forcing delegators to absorb losses that should fall on
the provider. P-10 (provider-first slash ordering) is formally verified to hold in the pure
arithmetic model — the violation is at the protocol interaction level, not the accounting math.

## Model Configuration

```toml
model = "haiku"                    # Global default (cheapest, used for most passes)

[passes.fuzzing]
model = "sonnet"                   # Better at generating compilable Solidity tests

[passes.formal]
model = "sonnet"                   # Better at generating symbolic test structure
```

Swap `haiku` → `sonnet` globally for higher-quality agent analysis at ~4× the cost.

## Installed AI Skills

The container auto-installs ~70 audit skills at startup from three sources:

| Source | Count | What |
|--------|-------|------|
| [Trail of Bits skills](https://github.com/trailofbits/skills) | 36 | fp-check, variant-analysis, entry-point-analyzer, etc. |
| [Trail of Bits skills-curated](https://github.com/trailofbits/skills-curated) | 28 | scv-scan (36 Solidity vuln classes), and more |
| [forefy/.context](https://github.com/forefy/.context) | 6 | smart-contract-security-audit, foundry-poc, etc. |

### Pipeline-integrated skills

| Skill | Where | Purpose |
|-------|-------|---------|
| `/tob-scv-scan` | Pass 1 (post-Slither) | 36-class vulnerability scan |
| `/tob-fp-check` | Pass 3 (pre-PoC) | False positive gate |
| `/tob-variant-analysis` | Pass 2 (post-merge) | Pattern search for high/critical findings |
| `/tob-scv-scan` | RED + GOLD agents | Structured scan before manual analysis |
| `/tob-fp-check` | BLUE agent | Self-challenge on potential violations |
| `/tob-token-integration-analyzer` | GOLD agent | Token-handling edge case detection |
| `/tob-spec-to-code-compliance` | BLUE agent | Cross-check property vs implementation |
| `/tob-variant-analysis` | RED agent | Post-analysis variant search |

All integrations degrade gracefully — if a skill is not installed, the pipeline continues.

## Context Files

Pre-populated for The Graph Protocol in `context/`:

- **AUDIT_CONTEXT.md** — Protocol overview, deployment, trust model, economic parameters
- **PROPERTIES.md** — 22 security invariants (P-1 through P-22)
- **KNOWN_ISSUES.md** — 4 accepted risks, 5 fixed issues, 3 focus areas
- **ATTACK_PATTERNS.md** — 10 known patterns from previous audits and bounties

Copied into the audit directory at container startup. Replace with your own context for a new target.

## Output Structure

```
audit-workspace/
├── recon/                       # Pass 1
│   ├── entry-points.json
│   ├── storage-layouts.json
│   ├── slither-results.json
│   ├── dependency-graph.json
│   ├── math-operations.json
│   └── access-control.json
├── findings/                    # Pass 2
│   ├── red-agent-raw.json
│   ├── blue-agent-raw.json
│   ├── gold-agent-raw.json
│   ├── merged-deduplicated.json
│   └── variant-analysis.json
├── pocs/                        # Pass 3
│   ├── F-001.t.sol
│   ├── validated-findings.json
│   └── discarded-findings.json
├── fuzzing/                     # Pass 4
│   ├── invariant-tests/
│   ├── fuzzing-campaign-results/
│   └── fuzzing-findings.json
├── formal/                      # Pass 5
│   ├── Symbolic*.t.sol
│   ├── verification-summary.json
│   └── formal-findings.json
├── review/                      # Pass 6
│   └── adversarial-review.json
├── final-report.md
├── final-report.json
└── pipeline-status.json
```

Reports are also exported to `./reports/` on the host after each run.

## Configuration Reference

`bulwark.toml` controls everything:

```toml
[target]
repo   = "https://github.com/graphprotocol/contracts.git"
branch = "main"
scope  = ["packages/horizon", "packages/subgraph-service"]
core_contracts = ["HorizonStaking", "GraphPayments", "PaymentsEscrow"]

model = "haiku"   # Global default; "sonnet" or "opus" for more depth

[passes.recon]
scv_scan          = true
scv_scan_max_turns = 20

[passes.agents]
max_turns        = 80
agents           = ["red", "blue", "gold"]
timeout_minutes  = 60
variant_analysis = true

[passes.poc]
max_turns   = 30
max_retries = 2
fp_check    = true

[passes.fuzzing]
fuzz_runs      = 10_000
max_turns      = 40
model          = "sonnet"

[passes.formal]
solver_timeout     = 300
loop_bound         = 5
target_properties  = ["P-1", "P-10", "P-15", "P-16", "P-19"]
max_turns          = 30
model              = "sonnet"

[passes.review]
max_turns = 60
```

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `ANTHROPIC_API_KEY` | For AI passes | Or authenticate via `bulwark login` inside the container |
| `AUDIT_TARGET` | No | Override git repo URL |
| `AUDIT_BRANCH` | No | Override branch |

## Development

```bash
cargo check           # Type check
cargo test            # Unit tests
cargo clippy          # Lint
cargo build --release # Build release binary
```

## License

[MIT](LICENSE)
