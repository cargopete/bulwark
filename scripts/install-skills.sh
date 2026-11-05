#!/usr/bin/env bash
set -euo pipefail

# Install Trail of Bits and forefy audit skills for Claude Code.
# These are prompt templates and configuration — not binary dependencies.
# If a repo doesn't exist or has changed structure, we skip gracefully.

TOOLS_DIR="/home/auditor/tools"
CLAUDE_DIR="/home/auditor/.claude"
COMMANDS_DIR="$CLAUDE_DIR/commands"

mkdir -p "$TOOLS_DIR" "$COMMANDS_DIR" "$CLAUDE_DIR/skills"

# ── Trail of Bits: claude-code-config ────────────────────────────────
echo "  → Trail of Bits claude-code-config..."
TOB_CONFIG="$TOOLS_DIR/claude-code-config"
if [ ! -d "$TOB_CONFIG" ]; then
    git clone --depth 1 https://github.com/trailofbits/claude-code-config.git "$TOB_CONFIG" 2>/dev/null || {
        echo "    ✗ Could not clone trailofbits/claude-code-config — skipping"
        TOB_CONFIG=""
    }
fi

if [ -n "$TOB_CONFIG" ] && [ -d "$TOB_CONFIG" ]; then
    # Copy slash commands if they exist
    if [ -d "$TOB_CONFIG/commands" ]; then
        cp "$TOB_CONFIG/commands/"*.md "$COMMANDS_DIR/" 2>/dev/null && \
            echo "    ✓ Slash commands installed" || echo "    ✗ No slash commands found"
    fi
    # Copy statusline script if present
    if [ -f "$TOB_CONFIG/scripts/statusline.sh" ]; then
        cp "$TOB_CONFIG/scripts/statusline.sh" "$CLAUDE_DIR/statusline.sh"
        chmod +x "$CLAUDE_DIR/statusline.sh"
        echo "    ✓ Statusline script installed"
    fi
    echo "    ✓ Trail of Bits config ready"
else
    echo "    ✗ Trail of Bits config unavailable"
fi

# Trail of Bits plugins keep each skill at plugins/<plugin>/skills/<skill>/SKILL.md, beside the
# references it reads. Install the whole directory as `tob-<skill>`, so `/tob-<skill>` resolves.
install_tob_skills() {
    local repo="$1" count=0 skill_dir skill_name dest
    for skill_dir in "$repo"/plugins/*/skills/*/; do
        [ -f "$skill_dir/SKILL.md" ] || continue
        skill_name="tob-$(basename "$skill_dir")"
        dest="$CLAUDE_DIR/skills/$skill_name"
        rm -rf "$dest" "$COMMANDS_DIR/$skill_name.md"
        mkdir -p "$dest"
        cp -r "$skill_dir". "$dest/"
        sed -i "1,/^---$/!b; 0,/^name:.*/s//name: $skill_name/" "$dest/SKILL.md"
        count=$((count + 1))
    done
    echo "$count"
}

clone_once() {
    [ -d "$2" ] || git clone --depth 1 "$1" "$2" 2>/dev/null
}

echo "  → Trail of Bits skills..."
if clone_once https://github.com/trailofbits/skills.git "$TOOLS_DIR/tob-skills"; then
    echo "    ✓ $(install_tob_skills "$TOOLS_DIR/tob-skills") Trail of Bits skills installed"
else
    echo "    ✗ Could not clone trailofbits/skills — skipping"
fi

echo "  → Trail of Bits skills-curated..."
if clone_once https://github.com/trailofbits/skills-curated.git "$TOOLS_DIR/tob-skills-curated"; then
    echo "    ✓ $(install_tob_skills "$TOOLS_DIR/tob-skills-curated") curated skills installed (includes scv-scan)"
else
    echo "    ✗ Could not clone trailofbits/skills-curated — skipping"
fi

# ── forefy/.context ──────────────────────────────────────────────────
echo "  → forefy/.context audit skills..."
FOREFY_DIR="$TOOLS_DIR/forefy-context"
if [ ! -d "$FOREFY_DIR" ]; then
    git clone --depth 1 https://github.com/forefy/.context.git "$FOREFY_DIR" 2>/dev/null || {
        echo "    ✗ Could not clone forefy/.context — skipping"
        FOREFY_DIR=""
    }
fi

if [ -n "$FOREFY_DIR" ] && [ -d "$FOREFY_DIR" ]; then
    # forefy skills live in skills/ subdirectory
    FOREFY_COUNT=0
    SKILLS_SRC="$FOREFY_DIR/skills"
    if [ -d "$SKILLS_SRC" ]; then
        # Skills to skip auto-loading (opt-in only, too noisy on startup)
        SKIP_SKILLS="agent-onboarding auditor-quiz"

        for skill_dir in "$SKILLS_SRC"/*/; do
            if [ -d "$skill_dir" ]; then
                skill_name=$(basename "$skill_dir")
                # Skip skills that auto-trigger and disrupt first-time experience
                if echo "$SKIP_SKILLS" | grep -qw "$skill_name"; then
                    mkdir -p "$CLAUDE_DIR/skills-optional/$skill_name"
                    cp -r "$skill_dir"* "$CLAUDE_DIR/skills-optional/$skill_name/" 2>/dev/null
                    continue
                fi
                mkdir -p "$CLAUDE_DIR/skills/$skill_name"
                cp -r "$skill_dir"* "$CLAUDE_DIR/skills/$skill_name/" 2>/dev/null
                FOREFY_COUNT=$((FOREFY_COUNT + 1))
            fi
        done
    fi
    # Also check for reference data (vulnerability patterns from 10,600+ findings)
    if [ -d "$FOREFY_DIR/reference" ]; then
        mkdir -p "$CLAUDE_DIR/skills/reference"
        cp -r "$FOREFY_DIR/reference/"* "$CLAUDE_DIR/skills/reference/" 2>/dev/null
        echo "    ✓ Reference vulnerability patterns installed"
    fi
    echo "    ✓ $FOREFY_COUNT forefy skills installed"
else
    echo "    ✗ forefy/.context unavailable"
fi

echo "  → Skill installation complete."
