# Doyran

Multi-pass, multi-agent smart contract audit pipeline for The Graph Protocol.
One Docker command to get a six-pass security analysis with adversarial AI agents,
PoC-gated findings, fuzzing, and formal verification.

## Pipeline Overview

```
Pass 1: Reconnaissance ──────────── Deterministic (Slither, Forge, grep)
  │
Pass 2: Multi-Agent Analysis ────── 3x parallel Claude sessions (RED/BLUE/GOLD)
  │
Pass 3: PoC Generation ─────────── "No PoC, no finding" — validates with Forge
  │
Pass 4: Fuzzing Campaign ───────── Foundry invariant tests + Medusa + Echidna
  │                                  (runs in parallel with Pass 5)
Pass 5: Formal Verification ────── Halmos bounded model checking
  │
Pass 6: Adversarial Review ─────── Fresh Claude session challenges everything
  │
  └──► final-report.md
```

## Quick Start

### 1. Configure

```bash
cp .env.example .env
# Set ANTHROPIC_API_KEY in .env (or use claude login inside container)
```

### 2. Build and enter

```bash
docker compose build
docker compose run --rm -it audit-env bash
```

### 3. Authenticate Claude Code

```bash
# API key (already set if .env configured):
echo $ANTHROPIC_API_KEY

# Or license login:
claude login
```

### 4. Run the pipeline

```bash
# Full pipeline (all 6 passes)
/home/auditor/pipeline/doyran-pipeline.sh

# Recon only (no AI, no auth needed)
/home/auditor/pipeline/doyran-pipeline.sh --pass 1

# Passes 1-3 (recon + agents + PoC gate)
/home/auditor/pipeline/doyran-pipeline.sh --pass 1-3

# Resume from a specific pass
/home/auditor/pipeline/doyran-pipeline.sh --resume 3
```

### 5. Or use interactively

```bash
# Standalone Slither analysis
/home/auditor/scripts/run-slither.sh

# Interactive Claude Code session
claude
```

## Agent Personas (Pass 2)

| Agent | Focus | Instruction |
|-------|-------|-------------|
| RED | Attacker | Find exploits that steal funds. Paid per critical finding. |
| BLUE | Systematic | Verify every property P-1 through P-22. Skipping is failure. |
| GOLD | Economic | Rounding errors, incentive misalignment, MEV, flash loans. |

Agents cannot see each other's output. Findings are merged and deduplicated
after all three complete.

## Validation Gate (Pass 3)

| PoC Status | Outcome |
|------------|---------|
| Compiles and demonstrates | Finding survives, severity preserved |
| Compiles but inconclusive | Finding survives, severity capped at Medium |
| Failed to compile | **Finding DISCARDED** |
| Infeasible conditions | **Finding DISCARDED** |
| Requires mainnet simulation | Flagged for manual review |

## Output Structure

```
audit-workspace/
├── recon/                    # Pass 1: structural data
│   ├── entry-points.json
│   ├── storage-layouts.json
│   ├── slither-results.json
│   ├── dependency-graph.json
│   ├── math-operations.json
│   ├── access-control.json
│   └── proxy-mappings.json
├── findings/                 # Pass 2: agent outputs
│   ├── red-agent-raw.json
│   ├── blue-agent-raw.json
│   ├── gold-agent-raw.json
│   └── merged-deduplicated.json
├── pocs/                     # Pass 3: validated PoCs
│   ├── F-001.t.sol
│   └── validated-findings.json
├── fuzzing/                  # Pass 4
├── formal/                   # Pass 5
├── review/                   # Pass 6
├── final-report.md           # Human-readable output
└── final-report.json         # Machine-readable output
```

## Context Files

Pre-populated for The Graph Protocol:

- **AUDIT_CONTEXT.md** — Protocol overview, trust model, in-scope contracts
- **PROPERTIES.md** — 22 security invariants (P-1 through P-22)
- **KNOWN_ISSUES.md** — 4 accepted risks, 5 fixed issues, 3 focus areas
- **ATTACK_PATTERNS.md** — 10 known patterns from previous audits and bounties

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `ANTHROPIC_API_KEY` | For AI passes | — | Or use `claude login` |
| `AUDIT_TARGET` | No | graphprotocol/contracts | Git repo URL |
| `AUDIT_BRANCH` | No | `main` | Branch to audit |

## Estimated Cost Per Run

| Pass | Cost |
|------|------|
| Pass 1 (Recon) | $0 |
| Pass 2 (3 agents) | ~$45 |
| Pass 3 (PoC generation) | ~$10-25 |
| Pass 4 (Fuzzing) | $0 |
| Pass 5 (Formal verification) | $0 |
| Pass 6 (Adversarial review) | ~$20 |
| **Total** | **$60-100** |

## Build Status

| Phase | Status |
|-------|--------|
| Phase 1: Pipeline skeleton + Pass 1 | Done |
| Phase 2: Agent prompts (RED/BLUE/GOLD) | Done |
| Phase 3: PoC pipeline | Done |
| Phase 4: Fuzzing integration | Done |
| Phase 5: Formal verification (Halmos) | Done |
| Phase 6: Review + reporting | Done |
| Phase 7: Calibration | Pending |

## License

Private — The Graph Protocol internal tooling.
