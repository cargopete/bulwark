# Agent Prompts

Persona-specific prompts for each pipeline pass that uses Claude Code.

## Pass 2 Agents

- `red-agent.md` — Attacker persona. Finds exploits that steal funds. Paid per critical finding.
- `blue-agent.md` — Systematic verifier. Checks all 22 properties: VERIFIED / VIOLATED / UNCERTAIN.
- `gold-agent.md` — DeFi economist. Rounding, accumulation, MEV. Every finding needs numbers.

## Anti-Deference Mechanisms

All three prompts include:
1. **Persona framing** — paid per finding at the stated severity
2. **Severity calibration** — concrete GRT thresholds (not vague "could be bad")
3. **No cross-agent visibility** — agents can't see each other's output
4. **Burden of proof on dismissal** — "explain why it's NOT exploitable" for anything below Medium
5. **No hedging language** — "this IS exploitable" or say nothing

## Invocation

Each prompt is fed to Claude Code in headless mode by `pipeline/pass2-agents.sh`:

```bash
claude -p "$(cat prompts/red-agent.md)" --max-turns 80
```

Agents write their findings directly to `audit-workspace/findings/<agent>-raw.json`.

## Pass 3

- `poc-generator.md` — Foundry PoC writer. Receives one finding, writes a compilable test. Retries on build failure with error context.

## Pass 4

- `invariant-generator.md` — Generates Foundry invariant tests from PROPERTIES.md. Handler contracts expose fuzzable actions, invariant functions assert one property each.

## Pass 5

- `halmos-generator.md` — Generates Halmos symbolic tests for bounded model checking. Targets P-10, P-15, P-19, P-1, P-16. Uses `check_` prefix, `vm.assume()` for constraints.

## Pass 6

- `adversarial-reviewer.md` — Last line of defence. Challenges verified properties, severity ratings, discarded findings. Identifies compound attacks and blind spots. Assembles final report.
