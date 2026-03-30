# Doyran — AI Smart Contract Audit Toolkit

A Docker-based, reproducible smart contract audit environment combining open source
static analysis tools with AI-augmented security review via Claude Code.

## What's Inside

| Layer | Tools | Purpose |
|-------|-------|---------|
| Static analysis | Slither | Known vulnerability pattern detection |
| Compilation & testing | Foundry (Forge), solc | Build, test, inspect storage layouts |
| AI infrastructure | Trail of Bits Skills (36 plugins) | Entry point mapping, FP verification, variant analysis |
| AI intelligence | forefy/.context (6 skills) | Multi-expert audit, protocol-type matching (10,600+ findings) |
| Orchestration | Claude Code | Ties everything together with structured audit prompts |

## Prerequisites

- [Docker Desktop](https://www.docker.com/products/docker-desktop/) installed and running
- One of the following for AI features:
  - An Anthropic API key (`ANTHROPIC_API_KEY`), **or**
  - A Claude Code license (Max, Team, or Enterprise plan)

## Step-by-Step Setup

### 1. Clone the repo

```bash
git clone https://github.com/cargopete/doyran.git
cd doyran
```

### 2. (Optional) Set your API key

Only needed if you're using API key auth. Skip this if you have a Claude Code license.

```bash
cp .env.example .env
# Edit .env and set ANTHROPIC_API_KEY=sk-ant-your-key-here
```

### 3. Build the Docker image

This takes ~3-5 minutes on first build. It installs Slither, Foundry, Claude Code,
and all the AI audit skills.

```bash
docker compose build
```

### 4. Start the audit environment

```bash
docker compose run --rm -it audit-env bash
```

On first run, the entrypoint will:
- Clone `graphprotocol/contracts` (~30 seconds)
- Install contract dependencies via pnpm (~30 seconds)
- Clone and install Trail of Bits Skills + forefy/.context (~10 seconds)
- Compile the Horizon contracts with Forge (~20 seconds)
- Print a status summary

You should see something like:

```
  Installed tools:
    ✓ Slither 0.11.5
    ✓ Foundry forge Version: 1.5.1-stable
    ✓ Claude Code
```

### 5. Authenticate Claude Code

You're now inside the container at a bash prompt.

**Option A: API key** — if you set `ANTHROPIC_API_KEY` in step 2, you're already
authenticated. Skip to step 6.

**Option B: Claude Code license** — run this at the bash prompt:

```bash
claude login
```

Follow the prompts to authenticate via your browser. This only needs to be done
once per container session.

### 6. Start auditing

You have three options:

**Interactive Claude Code session** (recommended for first use):

```bash
claude
```

You'll land in an interactive session with all audit context pre-loaded.
Try a prompt like:

```
Read AUDIT_CONTEXT.md, PROPERTIES.md and KNOWN_ISSUES.md, then audit
HorizonStaking for rounding errors in delegation pool math
```

**Slither static analysis** (no Claude auth needed):

```bash
/home/auditor/scripts/run-slither.sh
```

Runs Slither on all in-scope contracts and saves JSON + SARIF reports.

**Full automated workflow** (static analysis → AI deep audit):

```bash
/home/auditor/scripts/run-audit.sh
```

Runs Slither first, then launches Claude Code with a structured audit prompt
covering all critical areas.

## Example Audit Prompts

Once inside Claude Code, here are some useful starting points:

```
# Full scope audit
Audit all in-scope Horizon contracts against the 22 properties in PROPERTIES.md

# Focused analysis
Analyse the slashing logic in HorizonStaking.slash() — verify P-10 (provider-first
ordering) holds under all edge cases including concurrent slashes

# Entry point mapping
Map all external/public state-changing functions in packages/horizon/contracts/,
categorised by access level

# Rounding review (the $290K bounty area)
Review all division operations in delegation pool math. For each, determine
rounding direction and whether an attacker can exploit accumulated rounding

# Payment escrow race conditions
Analyse the thaw-then-collect interaction in PaymentsEscrow — can a payer
front-run collection by starting a thaw?
```

## Targeting a Different Repo

Override the default target to audit any Solidity codebase:

```bash
# Different repo
AUDIT_TARGET=https://github.com/example/contracts.git docker compose run --rm -it audit-env bash

# Different branch
AUDIT_BRANCH=feature/v2-upgrade docker compose run --rm -it audit-env bash
```

Note: the Graph-specific context files (AUDIT_CONTEXT.md, PROPERTIES.md,
KNOWN_ISSUES.md) will still be copied. Replace them with your own protocol's
context for best results.

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `ANTHROPIC_API_KEY` | For AI features* | — | Anthropic API key (*or use `claude login`) |
| `AUDIT_TARGET` | No | `graphprotocol/contracts` | Git repo URL to audit |
| `AUDIT_BRANCH` | No | `main` | Branch to check out |
| `AUDIT_SCOPE` | No | `packages/horizon packages/subgraph-service` | Packages to focus on |

## Directory Layout (inside container)

```
/home/auditor/
├── audits/
│   └── graph-contracts/          ← cloned target repo + context files
│       ├── AUDIT_CONTEXT.md      ← protocol overview, trust model
│       ├── PROPERTIES.md         ← 22 security invariants to verify
│       ├── KNOWN_ISSUES.md       ← accepted risks + focus areas
│       └── CLAUDE.md             ← audit instructions for Claude
├── tools/
│   ├── claude-code-config/       ← Trail of Bits config
│   ├── tob-skills/               ← Trail of Bits skills (36 plugins)
│   ├── tob-skills-curated/       ← scv-scan and curated skills (28 plugins)
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

1. **SubgraphService** — currently requires solc 0.8.33 (unreleased). Horizon
   contracts compile and analyse fine.

2. **Mythril** — optional dependency, fails to install on some platforms due to
   numpy/Python 3.12 incompatibility. Slither covers the primary use case.

3. **Trail of Bits Skills and forefy/.context** — installed at startup via git
   clone. If the repos change structure or are unavailable, the toolkit degrades
   gracefully to static analysis + vanilla Claude Code.

4. **Non-determinism** — running the same audit twice produces different results.
   For high-stakes reviews, run the AI analysis 2–3 times and union the findings.

5. **AI blind spots** — consistently misses: novel economic attacks, complex
   multi-tx exploit chains, governance manipulation, MEV-specific attacks, and
   subtle timing assumptions. These require human auditors.

6. **Requires human triage** — every AI finding should be verified before
   escalation. See the 6-gate triage checklist in the full research report.
