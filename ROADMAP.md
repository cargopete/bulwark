# Doyran Roadmap

Status of doyran vs the target architecture from the AI-augmented audit toolkit report.

## Current State (v0.1)

### What works

| Component | Status | Detail |
|---|---|---|
| **Pass 1: Recon** | **Working** | 9 sub-steps in Rust: forge build, Slither, entry points, storage layouts, dependency graph, access control, math ops, proxies, summary |
| **Pass 2: Agents** | **Plumbing works** | Launches RED/BLUE/GOLD Claude sessions in parallel. Prompts are well-crafted with Graph-specific scope, severity calibration, and structured JSON output. Merge/dedup logic is real. **Untested with actual findings** (Write permission was missing, now fixed) |
| **Pass 3: PoC Gate** | **Code exists** | Claude generates Foundry PoCs per finding, runs forge build + test, discards failures. Never executed (Pass 2 returned 0 findings) |
| **Pass 4: Fuzzing** | **Code exists** | Claude generates invariant tests, Forge runs them. Medusa/Echidna integration coded but tools not installed |
| **Pass 5: Formal** | **Code exists** | Claude generates symbolic tests. Halmos runner coded but tool not installed |
| **Pass 6: Review** | **Code exists** | Adversarial review session + markdown/JSON report generation. Never executed |
| **Docker container** | **Working** | Ubuntu 24.04, Forge, Slither, Claude Code, Node 22. Builds and runs |
| **CLI** | **Working** | run, status, findings, validate, report, doctor, login. All subcommands implemented |
| **Config** | **Working** | doyran.toml with per-pass settings, tool paths, target scope |
| **Skills installation** | **Working** | 36 Trail of Bits + 28 curated + 6 forefy skills auto-installed at container startup |
| **Context files** | **Working** | AUDIT_CONTEXT.md, PROPERTIES.md, KNOWN_ISSUES.md, ATTACK_PATTERNS.md — copied into audit dir |
| **Finding merge/dedup** | **Working** | Hash-based dedup, severity disagreement tracking, found-by attribution |
| **Schema validation** | **Working** | finding.schema.json + validate subcommand |
| **Tests** | **Working** | 66 unit tests across 8 modules |
| **CI** | **Working** | GitHub Actions: check, build, docker |

### What's installed but not orchestrated

The pipeline installs 70 third-party skills into Claude's commands directory. These are available to the agents during their sessions, but no pass explicitly invokes them. The agents *might* use them; they *might not*. It depends on what Claude decides to do.

| Skill | Available | Explicitly invoked by a pass |
|---|---|---|
| scv-scan (36 vuln classes) | Yes | No |
| fp-check (false positive gate) | Yes | No |
| entry-point-analyzer | Yes (but Pass 1 has its own Rust version) | No |
| variant-analysis | Yes | No |
| differential-review | Yes | No |
| token-integration-analyzer | Yes | No |
| property-based-testing | Yes | No |
| spec-to-code-compliance | Yes | No |
| smart-contract-security-audit (forefy) | Yes | No |
| foundry-poc (forefy) | Yes (but Pass 3 has its own prompt) | No |

### What's not implemented at all

| Report feature | Status |
|---|---|
| Three workflow modes (grant review, upgrade review, bounty sweep) | Single pipeline only |
| Non-determinism mitigation (2-3x runs, union of findings) | Not implemented |
| FPR measurement and tracking | Not implemented |
| Triage checklist automation (6 gates) | Not implemented |
| Code maturity scorecard | Not implemented |
| Escalation format generation | Not implemented |

---

## Phase 0: Prove the core works (NOW)

**Goal**: Get a single end-to-end run of Pass 1-3 that produces real findings.

- [x] Fix Write permission in settings.json
- [x] Fix let-chain compilation for Docker (Rust 1.85)
- [x] Fix config path resolution in container
- [ ] Run Pass 2 with Write enabled, verify agents produce findings
- [ ] Run Pass 3, verify PoC generation and validation gate works
- [ ] Check agent logs — are they reading recon output? Using installed skills?
- [ ] Tune prompts if agents are underperforming

**Success criteria**: At least 5 findings from Pass 2, at least 1 survives Pass 3 PoC gate.

## Phase 1: Wire skills into the pipeline

**Goal**: Stop hoping agents use installed skills and make it explicit.

### 1a. Add scv-scan to Pass 1

Pass 1 currently does its own static analysis (Slither + custom Rust). Add an explicit scv-scan step:
- After Slither, invoke Claude with the scv-scan slash command
- Save output to `audit-workspace/recon/scv-scan-results.json`
- Agent prompts already reference recon output, so findings flow naturally

### 1b. Add fp-check as a gate in Pass 3

Currently Pass 3 validates findings by generating PoCs. Add fp-check as a pre-filter:
- Before PoC generation, run each finding through fp-check
- Discard findings that fail adversarial challenge
- Then generate PoCs only for survivors
- This should reduce wasted Claude turns on false positives

### 1c. Tell agents to use specific skills

Update the agent prompts (red/blue/gold) to explicitly invoke installed skills:
- RED: "Run `/tob-scv-scan` on your target contracts before manual analysis"
- BLUE: "Use `/tob-fp-check` to challenge each VIOLATED property before reporting"
- GOLD: "Use `/tob-token-integration-analyzer` on GRT-handling contracts"

### 1d. Add variant-analysis post-processing

After merge/dedup in Pass 2, run a follow-up Claude session:
- For each unique finding, invoke variant-analysis
- Search for the same pattern elsewhere in scope
- Add any new instances as additional findings

**Success criteria**: Agents explicitly invoke at least 2 skills per session. FPR measurably lower than raw output.

## Phase 2: Complete the toolchain

**Goal**: Get Passes 4-6 fully operational.

### 2a. Install missing tools in Docker

```dockerfile
# Halmos (formal verification)
RUN pip3 install halmos

# Medusa (fuzzer)
RUN curl -L https://github.com/crytic/medusa/releases/latest/... -o /usr/local/bin/medusa

# Echidna (fuzzer)
RUN curl -L https://github.com/crytic/echidna/releases/latest/... -o /usr/local/bin/echidna
```

### 2b. Validate Pass 4 (Fuzzing)

- Verify Claude generates valid invariant test files
- Verify forge runs them and detects broken invariants
- Test Medusa/Echidna integration if installed

### 2c. Validate Pass 5 (Formal)

- Verify Claude generates valid Halmos symbolic tests
- Verify Halmos runs and produces verification results
- Map results back to PROPERTIES.md properties

### 2d. Validate Pass 6 (Review)

- Run adversarial review on findings from Passes 2-5
- Verify report generation (markdown + JSON)
- Check severity challenge and reinstatement logic

**Success criteria**: Full 6-pass run completes with real output at every stage.

## Phase 3: Multiple workflow modes

**Goal**: Support the three audit workflows from the report.

### 3a. Bounty sweep mode (current pipeline)

What we have now, polished:
```
doyran run                          # full 6-pass sweep
doyran run --pass 1-3               # quick sweep (recon + agents + PoC)
doyran run --pass 2 --agent red     # targeted single-agent
```

### 3b. Grant review mode

Lighter workflow for reviewing untrusted grant recipient code:
```
doyran review --mode grant <repo-url>
```
- Clones repo into sandboxed workspace
- Runs: entry-point-analyzer + scv-scan + code-maturity-assessor + fp-check
- Produces triage scorecard, not full audit report
- Single Claude session, not multi-agent

### 3c. Upgrade review mode

Diff-focused workflow for protocol upgrades:
```
doyran review --mode upgrade --base main --head feature-branch
```
- Runs: differential-review on the diff
- Runs: spec-to-code-compliance against GIP specs
- Runs: entry-point diff (before/after comparison)
- Focused agents on changed files only
- variant-analysis on any findings

### 3d. Config-driven workflow selection

Add to doyran.toml:
```toml
[workflow]
mode = "bounty"  # bounty | grant | upgrade
```

**Success criteria**: Three distinct workflow modes, each producing appropriate output.

## Phase 4: Quality measurement

**Goal**: Track and improve finding quality.

### 4a. Non-determinism mitigation

- Add `--runs N` flag to repeat agent passes N times
- Take union of findings across runs
- Track which findings appear in all runs (high confidence) vs some (lower confidence)
- Default to 1 run, recommend 3 for high-stakes audits

### 4b. FPR tracking

- After human triage, record which findings were true/false positives
- Store in `audit-workspace/triage/` as JSON
- `doyran stats` command to compute FPR across engagements
- Track per-agent FPR (is RED more accurate than GOLD?)

### 4c. Automated triage gates

Implement the 6-gate checklist from the report as code:
1. Scope check (is contract in scope?)
2. Code path reachability (does the entry point trace work?)
3. fp-check pass
4. Attack describability (is attack_scenario concrete?)
5. Severity classification
6. Evidence quality (PoC exists? Lines specified?)

Auto-tag findings: `gate_passed: [1,2,3,4,5,6]` or `gate_failed: [3]`.

**Success criteria**: Measurable FPR below 40% on Graph Protocol contracts.

## Phase 5: Operational polish

- [ ] `doyran init <repo-url>` — scaffold a new audit engagement
- [ ] `doyran diff --base main` — quick upgrade review without full pipeline
- [ ] Progress persistence — resume from last completed pass after container restart
- [ ] Findings dashboard — HTML report with filtering, not just CLI table
- [ ] Cost tracking — estimate token usage per pass
- [ ] Parallel pass execution — run Pass 4+5 concurrently (both post-PoC)
- [ ] Agent prompt versioning — track which prompt version produced which findings
- [ ] MCP server integration — Exa search for CVE/vulnerability lookups during analysis
