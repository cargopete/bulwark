# Bulwark Roadmap

Status of bulwark vs the target architecture from the AI-augmented audit toolkit report.

## Current State (v0.1)

### What works

| Component | Status | Detail |
|---|---|---|
| **Pass 1: Recon** | **Tested ✓** | 9 sub-steps in Rust: forge build, Slither, entry points, storage layouts, dependency graph, access control, math ops, proxies, summary. Produces real structured JSON (Slither H:28 M:75 L:48). |
| **Pass 2: Agents** | **Tested ✓** | RED/BLUE/GOLD run in parallel, produced 40 raw findings → 12 unique after merge/dedup. Report generated: C:1 H:2 M:5 L:4. |
| **Pass 3: PoC Gate** | **Tested, PoCs fail** | Runs and processes all 12 findings, but generated PoCs fail to compile against Graph's complex dependency tree. Needs test harness template or smarter model. |
| **Pass 4: Fuzzing** | **Tested ✓** | Generated 6 invariant tests that compile. Forge runs them but `--match-contract Invariant` filter doesn't match (0/0) — needs filter fix. Medusa/Echidna not installed. |
| **Pass 5: Formal** | **Tested ✓** | Generated 6 symbolic tests that compile. Halmos not installed so can't verify, but tests are ready. |
| **Pass 6: Review** | **Tested ✓** | Adversarial review works: 2 severity upgrades, 4 compound attacks, 8 blind spots. Generates markdown + JSON reports. |
| **Docker container** | **Working** | Ubuntu 24.04, Forge, Slither, Claude Code, Node 22. Builds and runs |
| **CLI** | **Working** | run, status, findings, validate, report, doctor, login. All subcommands implemented |
| **Config** | **Working** | bulwark.toml with per-pass settings, tool paths, target scope |
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

## Phase 0: Prove the core works ✓ DONE

**Goal**: Get all passes running and producing real output.

- [x] Fix Write permission in settings.json
- [x] Fix let-chain compilation for Docker (Rust 1.85)
- [x] Fix config path resolution in container
- [x] Fix settings.json runtime overwrite (backup/restore pattern)
- [x] Run Pass 2 with Write enabled — 40 raw findings, 12 unique after merge
- [x] Run Pass 3 — runs but PoCs fail to compile (Graph dependency tree too complex for Haiku)
- [x] Default model to haiku, configurable via bulwark.toml
- [x] Debug Pass 4 hang — was just silent, no progress output. Added debug lines, works fine.
- [x] Pass 4: 6 invariant tests generated and compiled. Filter mismatch (0/0) — minor fix needed.
- [x] Pass 5: 6 symbolic tests generated and compiled. Halmos not installed.
- [x] Pass 6: Adversarial review works — severity upgrades, compound attacks, blind spots, full report.

**Remaining from Phase 0:**
- [x] Fix Pass 4 test filter — copy tests into forge project, use `--match-path` instead of `--match-contract`
- [x] Remove debug eprintln lines from Pass 4 (fuzzing.rs)
- [ ] Improve Pass 3: add test harness template or use sonnet for PoC generation
- [ ] Check agent logs — are they reading recon output? Using installed skills?

## Phase 1: Wire skills into the pipeline ✓ DONE

**Goal**: Stop hoping agents use installed skills and make it explicit.

### 1a. Add scv-scan to Pass 1 ✓

- [x] After Slither, invoke Claude with `/tob-scv-scan` slash command
- [x] Save output to `audit-workspace/recon/scv-scan-results.json`
- [x] Graceful degradation: skips if skill not installed or Claude not authenticated
- [x] Configurable via `bulwark.toml`: `scv_scan = true`, `scv_scan_max_turns = 20`

### 1b. Add fp-check as a gate in Pass 3 ✓

- [x] Before PoC generation, run each finding through `/tob-fp-check`
- [x] Discard findings that return FALSE_POSITIVE
- [x] Fail-open design: skill errors or missing skill = finding passes through
- [x] Configurable via `bulwark.toml`: `fp_check = true`, `fp_check_max_turns = 10`

### 1c. Tell agents to use specific skills ✓

- [x] RED agent: instructed to run `/tob-scv-scan` + `/tob-variant-analysis`
- [x] BLUE agent: instructed to run `/tob-fp-check` + `/tob-spec-to-code-compliance`
- [x] GOLD agent: instructed to run `/tob-token-integration-analyzer` + `/tob-scv-scan`
- [x] All instructions include "if the skill is not available, proceed without it"

### 1d. Add variant-analysis post-processing ✓

- [x] After merge/dedup in Pass 2, run `/tob-variant-analysis` on high/critical findings
- [x] Search for the same pattern elsewhere in scope
- [x] Write additional instances to `findings/variant-analysis.json`
- [x] Configurable via `bulwark.toml`: `variant_analysis = true`, `variant_max_turns = 15`

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
bulwark run                          # full 6-pass sweep
bulwark run --pass 1-3               # quick sweep (recon + agents + PoC)
bulwark run --pass 2 --agent red     # targeted single-agent
```

### 3b. Grant review mode

Lighter workflow for reviewing untrusted grant recipient code:
```
bulwark review --mode grant <repo-url>
```
- Clones repo into sandboxed workspace
- Runs: entry-point-analyzer + scv-scan + code-maturity-assessor + fp-check
- Produces triage scorecard, not full audit report
- Single Claude session, not multi-agent

### 3c. Upgrade review mode

Diff-focused workflow for protocol upgrades:
```
bulwark review --mode upgrade --base main --head feature-branch
```
- Runs: differential-review on the diff
- Runs: spec-to-code-compliance against GIP specs
- Runs: entry-point diff (before/after comparison)
- Focused agents on changed files only
- variant-analysis on any findings

### 3d. Config-driven workflow selection

Add to bulwark.toml:
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
- `bulwark stats` command to compute FPR across engagements
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

- [ ] `bulwark init <repo-url>` — scaffold a new audit engagement
- [ ] `bulwark diff --base main` — quick upgrade review without full pipeline
- [ ] Progress persistence — resume from last completed pass after container restart
- [ ] Findings dashboard — HTML report with filtering, not just CLI table
- [ ] Cost tracking — estimate token usage per pass
- [ ] Parallel pass execution — run Pass 4+5 concurrently (both post-PoC)
- [ ] Agent prompt versioning — track which prompt version produced which findings
- [ ] MCP server integration — Exa search for CVE/vulnerability lookups during analysis
