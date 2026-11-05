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
    AUDIT_COMMIT_FROM_CONFIG=$(grep '^commit = ' "$PROJECT_CONFIG" | head -1 | sed 's/.*commit = "\(.*\)"/\1/' || true)
    AUDIT_COMMIT="${AUDIT_COMMIT:-$AUDIT_COMMIT_FROM_CONFIG}"
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
echo "→ Target:    $AUDIT_TARGET ($AUDIT_BRANCH${AUDIT_COMMIT:+ @ $AUDIT_COMMIT})"
echo "→ Audit dir: $AUDIT_DIR"
echo ""

# ── Inject host Claude credentials if mounted ─────────────────────────
HOST_CREDS=/home/auditor/.claude-host-creds/credentials.json
if [ -f "$HOST_CREDS" ]; then
    cp "$HOST_CREDS" /home/auditor/.claude/.credentials.json
    echo "✓  Claude credentials injected from host."
    # A refresh rotates the token and voids the old one, so whichever side refreshed last wins.
    # Written in place: the host file is a bind mount and cannot be renamed over.
    (
        LOCAL=/home/auditor/.claude/.credentials.json
        while sleep 30; do
            if cmp -s "$LOCAL" "$HOST_CREDS"; then
                :
            elif [ "$LOCAL" -nt "$HOST_CREDS" ]; then
                cat "$LOCAL" > "$HOST_CREDS" 2>/dev/null
            elif [ "$HOST_CREDS" -nt "$LOCAL" ]; then
                cp "$HOST_CREDS" "$LOCAL"
            fi
        done
    ) &
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
# A bench run must be blind: a fresh clone every time, since the snapshot below drops the remote.
if [ "${BULWARK_BENCH:-}" = "1" ] && [ -d "$AUDIT_DIR/.git" ] && [ ! -d "$AUDIT_DIR/audit-workspace" ]; then
    rm -rf "$AUDIT_DIR"
fi
if [ "${BULWARK_BENCH:-}" = "1" ] && [ -d "$AUDIT_DIR/.git" ]; then
    echo "→ Bench snapshot already in place (resuming)"
elif [ ! -d "$AUDIT_DIR/.git" ]; then
    echo "→ Cloning $AUDIT_TARGET ($AUDIT_BRANCH)..."
    git clone --branch "$AUDIT_BRANCH" --depth 50 "$AUDIT_TARGET" "$AUDIT_DIR"
else
    echo "→ Updating existing clone..."
    cd "$AUDIT_DIR" && git fetch origin && git reset --hard "origin/$AUDIT_BRANCH"
    # Clean bulwark-generated test artefacts so stale PoCs don't interfere with a fresh run
    git clean -fd -- "**/pocs/*.t.sol" "**/bulwark/*.sol" "**/bulwark-formal/" 2>/dev/null || true
fi

cd "$AUDIT_DIR"

if [ -n "${AUDIT_COMMIT:-}" ] && ! git cat-file -e "refs/heads/bench-snapshot" 2>/dev/null; then
    # A pin may sit deeper than the shallow clone reaches.
    git cat-file -e "${AUDIT_COMMIT}^{commit}" 2>/dev/null || git fetch --unshallow origin 2>/dev/null || git fetch origin
    git -c advice.detachedHead=false checkout --force "$AUDIT_COMMIT"
fi

if [ -f ".gitmodules" ] && ! git cat-file -e "refs/heads/bench-snapshot" 2>/dev/null; then
    echo "→ Initialising submodules..."
    git submodule update --init --recursive
fi

# Later commits on a contest repo can carry the fixes. Keep only the audited tree.
if [ "${BULWARK_BENCH:-}" = "1" ] && ! git cat-file -e "refs/heads/bench-snapshot" 2>/dev/null; then
    echo "→ Bench: collapsing history to a single snapshot commit"
    git checkout -q --orphan bench-snapshot
    git add -A
    git -c user.name=bench -c user.email=bench@localhost commit -qm snapshot --no-verify
    git for-each-ref --format='%(refname)' | grep -v '^refs/heads/bench-snapshot$' \
        | xargs -r -n1 git update-ref -d
    git remote | xargs -r -n1 git remote remove
    git reflog expire --expire=now --all
    git gc -q --prune=now
fi

# ── Install dependencies ────────────────────────────────────────────
echo "→ Installing contract dependencies..."
if [ -f "package.json" ]; then
    # Disable interactive git prompts so SSH/HTTPS git deps fail fast rather than blocking.
    # Linting plugins (solhint etc.) are irrelevant for auditing — failure here is non-fatal.
    GIT_TERMINAL_PROMPT=0 pnpm install --prod 2>/dev/null \
        || GIT_TERMINAL_PROMPT=0 pnpm install --prod --ignore-scripts 2>/dev/null \
        || echo "  ⚠ pnpm install failed (non-fatal — forge/vyper compilation proceeds without node_modules)"
fi

# ── Stage context files ─────────────────────────────────────────────
# Priority: project context/ > built-in context/
# ATTACK_PATTERNS.md always comes from the install (accumulated generic knowledge).
echo "→ Staging audit context files..."

stage_context_file() {
    local filename="$1"
    local install_only="${2:-false}"
    local dest="$AUDIT_DIR/$filename"

    # Project context always overwrites — stale files from previous runs on different
    # targets must not persist. ATTACK_PATTERNS.md is install-only (generic knowledge).
    if [ "$install_only" = "false" ] && [ -f "/home/auditor/project/context/$filename" ]; then
        cp "/home/auditor/project/context/$filename" "$dest"
        echo "  ✓ $filename (from project)"
    elif [ ! -f "$dest" ] && [ -f "/home/auditor/context/$filename" ] \
        && { [ "$install_only" = "true" ] || [ "$PROJECT_CONFIG" = "/home/auditor/bulwark.toml" ]; }; then
        # The install's protocol context describes The Graph: a default for the built-in
        # config only. Any other target without its own gets one derived in Pass 1.
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

# ── Compile contracts ────────────────────────────────────────────────
# Try forge build from root; if the repo is a monorepo, try each package subdir.
echo "→ Compiling contracts..."
# Vyper 0.3.x cannot target 'prague', the newer Foundry default, so fall back to cancun. The env
# var beats foundry.toml, so only set it when the target has not pinned its own evm_version.
evm_env() {
    grep -qs '^\s*evm_version' "${1:-.}/foundry.toml" && return
    echo "FOUNDRY_EVM_VERSION=cancun"
}
if [ -f "foundry.toml" ] || [ -f "forge.config.toml" ]; then
    if build_out=$(env $(evm_env .) forge build 2>&1); then
        echo "  ✓ Compiled"
    else
        echo "$build_out" | tail -20
        echo "  ✗ Root build failed (Pass 1 will retry per-package)"
    fi
elif [ -d "packages" ]; then
    for pkg in packages/*/; do
        [ -f "${pkg}foundry.toml" ] || continue
        pkg_name=$(basename "$pkg")
        (cd "$pkg" && env $(evm_env .) forge build 2>/dev/null) \
            && echo "  ✓ $pkg_name compiled" \
            || echo "  ✗ $pkg_name build failed (Pass 1 will retry)"
    done
fi

# ── Fork proxy ──────────────────────────────────────────────────────
# forge test cannot send RPC headers, so an authenticated upstream is fronted by a local anvil
# pinned at the config's fork_block. PoCs fork the proxy; the token never reaches them.
FORK_BLOCK=$(grep -E '^fork_block *=' "$PROJECT_CONFIG" 2>/dev/null | head -1 | sed 's/[^0-9]//g' || true)
if [ -n "${BULWARK_FORK_UPSTREAM:-}" ] && [ -n "$FORK_BLOCK" ]; then
    echo "→ Starting fork proxy at block $FORK_BLOCK..."
    auth=()
    [ -n "${BULWARK_FORK_UPSTREAM_TOKEN:-}" ] && auth=(--fork-header "Authorization: Bearer $BULWARK_FORK_UPSTREAM_TOKEN")
    anvil --fork-url "$BULWARK_FORK_UPSTREAM" "${auth[@]}" --fork-block-number "$FORK_BLOCK" \
        --host 127.0.0.1 --port 8545 --silent > /tmp/fork-proxy.log 2>&1 &
    unset BULWARK_FORK_UPSTREAM_TOKEN
    for _ in $(seq 1 30); do
        cast block-number --rpc-url http://127.0.0.1:8545 >/dev/null 2>&1 && break
        sleep 1
    done
    if cast block-number --rpc-url http://127.0.0.1:8545 >/dev/null 2>&1; then
        export BULWARK_FORK_RPC=http://127.0.0.1:8545
        echo "  ✓ Fork proxy ready"
    else
        echo "  ✗ Fork proxy did not start; PoCs run in local mode"
        tail -5 /tmp/fork-proxy.log
    fi
fi

# ── Restore Claude Code settings ────────────────────────────────────
cp /home/auditor/.bulwark-settings.json /home/auditor/.claude/settings.json 2>/dev/null || true
if [ "${BULWARK_BENCH:-}" = "1" ]; then
    # The contest's findings are public; an agent that can search the web can look them up.
    python3 - <<'PY'
import json
p = "/home/auditor/.claude/settings.json"
s = json.load(open(p))
s.setdefault("permissions", {}).setdefault("deny", []).extend([
    "WebFetch", "WebSearch",
    "Bash(curl *)", "Bash(wget *)", "Bash(gh *)", "Bash(git clone *)", "Bash(git fetch *)",
])
json.dump(s, open(p, "w"), indent=2)
PY
    echo "→ Bench: web and network commands denied"
fi

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
