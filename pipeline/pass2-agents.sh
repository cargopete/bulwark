#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
# DOYRAN v2 — Pass 2: Multi-Agent Adversarial Analysis
# ════════════════════════════════════════════════════════════════════════
# Three independent Claude Code sessions run in parallel:
#   RED agent  — attacker persona, exploit-focused
#   BLUE agent — systematic, property-by-property verification
#   GOLD agent — economic/mathematical analysis
#
# Agents cannot see each other's output.
# After all three complete, findings are merged and deduplicated.
# ════════════════════════════════════════════════════════════════════════

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib/common.sh"

PROMPTS_DIR="$DOYRAN_ROOT/prompts"
FINDINGS_DIR="$WORKSPACE/findings"
LOGS_DIR="$WORKSPACE/findings/logs"
MAX_TURNS="${DOYRAN_MAX_TURNS:-80}"

mkdir -p "$FINDINGS_DIR" "$LOGS_DIR"

timer_start
log_step "Pass 2: Multi-Agent Adversarial Analysis"

# ── Preflight checks ──────────────────────────────────────────────────

# Verify Pass 1 outputs exist
if [ ! -f "$WORKSPACE/recon/recon-summary.json" ]; then
    log_error "Pass 1 recon outputs not found. Run Pass 1 first."
    exit 1
fi

# Verify prompts exist
for agent in red blue gold; do
    if [ ! -f "$PROMPTS_DIR/${agent}-agent.md" ]; then
        log_error "Prompt not found: $PROMPTS_DIR/${agent}-agent.md"
        exit 1
    fi
done

# Verify Claude is available
if ! command -v claude >/dev/null 2>&1; then
    log_error "Claude Code not found. Install or check PATH."
    exit 1
fi

log_info "Max turns per agent: $MAX_TURNS"
log_info "Working directory: $AUDIT_DIR"

# ── Launch agents in parallel ─────────────────────────────────────────

launch_agent() {
    local agent_name="$1"
    local prompt_file="$2"
    local output_file="$3"
    local log_file="$4"

    log_info "Launching $agent_name agent..."

    # Run Claude in headless mode with the agent prompt
    # --max-turns prevents runaway sessions
    # Agents work from the audit directory so they can read source code
    cd "$AUDIT_DIR"

    claude -p "$(cat "$prompt_file")" \
        --max-turns "$MAX_TURNS" \
        --verbose \
        > "$log_file" 2>&1

    local exit_code=$?

    if [ $exit_code -ne 0 ]; then
        log_warn "$agent_name agent exited with code $exit_code"
    fi

    # Verify output was written by the agent
    if [ -f "$output_file" ]; then
        if jq empty "$output_file" 2>/dev/null; then
            local count
            count=$(jq 'if type == "array" then length else 0 end' "$output_file" 2>/dev/null || echo "0")
            log_ok "$agent_name agent complete: $count findings"
        else
            log_warn "$agent_name agent wrote invalid JSON — attempting extraction from log"
            extract_json_from_log "$log_file" "$output_file"
        fi
    else
        log_warn "$agent_name agent did not write output file — attempting extraction from log"
        extract_json_from_log "$log_file" "$output_file"
    fi
}

# Fallback: extract JSON array from Claude's text output if the agent
# didn't write to the expected file path
extract_json_from_log() {
    local log_file="$1"
    local output_file="$2"

    # Try to find a JSON array in the log output
    # Look for the largest [...] block
    if [ -f "$log_file" ]; then
        python3 -c "
import json, re, sys

with open('$log_file', 'r') as f:
    content = f.read()

# Find all JSON array candidates
arrays = re.findall(r'\[[\s\S]*?\]', content)

best = []
for candidate in arrays:
    try:
        parsed = json.loads(candidate)
        if isinstance(parsed, list) and len(parsed) > len(best):
            # Check if items look like findings
            if len(parsed) > 0 and isinstance(parsed[0], dict) and 'severity' in parsed[0]:
                best = parsed
    except (json.JSONDecodeError, ValueError):
        continue

if best:
    with open('$output_file', 'w') as f:
        json.dump(best, f, indent=2)
    print(f'Extracted {len(best)} findings from log')
else:
    with open('$output_file', 'w') as f:
        json.dump([], f)
    print('No findings extracted from log')
" 2>/dev/null || {
            log_warn "JSON extraction failed — writing empty array"
            echo '[]' > "$output_file"
        }
    else
        echo '[]' > "$output_file"
    fi
}

# Launch all three agents in parallel
log_info "Launching RED, BLUE, GOLD agents in parallel..."
echo ""

RED_OUTPUT="$FINDINGS_DIR/red-agent-raw.json"
BLUE_OUTPUT="$FINDINGS_DIR/blue-agent-raw.json"
GOLD_OUTPUT="$FINDINGS_DIR/gold-agent-raw.json"

launch_agent "RED" "$PROMPTS_DIR/red-agent.md" "$RED_OUTPUT" "$LOGS_DIR/red-agent.log" &
RED_PID=$!

launch_agent "BLUE" "$PROMPTS_DIR/blue-agent.md" "$BLUE_OUTPUT" "$LOGS_DIR/blue-agent.log" &
BLUE_PID=$!

launch_agent "GOLD" "$PROMPTS_DIR/gold-agent.md" "$GOLD_OUTPUT" "$LOGS_DIR/gold-agent.log" &
GOLD_PID=$!

log_info "Agent PIDs — RED: $RED_PID | BLUE: $BLUE_PID | GOLD: $GOLD_PID"
log_info "Waiting for all agents to complete (this takes 15-45 minutes)..."
echo ""

# ── Wait for all agents ───────────────────────────────────────────────

FAILED=0

wait $RED_PID || { log_warn "RED agent process failed"; FAILED=$((FAILED + 1)); }
log_info "RED agent finished ($(timer_human) elapsed)"

wait $BLUE_PID || { log_warn "BLUE agent process failed"; FAILED=$((FAILED + 1)); }
log_info "BLUE agent finished ($(timer_human) elapsed)"

wait $GOLD_PID || { log_warn "GOLD agent process failed"; FAILED=$((FAILED + 1)); }
log_info "GOLD agent finished ($(timer_human) elapsed)"

echo ""

if [ $FAILED -eq 3 ]; then
    log_error "All three agents failed. Check logs in $LOGS_DIR/"
    exit 1
fi

# ── Ensure valid JSON outputs ─────────────────────────────────────────

for output in "$RED_OUTPUT" "$BLUE_OUTPUT" "$GOLD_OUTPUT"; do
    if [ ! -f "$output" ]; then
        echo '[]' > "$output"
    elif ! jq empty "$output" 2>/dev/null; then
        log_warn "Invalid JSON in $(basename "$output") — resetting to empty"
        echo '[]' > "$output"
    fi
done

# ── Extract findings from BLUE agent property verification ────────────
# BLUE produces mixed output (property verifications + findings).
# Separate them: keep only items with "severity" for the merge,
# save property verifications separately.

if [ -f "$BLUE_OUTPUT" ]; then
    log_info "Processing BLUE agent output..."

    # Extract property verification results
    jq '[.[] | select(.type == "property_verification")]' \
        "$BLUE_OUTPUT" > "$FINDINGS_DIR/property-verifications.json" 2>/dev/null \
        || echo '[]' > "$FINDINGS_DIR/property-verifications.json"

    VERIFIED=$(jq '[.[] | select(.status == "VERIFIED")] | length' \
        "$FINDINGS_DIR/property-verifications.json" 2>/dev/null || echo "0")
    VIOLATED=$(jq '[.[] | select(.status == "VIOLATED")] | length' \
        "$FINDINGS_DIR/property-verifications.json" 2>/dev/null || echo "0")
    UNCERTAIN=$(jq '[.[] | select(.status == "UNCERTAIN")] | length' \
        "$FINDINGS_DIR/property-verifications.json" 2>/dev/null || echo "0")

    log_info "  Property status: $VERIFIED verified, $VIOLATED violated, $UNCERTAIN uncertain"

    # Extract only findings (items with severity) for the merge
    BLUE_FINDINGS="$FINDINGS_DIR/blue-agent-findings-only.json"
    jq '[.[] | select(.severity != null)]' \
        "$BLUE_OUTPUT" > "$BLUE_FINDINGS" 2>/dev/null \
        || echo '[]' > "$BLUE_FINDINGS"
else
    BLUE_FINDINGS="$BLUE_OUTPUT"
fi

# ── Merge and deduplicate ────────────────────────────────────────────

log_info "Merging and deduplicating findings..."
echo ""

MERGED_OUTPUT="$FINDINGS_DIR/merged-deduplicated.json"

"$SCRIPT_DIR/lib/merge-findings.sh" \
    "$MERGED_OUTPUT" \
    "$RED_OUTPUT" \
    "$BLUE_FINDINGS" \
    "$GOLD_OUTPUT"

# ── Summary ──────────────────────────────────────────────────────────

DURATION=$(timer_elapsed)
record_pass_status "agents" "completed" "$DURATION"

echo ""
log_step "Pass 2 Complete ($(timer_human))"
echo ""

RED_COUNT=$(jq 'if type == "array" then length else 0 end' "$RED_OUTPUT" 2>/dev/null || echo "0")
BLUE_COUNT=$(jq 'if type == "array" then [.[] | select(.severity != null)] | length else 0 end' "$BLUE_OUTPUT" 2>/dev/null || echo "0")
GOLD_COUNT=$(jq 'if type == "array" then length else 0 end' "$GOLD_OUTPUT" 2>/dev/null || echo "0")
MERGED_COUNT=$(jq 'if type == "array" then length else 0 end' "$MERGED_OUTPUT" 2>/dev/null || echo "0")

log_ok "RED agent:   $RED_COUNT findings"
log_ok "BLUE agent:  $BLUE_COUNT findings ($VERIFIED verified, $VIOLATED violated, $UNCERTAIN uncertain)"
log_ok "GOLD agent:  $GOLD_COUNT findings"
echo ""
log_ok "After merge: $MERGED_COUNT unique findings"
echo ""

# Severity breakdown
if [ "$MERGED_COUNT" -gt 0 ]; then
    log_info "Severity breakdown:"
    jq -r 'group_by(.severity) | .[] | "  \(.[0].severity): \(length)"' \
        "$MERGED_OUTPUT" 2>/dev/null || true
fi

echo ""
log_info "Agent logs: $LOGS_DIR/"
log_info "Property verifications: $FINDINGS_DIR/property-verifications.json"
log_info "Merged findings: $MERGED_OUTPUT"
echo ""
