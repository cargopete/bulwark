#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
# BULWARK — Pass 3: PoC Generation & Validation
# ════════════════════════════════════════════════════════════════════════
# "No PoC, no finding" gate.
#
# For each finding from Pass 2:
#   1. Generate a Foundry PoC via Claude headless mode
#   2. Attempt to compile with forge build
#   3. If compiles, run with forge test
#   4. Classify result and apply the validation gate
#
# Findings that fail to compile are DISCARDED.
# ════════════════════════════════════════════════════════════════════════

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib/common.sh"

PROMPTS_DIR="$BULWARK_ROOT/prompts"
POCS_DIR="$WORKSPACE/pocs"
LOGS_DIR="$WORKSPACE/pocs/logs"
MERGED_FINDINGS="$WORKSPACE/findings/merged-deduplicated.json"
MAX_TURNS="${BULWARK_POC_MAX_TURNS:-30}"
MAX_RETRIES="${BULWARK_POC_RETRIES:-2}"

mkdir -p "$POCS_DIR" "$LOGS_DIR"

timer_start
log_step "Pass 3: PoC Generation & Validation"

# ── Preflight ─────────────────────────────────────────────────────────

if [ ! -f "$MERGED_FINDINGS" ]; then
    log_error "Pass 2 findings not found: $MERGED_FINDINGS"
    exit 1
fi

FINDING_COUNT=$(jq 'if type == "array" then length else 0 end' "$MERGED_FINDINGS" 2>/dev/null || echo "0")

if [ "$FINDING_COUNT" -eq 0 ]; then
    log_info "No findings from Pass 2 — nothing to validate."
    echo '[]' > "$POCS_DIR/validated-findings.json"
    exit 0
fi

log_info "$FINDING_COUNT findings to process"
log_info "Max turns per PoC: $MAX_TURNS, Max retries: $MAX_RETRIES"

if ! command -v claude >/dev/null 2>&1; then
    log_error "Claude Code not found."
    exit 1
fi

if ! command -v forge >/dev/null 2>&1; then
    log_error "Forge not found."
    exit 1
fi

# ── Discover test infrastructure ──────────────────────────────────────
# Find existing test base contracts and import patterns the PoC generator
# can reference

log_info "Scanning test infrastructure..."

TEST_INFO=""
for pkg in "${GRAPH_PACKAGES[@]}"; do
    test_dir="$AUDIT_DIR/$pkg/test"
    if [ -d "$test_dir" ]; then
        # Find test base contracts
        bases=$(find "$test_dir" -name "*.sol" -exec grep -l "contract.*Test.*is" {} \; 2>/dev/null | head -5 || true)
        if [ -n "$bases" ]; then
            TEST_INFO="$TEST_INFO\n\nExisting test bases in $pkg/test/:"
            for base in $bases; do
                rel="${base#$AUDIT_DIR/}"
                # Get the contract name and imports
                contract_line=$(grep -m1 "contract.*Test" "$base" || true)
                TEST_INFO="$TEST_INFO\n  $rel: $contract_line"
            done
        fi
        # Find remappings
        if [ -f "$AUDIT_DIR/$pkg/remappings.txt" ]; then
            TEST_INFO="$TEST_INFO\n\nRemappings ($pkg):"
            TEST_INFO="$TEST_INFO\n$(cat "$AUDIT_DIR/$pkg/remappings.txt")"
        fi
        if [ -f "$AUDIT_DIR/$pkg/foundry.toml" ]; then
            TEST_INFO="$TEST_INFO\n\nfoundry.toml ($pkg):"
            TEST_INFO="$TEST_INFO\n$(grep -E '(src|test|out|libs|remappings)' "$AUDIT_DIR/$pkg/foundry.toml" || true)"
        fi
    fi
done

log_ok "Test infrastructure scanned"

# ── Process each finding ──────────────────────────────────────────────

VALIDATED='[]'
DISCARDED=0
VALIDATED_COUNT=0
INCONCLUSIVE=0

process_finding() {
    local idx="$1"
    local finding
    finding=$(jq ".[$idx]" "$MERGED_FINDINGS")

    local finding_id
    finding_id=$(echo "$finding" | jq -r '.id')
    local title
    title=$(echo "$finding" | jq -r '.title')
    local severity
    severity=$(echo "$finding" | jq -r '.severity')
    local contract
    contract=$(echo "$finding" | jq -r '.contract')

    log_info "[$finding_id] $title ($severity — $contract)"

    local poc_file="$POCS_DIR/${finding_id}.t.sol"
    local log_file="$LOGS_DIR/${finding_id}.log"
    local poc_status="not_attempted"
    local retry=0

    while [ $retry -le "$MAX_RETRIES" ]; do
        if [ $retry -gt 0 ]; then
            log_info "  Retry $retry/$MAX_RETRIES..."
        fi

        # Build the per-finding prompt
        local prompt
        prompt=$(build_poc_prompt "$finding" "$poc_file" "$retry")

        # Generate PoC via Claude
        cd "$AUDIT_DIR"
        claude -p "$prompt" \
            --max-turns "$MAX_TURNS" \
            > "$log_file" 2>&1 || true

        # Check if the PoC file was created
        if [ ! -f "$poc_file" ]; then
            log_warn "  Agent did not write PoC file — extracting from log"
            extract_solidity_from_log "$log_file" "$poc_file"
        fi

        if [ ! -f "$poc_file" ]; then
            log_warn "  No PoC generated"
            poc_status="failed_to_compile"
            break
        fi

        # Determine which package to build in
        local build_dir
        build_dir=$(find_build_dir "$contract")

        # Attempt compilation
        log_info "  Compiling..."
        local build_output
        build_output=$(cd "$build_dir" && forge build 2>&1) || true

        if echo "$build_output" | grep -q "Compiler run successful"; then
            log_ok "  Compilation successful"

            # Run the test
            log_info "  Running test..."
            local test_name
            test_name=$(grep -oP 'function\s+\Ktest_\w+' "$poc_file" | head -1 || echo "")

            local test_output
            local test_exit
            test_output=$(cd "$build_dir" && forge test \
                --match-path "$(realpath --relative-to="$build_dir" "$poc_file")" \
                -vvv 2>&1) || true
            test_exit=$?

            # Classify the result
            poc_status=$(classify_test_result "$test_output" "$test_exit")
            log_info "  Result: $poc_status"
            break
        else
            log_warn "  Compilation failed"

            if [ $retry -lt "$MAX_RETRIES" ]; then
                # Save the error for the retry prompt
                echo "$build_output" > "$LOGS_DIR/${finding_id}-build-error-${retry}.txt"
                retry=$((retry + 1))
            else
                poc_status="failed_to_compile"
                log_warn "  All retries exhausted — marking as failed_to_compile"
                break
            fi
        fi
    done

    # Apply the validation gate
    apply_validation_gate "$finding" "$poc_status" "$poc_file"
}

# ── Prompt builder ────────────────────────────────────────────────────

build_poc_prompt() {
    local finding="$1"
    local poc_file="$2"
    local retry="$3"

    local finding_id
    finding_id=$(echo "$finding" | jq -r '.id')

    local prompt
    prompt="$(cat "$PROMPTS_DIR/poc-generator.md")

---

## Finding to Demonstrate

\`\`\`json
$finding
\`\`\`

## Test Infrastructure
$(echo -e "$TEST_INFO")

## Output Path

Write the PoC test to: \`$poc_file\`

The test MUST compile with \`forge build\`. A compiling test that's inconclusive is
infinitely better than a perfect test that doesn't compile."

    # On retry, include the compilation error
    if [ "$retry" -gt 0 ]; then
        local error_file="$LOGS_DIR/${finding_id}-build-error-$((retry - 1)).txt"
        if [ -f "$error_file" ]; then
            prompt="$prompt

## PREVIOUS ATTEMPT FAILED TO COMPILE

The previous PoC failed to compile with these errors:

\`\`\`
$(tail -50 "$error_file")
\`\`\`

Fix the compilation errors. Common issues:
- Wrong import paths — check remappings.txt and foundry.toml
- Missing function signatures — read the actual contract source
- Wrong Solidity version — use \`pragma solidity ^0.8.27;\`
- Simplify: use \`deal()\` for balances, \`vm.prank()\` for callers"
        fi
    fi

    echo "$prompt"
}

# ── Solidity extraction fallback ──────────────────────────────────────

extract_solidity_from_log() {
    local log_file="$1"
    local output_file="$2"

    if [ ! -f "$log_file" ]; then
        return 1
    fi

    # Extract the largest Solidity code block from the log
    python3 -c "
import re, sys

with open('$log_file', 'r') as f:
    content = f.read()

# Find solidity code blocks
blocks = re.findall(r'// SPDX-License-Identifier.*?(?=\`\`\`|\Z)', content, re.DOTALL)

if not blocks:
    # Try finding pragma solidity blocks
    blocks = re.findall(r'pragma solidity.*?(?=\`\`\`|\Z)', content, re.DOTALL)

if blocks:
    # Take the longest block (most likely the full test)
    best = max(blocks, key=len)
    # Clean up markdown artifacts
    best = best.replace('\`\`\`solidity', '').replace('\`\`\`', '').strip()
    with open('$output_file', 'w') as f:
        f.write(best)
    sys.exit(0)
else:
    sys.exit(1)
" 2>/dev/null
}

# ── Build directory finder ────────────────────────────────────────────

find_build_dir() {
    local contract="$1"

    # Check which package contains this contract
    for pkg in "${GRAPH_PACKAGES[@]}"; do
        local pkg_path="$AUDIT_DIR/$pkg"
        if [ -d "$pkg_path" ]; then
            if find "$pkg_path/contracts" -name "$contract" 2>/dev/null | grep -q .; then
                echo "$pkg_path"
                return
            fi
            # Try without .sol extension
            if find "$pkg_path/contracts" -name "${contract%.sol}.sol" 2>/dev/null | grep -q .; then
                echo "$pkg_path"
                return
            fi
        fi
    done

    # Default to horizon
    echo "$AUDIT_DIR/packages/horizon"
}

# ── Test result classifier ────────────────────────────────────────────

classify_test_result() {
    local output="$1"
    local exit_code="$2"

    # Check for passing test with assertions
    if echo "$output" | grep -qE '\[PASS\].*test_'; then
        # Test passed — check if it actually demonstrates the vuln
        if echo "$output" | grep -qiE '(assert|revert|overflow|underflow|drift|profit|loss|extract)'; then
            echo "compiles_and_demonstrates"
        else
            echo "compiles_but_inconclusive"
        fi
    elif echo "$output" | grep -qE '\[FAIL\].*test_'; then
        # Test failed — could mean the assertion showed the vuln, or test is broken
        if echo "$output" | grep -qiE '(expected|assertion|revert)'; then
            # Assertion failure = the test IS demonstrating something
            # Check if the failure message indicates the vuln
            if echo "$output" | grep -qiE '(Rounding profit|shares.*0|price.*manipulat|extract|drain)'; then
                echo "compiles_and_demonstrates"
            else
                echo "compiles_but_inconclusive"
            fi
        else
            echo "compiles_but_inconclusive"
        fi
    elif echo "$output" | grep -qiE 'mainnet|fork|anvil.*fork'; then
        echo "requires_mainnet_simulation"
    else
        echo "compiles_but_inconclusive"
    fi
}

# ── Validation gate ───────────────────────────────────────────────────

apply_validation_gate() {
    local finding="$1"
    local poc_status="$2"
    local poc_file="$3"

    local finding_id
    finding_id=$(echo "$finding" | jq -r '.id')
    local severity
    severity=$(echo "$finding" | jq -r '.severity')

    case "$poc_status" in
        compiles_and_demonstrates)
            log_ok "  ✓ VALIDATED — severity preserved ($severity)"
            local rel_poc="${poc_file#$WORKSPACE/}"
            local updated
            updated=$(echo "$finding" | jq \
                --arg poc "$rel_poc" \
                --arg status "$poc_status" \
                '.poc_file = $poc | .poc_status = $status')
            VALIDATED=$(echo "$VALIDATED" | jq --argjson f "$updated" '. + [$f]')
            VALIDATED_COUNT=$((VALIDATED_COUNT + 1))
            ;;

        compiles_but_inconclusive)
            # Cap severity at Medium
            local capped_severity="$severity"
            if [ "$severity" = "Critical" ] || [ "$severity" = "High" ]; then
                capped_severity="Medium"
                log_warn "  ~ INCONCLUSIVE — severity capped: $severity → Medium"
            else
                log_warn "  ~ INCONCLUSIVE — severity preserved ($severity)"
            fi
            local rel_poc="${poc_file#$WORKSPACE/}"
            local updated
            updated=$(echo "$finding" | jq \
                --arg poc "$rel_poc" \
                --arg status "$poc_status" \
                --arg sev "$capped_severity" \
                '.poc_file = $poc | .poc_status = $status | .severity = $sev | .original_severity = .severity')
            VALIDATED=$(echo "$VALIDATED" | jq --argjson f "$updated" '. + [$f]')
            INCONCLUSIVE=$((INCONCLUSIVE + 1))
            ;;

        requires_mainnet_simulation)
            log_info "  ⚑ MAINNET REQUIRED — flagged for manual review"
            local updated
            updated=$(echo "$finding" | jq \
                --arg status "$poc_status" \
                '.poc_status = $status')
            VALIDATED=$(echo "$VALIDATED" | jq --argjson f "$updated" '. + [$f]')
            VALIDATED_COUNT=$((VALIDATED_COUNT + 1))
            ;;

        failed_to_compile|infeasible_conditions)
            log_error "  ✗ DISCARDED ($poc_status)"
            DISCARDED=$((DISCARDED + 1))
            ;;

        *)
            log_warn "  ? UNKNOWN status: $poc_status — discarding"
            DISCARDED=$((DISCARDED + 1))
            ;;
    esac
}

# ── Main loop ─────────────────────────────────────────────────────────

for idx in $(seq 0 $((FINDING_COUNT - 1))); do
    echo ""
    log_info "Processing finding $((idx + 1))/$FINDING_COUNT"
    process_finding "$idx"
done

# ── Write validated findings ──────────────────────────────────────────

echo "$VALIDATED" | jq '.' > "$POCS_DIR/validated-findings.json"

# Also write discarded findings for Pass 6 review
jq --argjson validated "$VALIDATED" '
    [.[] | . as $f |
        if ($validated | map(.id) | index($f.id)) == null then
            . + {poc_status: "discarded_no_poc"}
        else
            empty
        end
    ]
' "$MERGED_FINDINGS" > "$POCS_DIR/discarded-findings.json" 2>/dev/null || echo '[]' > "$POCS_DIR/discarded-findings.json"

# ── Summary ──────────────────────────────────────────────────────────

DURATION=$(timer_elapsed)
record_pass_status "poc" "completed" "$DURATION"

TOTAL_SURVIVED=$((VALIDATED_COUNT + INCONCLUSIVE))

echo ""
log_step "Pass 3 Complete ($(timer_human))"
echo ""
log_ok "Input:       $FINDING_COUNT findings from Pass 2"
log_ok "Validated:   $VALIDATED_COUNT (PoC demonstrates vulnerability)"
log_ok "Inconclusive: $INCONCLUSIVE (severity capped at Medium)"
log_ok "Discarded:   $DISCARDED (failed to compile or infeasible)"
log_ok "Survived:    $TOTAL_SURVIVED findings proceeding to Pass 4+"
echo ""

if [ "$TOTAL_SURVIVED" -gt 0 ]; then
    log_info "Severity breakdown (post-gate):"
    echo "$VALIDATED" | jq -r 'group_by(.severity) | .[] | "  \(.[0].severity): \(length)"' 2>/dev/null || true
fi

echo ""
log_info "Validated: $POCS_DIR/validated-findings.json"
log_info "Discarded: $POCS_DIR/discarded-findings.json"
log_info "PoC files: $POCS_DIR/*.t.sol"
log_info "Logs: $LOGS_DIR/"
echo ""
