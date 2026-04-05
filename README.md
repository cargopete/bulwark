# Bulwark

Multi-pass, multi-agent smart contract audit pipeline.
Rust CLI + Docker container with Slither, Forge, Halmos, Claude Code, and 70 AI audit skills.

> **Status**: Full 6-pass pipeline runs end-to-end (~70 min on Graph Protocol contracts).
> All 6 passes operational. Pass 5 (Halmos) runs all 5 properties with real symbolic execution;
> `Counterexample: unknown` (solver timeout) is correctly distinguished from real violations.
> Deduplication correctly merges findings across agents.

## Quick Start

### 1. Build

```bash
docker compose build
```

### 2. Enter the container

```bash
docker compose run --rm -it audit-env bash
```

### 3. Authenticate Claude Code

```bash
# Option A: API key (set ANTHROPIC_API_KEY in .env before docker compose run)
# Option B: Interactive login
bulwark login
```

### 4. Run the pipeline

```bash
bulwark run --pass 1          # Recon only (no AI, no auth needed)
bulwark run --pass 1-3        # Recon + Agents + PoC Gate
bulwark run                   # Full 6-pass pipeline
bulwark run --pass 2 --agent red  # Single agent run
```

### 5. Check results

```bash
bulwark status                # Which passes completed
bulwark findings              # List findings with severity
bulwark findings --severity high
bulwark report                # Regenerate final report
bulwark validate              # Check JSON against schemas
bulwark doctor                # Check tool availability
```

## Pipeline

```
Pass 1: Reconnaissance ──────── Deterministic (Slither, Forge, Rust)
  |
Pass 2: Multi-Agent Analysis ── 3x parallel Claude (RED/BLUE/GOLD)
  |
Pass 3: PoC Generation ──────── "No PoC, no finding" gate (Forge)
  |
Pass 4: Fuzzing Campaign ────── Foundry invariant tests + Medusa/Echidna
  |                               (runs in parallel with Pass 5)
Pass 5: Formal Verification ─── Halmos bounded model checking
  |
Pass 6: Adversarial Review ──── Fresh Claude session challenges all
  |
  +---> final-report.md + final-report.json
```

### Pass 1: Reconnaissance

Mostly Rust, no AI required. Optionally runs AI-assisted scv-scan after Slither.
Produces structured JSON consumed by all later passes:
- Compiles contracts (`forge build`)
- Runs Slither static analysis (H/M/L severity counts)
- Maps all external/public state-changing entry points (55 across 5 contracts)
- Extracts storage layouts via `forge inspect --json`
- Builds inheritance/dependency graph
- Enumerates access control modifiers and roles
- Inventories arithmetic operations (division, multiplication)
- Identifies proxy relationships

### Pass 2: Multi-Agent Analysis

Three independent Claude Code sessions run in parallel. Agents cannot see
each other's output. Each reads Pass 1 recon data + context files + source code.

| Agent | Persona | Focus |
|-------|---------|-------|
| RED | Attacker | Exploits that steal funds. Paid per critical finding. |
| BLUE | Systematic verifier | Verify/refute all 22 properties (P-1 to P-22). |
| GOLD | DeFi economist | Rounding errors, MEV, flash loans. Must include numbers. |

After completion, findings are merged and deduplicated with severity
disagreement tracking. Variant analysis runs on high/critical findings.

### Pass 3: PoC Gate

For each finding from Pass 2:
1. False-positive check (`/tob-fp-check`) filters obvious FPs
2. Claude generates a Foundry test PoC (positive convention — `[PASS]` = attack succeeded)
3. PoC is placed inside the forge project (`test/pocs/`) so forge can compile and find it
4. `forge build` — must compile; compilation errors fed back for retry
5. `forge test --match-path test/pocs/F-XXX.t.sol` — `[PASS]` = validated, `[FAIL]` = inconclusive
6. On inconclusive: retry with full test output so the AI can fix assertion direction
7. Findings that fail all retries are discarded; inconclusive High/Critical capped to Medium

### Pass 4: Fuzzing Campaign

Claude (Sonnet) generates Foundry invariant tests from PROPERTIES.md. Tests are placed
alongside the project's existing invariant tests and run with `forge test --match-test invariant_`.
Missing Forge remappings are auto-detected and patched before compilation. AI-generated files
are sanitized to replace curly/smart quotes and typographic dashes (Solidity only accepts ASCII).
Medusa and Echidna integration coded but not yet installed.

### Pass 5: Formal Verification

Claude (Sonnet) generates symbolic tests for critical properties. Tests run in an isolated
directory (5 files, not the full project) with a minimal `foundry.toml`. Halmos runs bounded
model checking on each property, producing VERIFIED/VIOLATED/TIMEOUT results.
- `Counterexample: unknown` = solver timed out, classified as TIMEOUT not VIOLATED
- VACUOUS detection uses `[PASS]`/`[FAIL]`/`[TIMEOUT]` markers, not duration heuristics
- Test functions must follow `check_P{N}_{description}` naming (suffix required)
- Bare `check_P10()` names are auto-renamed to `check_P10_verify()` before compilation
- A Dockerfile patch guards `sevm.py`'s `clear_live()` call against empty `_live_stack`
  (Halmos crashes in non-TTY subprocesses without this fix)

### Pass 6: Adversarial Review

Fresh Claude session challenges all findings from passes 2-5. Produces severity
upgrades, compound attack scenarios, blind spot analysis, and both markdown + JSON reports.

## Model Configuration

The global model defaults to `haiku` (cheapest). Individual passes can override:

```toml
model = "haiku"                    # Global default

[passes.fuzzing]
model = "sonnet"                   # Better at generating compilable tests

[passes.formal]
model = "sonnet"                   # Better at generating compilable tests
```

## Installed AI Skills

The container auto-installs 70 audit skills at startup:

| Source | Count | What |
|--------|-------|------|
| [Trail of Bits skills](https://github.com/trailofbits/skills) | 36 | entry-point-analyzer, fp-check, variant-analysis, etc. |
| [Trail of Bits skills-curated](https://github.com/trailofbits/skills-curated) | 28 | scv-scan (36 Solidity vuln classes), and others |
| [forefy/.context](https://github.com/forefy/.context) | 6 | smart-contract-security-audit, foundry-poc, etc. |

### Pipeline-integrated skills

| Skill | Where | Purpose |
|-------|-------|---------|
| `/tob-scv-scan` | Pass 1 (after Slither) | 36-class vulnerability scan |
| `/tob-fp-check` | Pass 3 (pre-filter) | False positive gate before PoC generation |
| `/tob-variant-analysis` | Pass 2 (post-merge) | Pattern search for high/critical findings |
| `/tob-scv-scan` | RED + GOLD agents | Agents instructed to run before manual analysis |
| `/tob-fp-check` | BLUE agent | Self-challenge on VIOLATED properties |
| `/tob-token-integration-analyzer` | GOLD agent | Token-handling edge case detection |
| `/tob-spec-to-code-compliance` | BLUE agent | Cross-check property verification |
| `/tob-variant-analysis` | RED agent | Post-analysis variant search |

All skill integrations degrade gracefully — if a skill is not installed, the
pipeline continues without it.

## Context Files

Pre-populated for The Graph Protocol in `context/`:

- **AUDIT_CONTEXT.md** — Protocol overview, deployment, trust model, economic parameters
- **PROPERTIES.md** — 22 security invariants (P-1 through P-22)
- **KNOWN_ISSUES.md** — 4 accepted risks, 5 fixed issues, 3 focus areas
- **ATTACK_PATTERNS.md** — 10 known patterns from previous audits and bounties

Copied into the audit directory at container startup.

## Output Structure

```
audit-workspace/
+-- recon/                       # Pass 1
|   +-- recon-summary.json
|   +-- entry-points.json
|   +-- storage-layouts.json
|   +-- slither-results.json
|   +-- dependency-graph.json
|   +-- math-operations.json
|   +-- access-control.json
|   +-- proxy-mappings.json
|   +-- pragma-versions.json
+-- findings/                    # Pass 2
|   +-- red-agent-raw.json
|   +-- blue-agent-raw.json
|   +-- gold-agent-raw.json
|   +-- merged-deduplicated.json
|   +-- variant-analysis.json
|   +-- logs/
+-- pocs/                        # Pass 3
|   +-- *.t.sol
|   +-- validated-findings.json
|   +-- discarded-findings.json
+-- fuzzing/                     # Pass 4
|   +-- invariant-tests/
|   +-- fuzzing-campaign-results/
|   +-- fuzzing-findings.json
+-- formal/                      # Pass 5
|   +-- verification-summary.json
|   +-- formal-findings.json
+-- review/                      # Pass 6
|   +-- adversarial-review.json
+-- final-report.md
+-- final-report.json
+-- pipeline-status.json
```

## Configuration

`bulwark.toml` controls everything:

```toml
[target]
repo = "https://github.com/graphprotocol/contracts.git"
scope = ["packages/horizon", "packages/subgraph-service"]
core_contracts = ["HorizonStaking", "GraphPayments", "PaymentsEscrow"]

# Claude model for AI passes: "haiku" (cheapest), "sonnet", or "opus"
model = "haiku"

[passes.recon]
scv_scan = true              # AI vulnerability scan after Slither
scv_scan_max_turns = 20

[passes.agents]
max_turns = 80
agents = ["red", "blue", "gold"]
timeout_minutes = 60
variant_analysis = true      # Search for pattern variants post-merge

[passes.poc]
max_turns = 30
max_retries = 2
fp_check = true              # False-positive gate before PoC generation

[passes.fuzzing]
fuzz_runs = 10_000
invariant_depth = 50
model = "sonnet"             # Override model for test generation

[passes.formal]
solver_timeout = 300
loop_bound = 5
target_properties = ["P-1", "P-10", "P-15", "P-16", "P-19"]
model = "sonnet"             # Override model for test generation

[passes.review]
max_turns = 60
```

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `ANTHROPIC_API_KEY` | For AI passes | — | Or use `bulwark login` |
| `AUDIT_TARGET` | No | graphprotocol/contracts | Git repo URL |
| `AUDIT_BRANCH` | No | `main` | Branch to audit |

## Development

```bash
cargo check          # Type check
cargo test           # 68 unit tests
cargo clippy         # Lint
cargo build --release
```

## License

[MIT](LICENSE)
