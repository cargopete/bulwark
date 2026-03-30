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

## Pass 3 (TODO)

- `poc-generator.md` — Foundry PoC writer for validated findings

## Pass 6 (TODO)

- `adversarial-reviewer.md` — Final review, challenges all conclusions
