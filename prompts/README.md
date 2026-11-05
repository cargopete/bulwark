# Prompts

Each Claude Code session the pipeline starts is built from one of these files, plus the context
the pipeline appends (scope, test infrastructure, impact table, output paths) and an optional
persona. A project can override any file by placing its own copy under `<project>/prompts/`.

| File | Pass | Session |
|---|---|---|
| `context-generator.md` | 1 | Writes AUDIT_CONTEXT.md and PROPERTIES.md when the target has none |
| `red-agent.md` | 2 | Attacker: exploits that take funds |
| `blue-agent.md` | 2 | Verifier: VERIFIED, VIOLATED or UNCERTAIN for every property in PROPERTIES.md |
| `gold-agent.md` | 2 | Economist: rounding, accumulation, MEV, with numbers |
| `hunter-agent.md` | 2 | One per `[[passes.agents.components]]` entry, scoped to its files |
| `poc-generator.md` | 3 | Fork-native Foundry PoC for one finding, logging measured impact |
| `invariant-generator.md` | 4 | One suite for Forge, Medusa and Echidna |
| `halmos-generator.md` | 5 | Real-contract checks and import-free models, keyed by property ID |
| `adversarial-reviewer.md` | 6 | Challenges severity, reinstates discards, finds compound attacks and blind spots |

Agents return findings as JSON in the shape of `schemas/finding.schema.json`, including
`impact` (an `I-<n>` row from the programme), `asset`, and `recommendation`. Scratch tests go
under the project's test directory; the pipeline rewrites `test/scratch/` in these prompts to
the real path.

## What the prompts hold agents to

- No agent sees another's output.
- Severity is the programme's, by impact row, never the model's own scale.
- Dismissal carries the burden of proof: anything rated below Medium says why it is not
  exploitable.
- A PoC proves impact only by logging `bulwark:attacker_gain` or `bulwark:victim_loss`; a retry
  is never asked to make a test pass.
- Files are read with the Read tool; a shell read under `~/.claude` is refused.

## Personas

`personas/` holds optional voices for the logbook each session keeps (`[personas]` in
`bulwark.toml` maps a stage to one). `_logbook.md` is the shared instruction: status lines, not
reasoning. A persona changes tone only, never what a session must do.
