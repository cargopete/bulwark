# Graph Protocol AI Audit Toolkit

A Docker-based, reproducible smart contract audit environment combining open source
static analysis tools with AI-augmented security review via Claude Code.

## What's Inside

| Layer | Tools | Purpose |
|-------|-------|---------|
| Static analysis | Slither, Mythril | Known vulnerability pattern detection |
| Compilation & testing | Foundry (Forge), solc | Build, test, inspect storage layouts |
| AI infrastructure | Trail of Bits Skills | Entry point mapping, FP verification, variant analysis |
| AI intelligence | forefy/.context | Multi-expert audit, protocol-type matching (10,600+ findings) |
| Orchestration | Claude Code | Ties everything together with structured audit prompts |

## Quick Start

```bash
# 1. Copy and configure environment
cp .env.example .env
# Edit .env — add your ANTHROPIC_API_KEY

# 2. Build and start
docker compose up --build -d

# 3. Enter the audit environment
docker compose exec -it audit-env bash

# 4. Inside the container — three options:

# Option A: Static analysis only (no API key needed)
~/scripts/run-slither.sh

# Option B: Full automated audit workflow
~/scripts/run-audit.sh

# Option C: Interactive Claude Code session
claude
```

## What Happens on Startup

1. Clones `graphprotocol/contracts` (or your configured target repo)
2. Installs contract dependencies via pnpm
3. Attempts to install Trail of Bits Skills and forefy/.context from GitHub
4. Copies Graph-specific audit context files (AUDIT_CONTEXT.md, PROPERTIES.md, KNOWN_ISSUES.md)
5. Compiles contracts with Forge
6. Prints a status summary of all available tools

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `ANTHROPIC_API_KEY` | For AI features | — | Claude API key |
| `AUDIT_TARGET` | No | graphprotocol/contracts | Git repo URL to audit |
| `AUDIT_BRANCH` | No | `main` | Branch to check out |
| `AUDIT_SCOPE` | No | `packages/horizon packages/subgraph-service` | Packages to focus on |

## Directory Layout (inside container)

```
/home/auditor/
├── audits/
│   └── graph-contracts/          ← cloned target repo + context files
│       ├── AUDIT_CONTEXT.md
│       ├── PROPERTIES.md
│       ├── KNOWN_ISSUES.md
│       └── CLAUDE.md
├── tools/
│   ├── claude-code-config/       ← Trail of Bits config
│   ├── tob-skills/               ← Trail of Bits skills
│   ├── tob-skills-curated/       ← scv-scan and other curated skills
│   └── forefy-context/           ← forefy/.context audit skills
├── scripts/
│   ├── run-slither.sh            ← standalone static analysis
│   ├── run-audit.sh              ← full audit workflow
│   └── install-skills.sh         ← skill installation (runs at startup)
└── .claude/
    ├── settings.json             ← Claude Code permissions and guardrails
    ├── CLAUDE.md                 ← global audit instructions
    ├── commands/                 ← Trail of Bits slash commands
    └── skills/                   ← forefy audit skills
```

## Audit Workflows

### A. Grant Recipient Review (2-4 hours)
For evaluating untrusted code from grant recipients. Runs sandboxed.
```bash
# Override the target repo
AUDIT_TARGET=https://github.com/example/grant-repo.git docker compose up --build
```

### B. Protocol Upgrade Review (3-6 hours)
For reviewing PRs or branches with protocol changes.
```bash
AUDIT_BRANCH=feature/horizon-v2 docker compose up --build
```

### C. Immunefi Bounty Sweep (4-8 hours)
Proactive security review of the full bounty scope. Use the default config.

## Realistic Expectations

| Metric | Range |
|--------|-------|
| False positive rate (after FP verification) | 25–40% |
| Critical/High recall vs human auditor | 40–50% |
| Unique findings not in human audit | 1–3 per engagement |
| Token cost per audit | $15–$50 |

This is a **force multiplier**, not a replacement for human auditors. It catches
the 1–3 findings that both audit firms missed because they reviewed different
parts of the codebase in isolation.

## Known Limitations

1. **Trail of Bits Skills and forefy/.context repos** — these are installed at
   startup via git clone. If the repos have changed structure or are unavailable,
   the toolkit degrades gracefully to static analysis + vanilla Claude Code.

2. **Non-determinism** — running the same audit twice produces different results.
   For high-stakes reviews, run the AI analysis 2–3 times and union the findings.

3. **AI misses consistently**: novel economic attacks, complex multi-tx exploit
   chains, governance manipulation, MEV-specific attacks, subtle timing assumptions.

4. **Requires human triage** — every AI finding must pass the 6-gate checklist
   before escalation (see the full report in research/).
