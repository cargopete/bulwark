# Agent Prompts

This directory contains the persona-specific prompts for each pipeline pass that uses Claude Code.

## Structure (populated in Phase 2)

- `red-agent.md` — Attacker persona, focused on exploits and fund extraction
- `blue-agent.md` — Systematic auditor, property-by-property verification
- `gold-agent.md` — DeFi economist, rounding/incentive/MEV analysis
- `poc-generator.md` — Foundry PoC writer for validated findings
- `adversarial-reviewer.md` — Final review, challenges all conclusions

## Usage

Each prompt is fed to Claude Code in headless mode:
```bash
claude -p "$(cat prompts/red-agent.md)" --output-format json
```

The prompts reference files in `audit-workspace/recon/` produced by Pass 1.
