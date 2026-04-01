#!/usr/bin/env bash
set -euo pipefail

# Ensure all tool paths are available
export PATH="/home/auditor/.local/bin:/home/auditor/.foundry/bin:${PATH}"

AUDIT_DIR="/home/auditor/audits/graph-contracts"
AUDIT_TARGET="${AUDIT_TARGET:-https://github.com/graphprotocol/contracts.git}"
AUDIT_BRANCH="${AUDIT_BRANCH:-main}"

echo "╔══════════════════════════════════════════════════════╗"
echo "║  Bulwark — Smart Contract Audit Pipeline           ║"
echo "║  Multi-pass · Multi-agent · PoC-gated              ║"
echo "╚══════════════════════════════════════════════════════╝"
echo ""

# ── Inject host Claude credentials if mounted ─────────────────────────
if [ -f "/home/auditor/.claude-host-creds/credentials.json" ]; then
    cp /home/auditor/.claude-host-creds/credentials.json /home/auditor/.claude/.credentials.json
    echo "✓  Claude credentials injected from host."
fi

# ── Check Claude Code auth ────────────────────────────────────────────
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    # Check if user has an existing Claude Code login (Max/Team/Enterprise)
    if [ -f "/home/auditor/.claude/.credentials.json" ] || [ -f "/home/auditor/.claude-auth/.credentials.json" ]; then
        echo "✓  Claude Code authenticated via existing login."
    else
        echo "⚠  Claude Code not authenticated."
        echo "   Static analysis tools (Slither, Forge) still available."
        echo ""
        echo "   To authenticate, either:"
        echo "     a) Set ANTHROPIC_API_KEY in .env (API key auth)"
        echo "     b) Run 'claude login' inside the container (Max/Team/Enterprise)"
        echo ""
    fi
fi

# ── Clone or update target repo ─────────────────────────────────────
if [ ! -d "$AUDIT_DIR/.git" ]; then
    echo "→ Cloning $AUDIT_TARGET ($AUDIT_BRANCH)..."
    git clone --branch "$AUDIT_BRANCH" --depth 50 "$AUDIT_TARGET" "$AUDIT_DIR"
else
    echo "→ Updating existing clone..."
    cd "$AUDIT_DIR" && git fetch origin && git reset --hard "origin/$AUDIT_BRANCH"
fi

cd "$AUDIT_DIR"

# ── Install dependencies ────────────────────────────────────────────
echo "→ Installing contract dependencies..."
if [ -f "package.json" ]; then
    pnpm install --frozen-lockfile 2>/dev/null || pnpm install
fi

# ── Copy audit context files ────────────────────────────────────────
echo "→ Setting up audit context files..."
cp /home/auditor/context/AUDIT_CONTEXT.md "$AUDIT_DIR/AUDIT_CONTEXT.md"
cp /home/auditor/context/PROPERTIES.md "$AUDIT_DIR/PROPERTIES.md"
cp /home/auditor/context/KNOWN_ISSUES.md "$AUDIT_DIR/KNOWN_ISSUES.md"
cp /home/auditor/context/ATTACK_PATTERNS.md "$AUDIT_DIR/ATTACK_PATTERNS.md"

# Create project-level CLAUDE.md that references the context files
if [ ! -f "$AUDIT_DIR/CLAUDE.md" ]; then
    cat > "$AUDIT_DIR/CLAUDE.md" << 'CLAUSEMD'
# Graph Protocol Audit Context

This is The Graph Protocol contracts monorepo. Primary audit target is packages/horizon.

Read AUDIT_CONTEXT.md, PROPERTIES.md, and KNOWN_ISSUES.md before starting any audit work.

## Key Commands
- `cd packages/horizon && pnpm test` — Run Horizon tests (Foundry)
- `cd packages/subgraph-service && pnpm test` — Run SubgraphService tests
- `slither packages/horizon/contracts/ --config-file slither.config.json` — Static analysis
- `forge test -vvv` — Verbose test output

## Architecture
- Upgradeable proxy pattern (OpenZeppelin) for all contracts
- HorizonStaking is the core — handles provisions, delegation, slashing
- GraphPayments + PaymentsEscrow handle fee distribution
- SubgraphService is the first data service implementation
- GraphTallyCollector verifies aggregated payment receipts (RAVs)

## Trust Assumptions
- Graph Council (multisig) is the Controller owner — can upgrade any contract
- Data services are the slashing authority for their provisions
- Operators can manage provisions but NEVER withdraw tokens
- RAV valueAggregate is monotonically increasing per (payer, SP, dataService)

## Known Sensitive Areas
- Delegation pool share/token math (rounding — $290K bounty paid for rounding bugs)
- Provision/thawing period enforcement (front-running prevention)
- Slashing order (provider first, then delegators)
- PaymentsEscrow thaw-then-collect race conditions
- Storage gap management in proxy upgrades
CLAUSEMD
fi

# ── Install Trail of Bits + forefy skills ────────────────────────────
echo "→ Installing AI audit skills..."
/home/auditor/scripts/install-skills.sh

# ── Compile contracts ────────────────────────────────────────────────
echo "→ Compiling contracts..."
if [ -d "packages/horizon" ]; then
    (cd packages/horizon && forge build 2>/dev/null) && echo "  ✓ Horizon compiled" || echo "  ✗ Horizon build failed (may need manual setup)"
fi
if [ -d "packages/subgraph-service" ]; then
    (cd packages/subgraph-service && forge build 2>/dev/null) && echo "  ✓ SubgraphService compiled" || echo "  ✗ SubgraphService build failed (may need manual setup)"
fi

# ── Restore Claude Code settings (something overwrites ours during setup) ─
cp /home/auditor/.bulwark-settings.json /home/auditor/.claude/settings.json 2>/dev/null || true

# ── Print status (only in interactive mode) ──────────────────────────
cd "$AUDIT_DIR"

# If running an explicit command (not bash), skip the banner and just exec it
if [ $# -gt 0 ] && [ "$1" != "bash" ] && [ "$1" != "sh" ]; then
    exec "$@"
fi

echo ""
echo "════════════════════════════════════════════════════════════"
echo "  Ready. Working directory: $AUDIT_DIR"
echo ""
echo "  Available commands:"
echo "    bulwark run                   — Run full 6-pass pipeline"
echo "    bulwark run --pass 1          — Recon only (no AI)"
echo "    bulwark run --pass 2-3        — Agents + PoC Gate"
echo "    bulwark run --pass 2 --agent red  — Single agent run"
echo "    bulwark status                — Pipeline status"
echo "    bulwark findings              — List findings"
echo "    bulwark report                — Regenerate report"
echo "    bulwark doctor                — Check tool availability"
echo "    bulwark login                 — Authenticate Claude"
echo "    claude                       — Claude Code (interactive)"
echo ""
echo "  Installed tools:"
command -v slither >/dev/null && echo "    ✓ Slither $(slither --version 2>&1 | head -1)" || echo "    ✗ Slither"
command -v myth >/dev/null && echo "    ✓ Mythril" || echo "    ✗ Mythril"
command -v forge >/dev/null && echo "    ✓ Foundry $(forge --version 2>&1 | head -1)" || echo "    ✗ Foundry"
command -v claude >/dev/null && echo "    ✓ Claude Code" || echo "    ✗ Claude Code"
echo "════════════════════════════════════════════════════════════"
echo ""

exec "$@"
