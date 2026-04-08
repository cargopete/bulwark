#!/usr/bin/env bash
set -euo pipefail

# Ensure all tool paths are available
export PATH="/home/auditor/.local/bin:/home/auditor/.foundry/bin:${PATH}"

# ── Resolve project config ────────────────────────────────────────────
# The project directory is mounted at /home/auditor/project.
# If it contains a bulwark.toml, use it — otherwise fall back to the
# built-in Graph Protocol audit config.

PROJECT_CONFIG=""
if [ -f "/home/auditor/project/bulwark.toml" ]; then
    PROJECT_CONFIG="/home/auditor/project/bulwark.toml"
    AUDIT_TARGET_FROM_CONFIG=$(grep 'repo = ' "$PROJECT_CONFIG" | head -1 | sed 's/.*repo = "\(.*\)"/\1/' || true)
    AUDIT_TARGET="${AUDIT_TARGET:-$AUDIT_TARGET_FROM_CONFIG}"
    AUDIT_BRANCH_FROM_CONFIG=$(grep 'branch = ' "$PROJECT_CONFIG" | head -1 | sed 's/.*branch = "\(.*\)"/\1/' || true)
    AUDIT_BRANCH="${AUDIT_BRANCH:-${AUDIT_BRANCH_FROM_CONFIG:-main}}"
else
    echo "⚠  No project mounted at /home/auditor/project — using built-in Graph Protocol config."
    PROJECT_CONFIG="/home/auditor/bulwark.toml"
    AUDIT_TARGET="${AUDIT_TARGET:-https://github.com/graphprotocol/contracts.git}"
    AUDIT_BRANCH="${AUDIT_BRANCH:-main}"
fi

# ── Derive audit directory from repo URL ─────────────────────────────
if [ -z "${AUDIT_DIR:-}" ]; then
    REPO_NAME=$(basename "$AUDIT_TARGET" .git)
    AUDIT_DIR="/home/auditor/audits/$REPO_NAME"
fi

echo "╔══════════════════════════════════════════════════════╗"
echo "║  Bulwark — Smart Contract Audit Pipeline           ║"
echo "║  Multi-pass · Multi-agent · PoC-gated              ║"
echo "╚══════════════════════════════════════════════════════╝"
echo ""
echo "→ Config:    $PROJECT_CONFIG"
echo "→ Target:    $AUDIT_TARGET ($AUDIT_BRANCH)"
echo "→ Audit dir: $AUDIT_DIR"
echo ""

# ── Inject host Claude credentials if mounted ─────────────────────────
if [ -f "/home/auditor/.claude-host-creds/credentials.json" ]; then
    cp /home/auditor/.claude-host-creds/credentials.json /home/auditor/.claude/.credentials.json
    echo "✓  Claude credentials injected from host."
fi

# ── Check Claude Code auth ────────────────────────────────────────────
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
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
    git clean -fd -- packages/horizon/test/pocs/ packages/horizon/test/bulwark/ \
        packages/horizon/test/formal/ packages/horizon/out/ 2>/dev/null || true
fi

cd "$AUDIT_DIR"

# ── Install dependencies ────────────────────────────────────────────
echo "→ Installing contract dependencies..."
if [ -f "package.json" ]; then
    pnpm install --frozen-lockfile 2>/dev/null || pnpm install
fi

# ── Stage context files ─────────────────────────────────────────────
# Priority: project context/ > built-in context/
# ATTACK_PATTERNS.md always comes from the install (accumulated generic knowledge).
echo "→ Staging audit context files..."

stage_context_file() {
    local filename="$1"
    local install_only="${2:-false}"
    local dest="$AUDIT_DIR/$filename"

    [ -f "$dest" ] && return  # already in place

    if [ "$install_only" = "false" ] && [ -f "/home/auditor/project/context/$filename" ]; then
        cp "/home/auditor/project/context/$filename" "$dest"
        echo "  ✓ $filename (from project)"
    elif [ -f "/home/auditor/context/$filename" ]; then
        cp "/home/auditor/context/$filename" "$dest"
        echo "  ✓ $filename (from install)"
    fi
}

stage_context_file "AUDIT_CONTEXT.md"
stage_context_file "PROPERTIES.md"
stage_context_file "KNOWN_ISSUES.md"
stage_context_file "ATTACK_PATTERNS.md" "true"  # always from install

# Create a minimal CLAUDE.md if not present
if [ ! -f "$AUDIT_DIR/CLAUDE.md" ]; then
    cat > "$AUDIT_DIR/CLAUDE.md" << 'CLAUSEMD'
# Audit Context

Read AUDIT_CONTEXT.md, PROPERTIES.md, and KNOWN_ISSUES.md before starting any audit work.
These files contain the protocol overview, invariants to verify, and accepted risks.
CLAUSEMD
fi

# ── Install Trail of Bits + forefy skills ────────────────────────────
echo "→ Installing AI audit skills..."
/home/auditor/scripts/install-skills.sh

# ── Patch missing remappings ─────────────────────────────────────────
patch_remapping() {
    local pkg_dir="$1"
    local remap_file="$pkg_dir/remappings.txt"
    [ ! -f "$remap_file" ] && return
    if ! grep -q "^horizon-test/" "$remap_file" 2>/dev/null; then
        local test_dir
        test_dir=$(find "$pkg_dir" -maxdepth 3 -type d -name "test" 2>/dev/null | head -1)
        if [ -n "$test_dir" ]; then
            local rel
            rel=$(realpath --relative-to="$pkg_dir" "$test_dir")
            echo "horizon-test/=${rel}/" >> "$remap_file"
        fi
    fi
}

# ── Compile contracts ────────────────────────────────────────────────
echo "→ Compiling contracts..."
if [ -d "packages/horizon" ]; then
    patch_remapping "packages/horizon"
    (cd packages/horizon && forge build 2>/dev/null) && echo "  ✓ Horizon compiled" || echo "  ✗ Horizon build failed"
fi
if [ -d "packages/subgraph-service" ]; then
    patch_remapping "packages/subgraph-service"
    (cd packages/subgraph-service && forge build 2>/dev/null) && echo "  ✓ SubgraphService compiled" || echo "  ✗ SubgraphService build failed"
fi

# ── Restore Claude Code settings ────────────────────────────────────
cp /home/auditor/.bulwark-settings.json /home/auditor/.claude/settings.json 2>/dev/null || true

# ── Print status (interactive mode only) ─────────────────────────────
cd "$AUDIT_DIR"

if [ $# -gt 0 ] && [ "$1" != "bash" ] && [ "$1" != "sh" ]; then
    exec "$@"
fi

echo ""
echo "════════════════════════════════════════════════════════════"
echo "  Ready. Working directory: $AUDIT_DIR"
echo ""
echo "  Available commands:"
echo "    bulwark run                      — Run full 6-pass pipeline"
echo "    bulwark run --pass 1             — Recon only (no AI)"
echo "    bulwark run --pass 2-3           — Agents + PoC Gate"
echo "    bulwark run --pass 2 --agent red — Single agent run"
echo "    bulwark status                   — Pipeline status"
echo "    bulwark findings                 — List findings"
echo "    bulwark report                   — Regenerate report"
echo "    bulwark doctor                   — Check tool availability"
echo "    bulwark login                    — Authenticate Claude"
echo "    bulwark init ./my-project        — Scaffold a new project"
echo "    claude                           — Claude Code (interactive)"
echo ""
echo "  Installed tools:"
command -v slither >/dev/null && echo "    ✓ Slither $(slither --version 2>&1 | head -1)" || echo "    ✗ Slither"
command -v forge >/dev/null && echo "    ✓ Foundry $(forge --version 2>&1 | head -1)" || echo "    ✗ Foundry"
command -v claude >/dev/null && echo "    ✓ Claude Code" || echo "    ✗ Claude Code"
echo "════════════════════════════════════════════════════════════"
echo ""

exec "$@"
