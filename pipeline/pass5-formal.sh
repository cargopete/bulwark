#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
# DOYRAN — Pass 5: Formal Verification
# ════════════════════════════════════════════════════════════════════════
# Bounded model checking via Halmos on critical properties:
#   P-10 (provider-first slashing) — most critical
#   P-15 (fee distribution conservation) — exact arithmetic, best fit
#   P-19 (operators can't extract value) — bounded sequence check
#   P-1  (stake conservation) — may timeout
#   P-16 (RAV monotonicity) — if time permits
#
# Time budget: 1-2 hours. Runs in parallel with Pass 4.
# ════════════════════════════════════════════════════════════════════════

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib/common.sh"

PROMPTS_DIR="$DOYRAN_ROOT/prompts"
FORMAL_DIR="$WORKSPACE/formal"
LOGS_DIR="$FORMAL_DIR/logs"
MAX_TURNS="${DOYRAN_HALMOS_MAX_TURNS:-30}"
SOLVER_TIMEOUT="${DOYRAN_SOLVER_TIMEOUT:-300}"
LOOP_BOUND="${DOYRAN_LOOP_BOUND:-5}"

mkdir -p "$FORMAL_DIR" "$LOGS_DIR"

timer_start
log_step "Pass 5: Formal Verification"

# ── Check Halmos availability ─────────────────────────────────────────

HALMOS_AVAILABLE=false
if command -v halmos >/dev/null 2>&1; then
    HALMOS_AVAILABLE=true
    HALMOS_VERSION=$(halmos --version 2>&1 | head -1 || echo "unknown")
    log_ok "Halmos available: $HALMOS_VERSION"
else
    log_warn "Halmos not installed"
    log_info "  Install: pip install halmos"
    log_info "  Tests will still be generated (compilable with forge) but not run symbolically"
fi

# ── Step 1: Generate symbolic tests ──────────────────────────────────

log_info "Generating symbolic tests for critical properties..."

if ! command -v claude >/dev/null 2>&1; then
    log_warn "Claude not available — skipping test generation"
    log_warn "Place symbolic tests manually in $FORMAL_DIR/"
else
    HALMOS_PROMPT="$(cat "$PROMPTS_DIR/halmos-generator.md")

---

## Context Files

Read these files:
- PROPERTIES.md (the properties to verify)
- AUDIT_CONTEXT.md (protocol overview)
- audit-workspace/recon/entry-points.json (function signatures)

## Halmos Availability

$([ "$HALMOS_AVAILABLE" = true ] && echo "Halmos IS installed ($HALMOS_VERSION). Tests will be run." || echo "Halmos is NOT installed. Generate compilable tests anyway — they serve as documentation and can run when Halmos is added.")

## Output Directory

Write all test files and the assessment JSON to: $FORMAL_DIR/
"

    cd "$AUDIT_DIR"
    claude -p "$HALMOS_PROMPT" \
        --max-turns "$MAX_TURNS" \
        > "$LOGS_DIR/halmos-generation.log" 2>&1 || true

    GENERATED=$(find "$FORMAL_DIR" -name "*.t.sol" -o -name "*.sol" 2>/dev/null | wc -l | tr -d ' ')
    if [ "$GENERATED" -gt 0 ]; then
        log_ok "Generated $GENERATED symbolic test files"
    else
        log_warn "No symbolic tests generated — check $LOGS_DIR/halmos-generation.log"
    fi
fi

# ── Step 2: Compile symbolic tests ───────────────────────────────────

TESTS_EXIST=false
if find "$FORMAL_DIR" -name "*.sol" 2>/dev/null | grep -q .; then
    TESTS_EXIST=true
    log_info "Compiling symbolic tests..."

    BUILD_DIR="$AUDIT_DIR/packages/horizon"
    if [ ! -d "$BUILD_DIR" ]; then
        BUILD_DIR="$AUDIT_DIR"
    fi

    BUILD_OUTPUT=$(cd "$BUILD_DIR" && forge build 2>&1) || true

    if echo "$BUILD_OUTPUT" | grep -q "Compiler run successful"; then
        log_ok "Symbolic tests compile"
    else
        log_warn "Some symbolic tests failed to compile"
        echo "$BUILD_OUTPUT" > "$LOGS_DIR/formal-build.log"
    fi
fi

# ── Step 3: Run Halmos on each property ──────────────────────────────

VERIFICATION_RESULTS='{}'
TARGET_PROPERTIES=("P-10" "P-15" "P-19" "P-1" "P-16")

if [ "$HALMOS_AVAILABLE" = true ] && [ "$TESTS_EXIST" = true ]; then
    log_info "Running Halmos verification (solver-timeout=${SOLVER_TIMEOUT}s, loop=${LOOP_BOUND})..."
    echo ""

    for prop in "${TARGET_PROPERTIES[@]}"; do
        prop_num="${prop#P-}"
        # Find the check function for this property
        check_func="check_P${prop_num}"

        # Check if any test file contains this function
        if ! grep -rq "$check_func" "$FORMAL_DIR/"*.sol 2>/dev/null; then
            log_info "  $prop: no symbolic test found — skipping"
            VERIFICATION_RESULTS=$(echo "$VERIFICATION_RESULTS" | jq \
                --arg prop "$prop" \
                '.[$prop] = {status: "no_test", duration: 0}')
            continue
        fi

        log_info "  $prop: verifying..."
        local_start=$(date +%s)

        HALMOS_OUTPUT=$(cd "$BUILD_DIR" && timeout $((SOLVER_TIMEOUT + 60)) halmos \
            --function "$check_func" \
            --loop "$LOOP_BOUND" \
            --solver-timeout-assertion "$SOLVER_TIMEOUT" \
            --solver-timeout-branching "$SOLVER_TIMEOUT" \
            -vvv 2>&1) || true

        local_duration=$(( $(date +%s) - local_start ))
        echo "$HALMOS_OUTPUT" > "$FORMAL_DIR/halmos-${prop}.log"

        # Classify result
        if echo "$HALMOS_OUTPUT" | grep -qE 'Counterexample'; then
            status="VIOLATED"
            log_warn "  $prop: VIOLATED — counterexample found (${local_duration}s)"
        elif echo "$HALMOS_OUTPUT" | grep -qE '(timeout|Timeout|TIMEOUT)'; then
            status="TIMEOUT"
            log_info "  $prop: TIMEOUT after ${local_duration}s"
        elif echo "$HALMOS_OUTPUT" | grep -qE '(Verified|passed|0 counterexample)'; then
            status="VERIFIED"
            log_ok "  $prop: VERIFIED (bounded, loop=$LOOP_BOUND) in ${local_duration}s"
        elif echo "$HALMOS_OUTPUT" | grep -qE '(Error|error|panic)'; then
            status="ERROR"
            log_error "  $prop: ERROR — see halmos-${prop}.log"
        else
            status="UNKNOWN"
            log_warn "  $prop: UNKNOWN result in ${local_duration}s"
        fi

        # Extract counterexample if present
        counterexample=""
        if [ "$status" = "VIOLATED" ]; then
            counterexample=$(echo "$HALMOS_OUTPUT" | grep -A 20 "Counterexample" | head -20 || true)
        fi

        VERIFICATION_RESULTS=$(echo "$VERIFICATION_RESULTS" | jq \
            --arg prop "$prop" \
            --arg status "$status" \
            --argjson dur "$local_duration" \
            --arg ce "$counterexample" \
            '.[$prop] = {status: $status, duration: $dur, counterexample: (if $ce == "" then null else $ce end), loop_bound: '"$LOOP_BOUND"'}')
    done
else
    if [ "$TESTS_EXIST" = true ]; then
        log_info "Halmos not available — recording all properties as NOT_RUN"
    fi
    for prop in "${TARGET_PROPERTIES[@]}"; do
        VERIFICATION_RESULTS=$(echo "$VERIFICATION_RESULTS" | jq \
            --arg prop "$prop" \
            '.[$prop] = {status: "not_run", reason: "halmos_not_installed"}')
    done
fi

# ── Step 4: Generate findings from violations ─────────────────────────

FORMAL_FINDINGS='[]'

for prop in "${TARGET_PROPERTIES[@]}"; do
    status=$(echo "$VERIFICATION_RESULTS" | jq -r --arg p "$prop" '.[$p].status')

    if [ "$status" = "VIOLATED" ]; then
        counterexample=$(echo "$VERIFICATION_RESULTS" | jq -r --arg p "$prop" '.[$p].counterexample // "See log for details"')

        FORMAL_FINDINGS=$(echo "$FORMAL_FINDINGS" | jq \
            --arg prop "$prop" \
            --arg ce "$counterexample" \
            '. + [{
                id: ("HALMOS-" + $prop),
                source: "halmos",
                severity: "Critical",
                confidence: "High",
                title: ("Halmos found counterexample violating " + $prop),
                contract: "multiple",
                function: ("check_" + $prop),
                lines: [],
                property_violated: $prop,
                attack_scenario: ("Halmos bounded model checker found a concrete counterexample proving " + $prop + " can be violated. Counterexample: " + $ce),
                poc_file: null,
                poc_status: "compiles_and_demonstrates",
                dedup_hash: ""
            }]')
    fi
done

# ── Write outputs ─────────────────────────────────────────────────────

echo "$VERIFICATION_RESULTS" | jq '{
    properties: .,
    summary: {
        verified: [to_entries[] | select(.value.status == "VERIFIED") | .key],
        violated: [to_entries[] | select(.value.status == "VIOLATED") | .key],
        timeout: [to_entries[] | select(.value.status == "TIMEOUT") | .key],
        not_run: [to_entries[] | select(.value.status == "not_run" or .value.status == "no_test") | .key]
    },
    loop_bound: '"$LOOP_BOUND"',
    solver_timeout: '"$SOLVER_TIMEOUT"'
}' > "$FORMAL_DIR/verification-summary.json"

echo "$FORMAL_FINDINGS" | jq '.' > "$FORMAL_DIR/formal-findings.json"

# ── Summary ──────────────────────────────────────────────────────────

DURATION=$(timer_elapsed)
record_pass_status "formal" "completed" "$DURATION"

VERIFIED_COUNT=$(echo "$VERIFICATION_RESULTS" | jq '[to_entries[] | select(.value.status == "VERIFIED")] | length')
VIOLATED_COUNT=$(echo "$VERIFICATION_RESULTS" | jq '[to_entries[] | select(.value.status == "VIOLATED")] | length')
TIMEOUT_COUNT=$(echo "$VERIFICATION_RESULTS" | jq '[to_entries[] | select(.value.status == "TIMEOUT")] | length')
NOT_RUN_COUNT=$(echo "$VERIFICATION_RESULTS" | jq '[to_entries[] | select(.value.status == "not_run" or .value.status == "no_test")] | length')

echo ""
log_step "Pass 5 Complete ($(timer_human))"
echo ""
log_ok "Symbolic tests: $(find "$FORMAL_DIR" -name "*.sol" 2>/dev/null | wc -l | tr -d ' ')"
log_ok "Verified:    $VERIFIED_COUNT (bounded, loop=$LOOP_BOUND)"
if [ "$VIOLATED_COUNT" -gt 0 ]; then
    log_error "Violated:    $VIOLATED_COUNT — COUNTEREXAMPLES FOUND"
else
    log_ok "Violated:    0"
fi
log_info "Timeout:     $TIMEOUT_COUNT (recommend fuzzing instead)"
log_info "Not run:     $NOT_RUN_COUNT"
echo ""

if [ "$HALMOS_AVAILABLE" = false ]; then
    log_info "Note: Halmos was not installed. Symbolic tests were generated but not executed."
    log_info "Install halmos and re-run: pip install halmos"
fi

echo ""
log_info "Verification summary: $FORMAL_DIR/verification-summary.json"
log_info "Findings: $FORMAL_DIR/formal-findings.json"
log_info "Logs: $FORMAL_DIR/halmos-P*.log"
echo ""
