# Bulwark

Multi-pass, multi-agent smart contract audit pipeline for The Graph Protocol.
Rust CLI + Docker container with Slither, Forge, Claude Code, and 70 AI audit skills.

> **Status**: Passes 1-2 tested and working (Recon + Agents produce real findings).
> Pass 3 (PoC Gate) runs but PoCs struggle to compile against Graph's dependency tree.
> Passes 4-6 have code but are untested — Pass 4/5 need missing tools (Halmos, Medusa, Echidna).
> See [ROADMAP.md](ROADMAP.md) for full status.

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
Pass 1: Reconnaissance ──────── Deterministic (Slither, Forge, Rust)     [Tested ✓]
  |
Pass 2: Multi-Agent Analysis ── 3x parallel Claude (RED/BLUE/GOLD)       [Tested ✓]
  |
Pass 3: PoC Generation ──────── "No PoC, no finding" gate (Forge)        [Runs, PoCs fail to compile]
  |
Pass 4: Fuzzing Campaign ────── Foundry invariant tests + Medusa         [Untested, tools missing]
  |                               (runs in parallel with Pass 5)
Pass 5: Formal Verification ─── Halmos bounded model checking            [Untested, tools missing]
  |
Pass 6: Adversarial Review ──── Fresh Claude session challenges all       [Untested]
  |
  +---> final-report.md
```

### Pass 1: Reconnaissance (tested, working)

All Rust, no AI. Produces structured JSON consumed by all later passes:
- Compiles contracts (`forge build`)
- Runs Slither static analysis (H/M/L severity counts)
- Maps all external/public state-changing entry points
- Extracts storage layouts via `forge inspect`
- Builds inheritance/dependency graph
- Enumerates access control modifiers and roles
- Inventories arithmetic operations (division, multiplication)
- Identifies proxy relationships

### Pass 2: Multi-Agent Analysis (tested, working — 40 raw → 12 unique findings)

Three independent Claude Code sessions run in parallel. Agents cannot see
each other's output. Each reads Pass 1 recon data + context files + source code.

| Agent | Persona | Focus |
|-------|---------|-------|
| RED | Attacker | Exploits that steal funds. Paid per critical finding. |
| BLUE | Systematic verifier | Verify/refute all 22 properties (P-1 to P-22). |
| GOLD | DeFi economist | Rounding errors, MEV, flash loans. Must include numbers. |

After completion, findings are merged and deduplicated with severity
disagreement tracking.

### Pass 3: PoC Gate (tested — runs but PoCs fail to compile)

For each finding from Pass 2:
1. Claude generates a Foundry test PoC
2. `forge build` — must compile
3. `forge test` — must demonstrate the vulnerability
4. Findings that fail are discarded

Tested: runs and processes all findings, but generated PoCs fail to compile
against Graph's complex dependency tree. Likely needs a test harness template
or a smarter model (sonnet/opus) for this pass.

### Passes 4-6 (untested)

- **Pass 4**: Claude generates invariant tests from PROPERTIES.md, Forge runs them.
  Medusa/Echidna for extended fuzzing (not installed). Attempted but hung — needs debugging.
- **Pass 5**: Claude generates symbolic tests, Halmos verifies (not installed).
- **Pass 6**: Adversarial review Claude session challenges all findings.
  Generates final markdown + JSON report. No missing dependencies — just needs testing.

## Installed AI Skills

The container auto-installs 70 audit skills at startup from three sources:

| Source | Count | What |
|--------|-------|------|
| [Trail of Bits skills](https://github.com/trailofbits/skills) | 36 | entry-point-analyzer, fp-check, variant-analysis, property-based-testing, etc. |
| [Trail of Bits skills-curated](https://github.com/trailofbits/skills-curated) | 28 | scv-scan (36 Solidity vuln classes), and others |
| [forefy/.context](https://github.com/forefy/.context) | 6 | smart-contract-security-audit, foundry-poc, sandboxed-audit-runner, etc. |

These are available to Claude agents during their sessions as slash commands.
Currently **not explicitly orchestrated** by the pipeline — agents may or may
not use them. Wiring skills into specific passes is on the roadmap (Phase 1).

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
|   +-- logs/
+-- pocs/                        # Pass 3
|   +-- *.t.sol
|   +-- validated-findings.json
+-- fuzzing/                     # Pass 4
+-- formal/                      # Pass 5
+-- review/                      # Pass 6
|   +-- final-report.md
|   +-- final-report.json
+-- pipeline-status.json
```

## Configuration

`bulwark.toml` controls everything:

```toml
[target]
repo = "https://github.com/graphprotocol/contracts.git"
scope = ["packages/horizon", "packages/subgraph-service"]
core_contracts = ["HorizonStaking", "GraphPayments", "PaymentsEscrow", ...]

# Claude model for AI passes: "haiku" (cheapest), "sonnet", or "opus"
model = "haiku"

[passes.agents]
max_turns = 80
agents = ["red", "blue", "gold"]
timeout_minutes = 60

[passes.poc]
max_turns = 30
max_retries = 2

[passes.fuzzing]
fuzz_runs = 10_000
invariant_depth = 50
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
cargo test           # 66 unit tests
cargo clippy         # Lint
cargo build --release
```

## License

Private — The Graph Protocol internal tooling.
