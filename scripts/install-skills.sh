#!/usr/bin/env bash
set -euo pipefail

# Install Trail of Bits and forefy audit skills for Claude Code.
# These are prompt templates and configuration — not binary dependencies.
# If a repo doesn't exist or has changed structure, we skip gracefully.

TOOLS_DIR="/home/auditor/tools"
CLAUDE_DIR="/home/auditor/.claude"
COMMANDS_DIR="$CLAUDE_DIR/commands"

mkdir -p "$TOOLS_DIR" "$COMMANDS_DIR"

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

# ── Trail of Bits: skills repo ───────────────────────────────────────
echo "  → Trail of Bits skills..."
TOB_SKILLS="$TOOLS_DIR/tob-skills"
if [ ! -d "$TOB_SKILLS" ]; then
    git clone --depth 1 https://github.com/trailofbits/skills.git "$TOB_SKILLS" 2>/dev/null || {
        echo "    ✗ Could not clone trailofbits/skills — skipping"
        TOB_SKILLS=""
    }
fi

if [ -n "$TOB_SKILLS" ] && [ -d "$TOB_SKILLS" ]; then
    # Look for plugin/skill definitions and copy as slash commands
    SKILL_COUNT=0
    for skill_dir in "$TOB_SKILLS"/plugins/*/; do
        if [ -d "$skill_dir" ]; then
            skill_name=$(basename "$skill_dir")
            # Look for the main prompt/instruction file
            for candidate in "$skill_dir/SKILL.md" "$skill_dir/PROMPT.md" "$skill_dir/README.md" "$skill_dir/prompt.md" "$skill_dir/skill.md" "$skill_dir/index.md"; do
                if [ -f "$candidate" ]; then
                    cp "$candidate" "$COMMANDS_DIR/tob-${skill_name}.md"
                    SKILL_COUNT=$((SKILL_COUNT + 1))
                    break
                fi
            done
        fi
    done
    echo "    ✓ $SKILL_COUNT Trail of Bits skills installed"
else
    echo "    ✗ Trail of Bits skills unavailable"
fi

# ── Trail of Bits: skills-curated (contains scv-scan) ────────────────
echo "  → Trail of Bits skills-curated..."
TOB_CURATED="$TOOLS_DIR/tob-skills-curated"
if [ ! -d "$TOB_CURATED" ]; then
    git clone --depth 1 https://github.com/trailofbits/skills-curated.git "$TOB_CURATED" 2>/dev/null || {
        echo "    ✗ Could not clone trailofbits/skills-curated — skipping"
        TOB_CURATED=""
    }
fi

if [ -n "$TOB_CURATED" ] && [ -d "$TOB_CURATED" ]; then
    CURATED_COUNT=0
    for skill_dir in "$TOB_CURATED"/plugins/*/; do
        if [ -d "$skill_dir" ]; then
            skill_name=$(basename "$skill_dir")
            for candidate in "$skill_dir/SKILL.md" "$skill_dir/PROMPT.md" "$skill_dir/README.md" "$skill_dir/prompt.md" "$skill_dir/skill.md" "$skill_dir/index.md"; do
                if [ -f "$candidate" ]; then
                    cp "$candidate" "$COMMANDS_DIR/tob-${skill_name}.md"
                    CURATED_COUNT=$((CURATED_COUNT + 1))
                    break
                fi
            done
        fi
    done
    echo "    ✓ $CURATED_COUNT curated skills installed (includes scv-scan)"
else
    echo "    ✗ Trail of Bits curated skills unavailable"
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
