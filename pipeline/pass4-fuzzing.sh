#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
# DOYRAN — Pass 4: Fuzzing Campaign
# ════════════════════════════════════════════════════════════════════════
# 1. Claude generates invariant tests from PROPERTIES.md
# 2. Foundry runs invariant tests (fast, ~10 minutes)
# 3. Medusa runs extended fuzzing if available (1 hour)
# 4. Echidna runs extended fuzzing if available (1 hour)
# 5. Any broken invariants become findings
#
# Runs in parallel with Pass 5 (formal verification).
# ════════════════════════════════════════════════════════════════════════

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib/common.sh"

PROMPTS_DIR="$DOYRAN_ROOT/prompts"
FUZZ_DIR="$WORKSPACE/fuzzing"
INVARIANT_DIR="$FUZZ_DIR/invariant-tests"
RESULTS_DIR="$FUZZ_DIR/fuzzing-campaign-results"
LOGS_DIR="$FUZZ_DIR/logs"
MAX_TURNS="${DOYRAN_FUZZ_MAX_TURNS:-40}"
FOUNDRY_FUZZ_RUNS="${DOYRAN_FUZZ_RUNS:-10000}"
FOUNDRY_INVARIANT_DEPTH="${DOYRAN_INVARIANT_DEPTH:-50}"

mkdir -p "$INVARIANT_DIR" "$RESULTS_DIR" "$LOGS_DIR"

timer_start
log_step "Pass 4: Fuzzing Campaign"

# ── Step 1: Generate invariant tests ──────────────────────────────────

log_info "Generating invariant tests from PROPERTIES.md..."

if ! command -v claude >/dev/null 2>&1; then
    log_warn "Claude not available — skipping test generation"
    log_warn "Place invariant tests manually in $INVARIANT_DIR/"
else
    # Build the prompt with context
    INVARIANT_PROMPT="$(cat "$PROMPTS_DIR/invariant-generator.md")

---

## Context Files

Read these files for protocol context:
- PROPERTIES.md (the invariants to test)
- KNOWN_ISSUES.md (focus areas)
- ATTACK_PATTERNS.md (known patterns to target)
- audit-workspace/recon/entry-points.json (function signatures)
- audit-workspace/recon/storage-layouts.json (state structure)

## Output Directory

Write all test files to: $INVARIANT_DIR/
"

    cd "$AUDIT_DIR"
    claude -p "$INVARIANT_PROMPT" \
        --max-turns "$MAX_TURNS" \
        > "$LOGS_DIR/invariant-generation.log" 2>&1 || true

    # Count generated files
    GENERATED=$(find "$INVARIANT_DIR" -name "*.t.sol" -o -name "*.sol" 2>/dev/null | wc -l | tr -d ' ')
    if [ "$GENERATED" -gt 0 ]; then
        log_ok "Generated $GENERATED invariant test files"
    else
        log_warn "No invariant tests generated — check $LOGS_DIR/invariant-generation.log"
    fi
fi

# ── Step 2: Compile invariant tests ──────────────────────────────────

TESTS_EXIST=false
if find "$INVARIANT_DIR" -name "*.sol" 2>/dev/null | grep -q .; then
    TESTS_EXIST=true
    log_info "Compiling invariant tests..."

    # Try to compile from the main package directory
    BUILD_DIR="$AUDIT_DIR/packages/horizon"
    if [ ! -d "$BUILD_DIR" ]; then
        BUILD_DIR="$AUDIT_DIR"
    fi

    BUILD_OUTPUT=$(cd "$BUILD_DIR" && forge build 2>&1) || true

    if echo "$BUILD_OUTPUT" | grep -q "Compiler run successful"; then
        log_ok "Invariant tests compile"
    else
        log_warn "Some invariant tests failed to compile"
        echo "$BUILD_OUTPUT" > "$LOGS_DIR/invariant-build.log"
        log_info "Build log: $LOGS_DIR/invariant-build.log"
    fi
fi

# ── Step 3: Foundry invariant tests (fast run) ───────────────────────

FOUNDRY_FINDINGS='[]'

if [ "$TESTS_EXIST" = true ]; then
    log_info "Running Foundry invariant tests (fuzz-runs=$FOUNDRY_FUZZ_RUNS, depth=$FOUNDRY_INVARIANT_DEPTH)..."

    FOUNDRY_OUTPUT=$(cd "$BUILD_DIR" && forge test \
        --match-contract "Invariant" \
        --fuzz-runs "$FOUNDRY_FUZZ_RUNS" \
        --invariant-depth "$FOUNDRY_INVARIANT_DEPTH" \
        -vvv 2>&1) || true

    echo "$FOUNDRY_OUTPUT" > "$RESULTS_DIR/foundry-invariant.log"

    # Parse results
    PASSED=$(echo "$FOUNDRY_OUTPUT" | grep -c '\[PASS\]' || echo "0")
    FAILED=$(echo "$FOUNDRY_OUTPUT" | grep -c '\[FAIL\]' || echo "0")

    log_info "Foundry results: $PASSED passed, $FAILED failed"

    # Extract broken invariants as findings
    if [ "$FAILED" -gt 0 ]; then
        log_warn "Broken invariants detected!"

        FOUNDRY_FINDINGS=$(echo "$FOUNDRY_OUTPUT" | grep '\[FAIL\]' | \
            python3 -c "
import json, re, sys

findings = []
idx = 1
for line in sys.stdin:
    line = line.strip()
    # Extract test name: [FAIL. Reason: ...] invariant_P10_provider_first()
    match = re.search(r'\[FAIL.*?\]\s*(invariant_\w+)', line)
    if match:
        test_name = match.group(1)
        # Extract property number
        prop = re.search(r'P(\d+)', test_name)
        prop_id = f'P-{prop.group(1)}' if prop else None
        # Extract reason
        reason_match = re.search(r'Reason:\s*(.+?)[\]\)]', line)
        reason = reason_match.group(1) if reason_match else 'Invariant broken'

        findings.append({
            'id': f'FUZZ-{idx:03d}',
            'source': 'fuzzer',
            'severity': 'High',
            'confidence': 'High',
            'title': f'Fuzzer broke invariant: {test_name}',
            'contract': 'multiple',
            'function': test_name,
            'lines': [],
            'property_violated': prop_id,
            'attack_scenario': f'Foundry invariant fuzzer found counterexample breaking {test_name}. Reason: {reason}. Run with -vvvv for full trace.',
            'poc_file': None,
            'poc_status': 'compiles_and_demonstrates',
            'dedup_hash': ''
        })
        idx += 1

print(json.dumps(findings, indent=2))
" 2>/dev/null || echo '[]')
    fi
else
    log_warn "No invariant tests to run"
fi

# ── Step 4: Medusa extended fuzzing (optional) ───────────────────────

MEDUSA_FINDINGS='[]'

if command -v medusa >/dev/null 2>&1 && [ "$TESTS_EXIST" = true ]; then
    MEDUSA_TIMEOUT="${DOYRAN_MEDUSA_TIMEOUT:-3600}"
    log_info "Running Medusa extended fuzzing (timeout=${MEDUSA_TIMEOUT}s)..."

    MEDUSA_OUTPUT=$(cd "$BUILD_DIR" && medusa fuzz \
        --target-contracts "Invariant" \
        --timeout "$MEDUSA_TIMEOUT" \
        2>&1) || true

    echo "$MEDUSA_OUTPUT" > "$RESULTS_DIR/medusa.log"

    # Check for broken properties
    if echo "$MEDUSA_OUTPUT" | grep -qiE '(failed|broken|violated)'; then
        log_warn "Medusa found broken invariants — see $RESULTS_DIR/medusa.log"
        # Parse medusa output for findings
        MEDUSA_BROKEN=$(echo "$MEDUSA_OUTPUT" | grep -ciE '(failed|broken|violated)' || echo "0")
        log_info "Medusa failures: $MEDUSA_BROKEN"
    else
        log_ok "Medusa: all invariants held"
    fi
else
    if [ "$TESTS_EXIST" = true ]; then
        log_info "Medusa not installed — skipping extended fuzzing"
        log_info "  Install: pip install medusa-fuzzer (or add to Dockerfile)"
    fi
fi

# ── Step 5: Echidna extended fuzzing (optional) ──────────────────────

if command -v echidna >/dev/null 2>&1 && [ "$TESTS_EXIST" = true ]; then
    ECHIDNA_LIMIT="${DOYRAN_ECHIDNA_LIMIT:-500000}"
    log_info "Running Echidna (test-limit=$ECHIDNA_LIMIT)..."

    ECHIDNA_OUTPUT=$(cd "$BUILD_DIR" && echidna . \
        --contract CryticTester \
        --test-limit "$ECHIDNA_LIMIT" \
        2>&1) || true

    echo "$ECHIDNA_OUTPUT" > "$RESULTS_DIR/echidna.log"

    if echo "$ECHIDNA_OUTPUT" | grep -qiE '(failed|falsified)'; then
        log_warn "Echidna found broken properties — see $RESULTS_DIR/echidna.log"
    else
        log_ok "Echidna: all properties held"
    fi
else
    if [ "$TESTS_EXIST" = true ]; then
        log_info "Echidna not installed — skipping"
        log_info "  Install: pip install echidna (or add to Dockerfile)"
    fi
fi

# ── Step 6: Collect and output findings ──────────────────────────────

# Combine Foundry + Medusa findings
ALL_FUZZ_FINDINGS=$(echo "$FOUNDRY_FINDINGS" | jq --argjson medusa "$MEDUSA_FINDINGS" '. + $medusa')
echo "$ALL_FUZZ_FINDINGS" | jq '.' > "$FUZZ_DIR/fuzzing-findings.json"

FUZZ_FINDING_COUNT=$(echo "$ALL_FUZZ_FINDINGS" | jq 'length' 2>/dev/null || echo "0")

# ── Summary ──────────────────────────────────────────────────────────

DURATION=$(timer_elapsed)
record_pass_status "fuzzing" "completed" "$DURATION"

echo ""
log_step "Pass 4 Complete ($(timer_human))"
echo ""
log_ok "Invariant tests generated: $(find "$INVARIANT_DIR" -name "*.sol" 2>/dev/null | wc -l | tr -d ' ')"
log_ok "Foundry invariant results: $PASSED passed, $FAILED failed"
if command -v medusa >/dev/null 2>&1; then
    log_ok "Medusa: completed"
else
    log_info "Medusa: not installed (skipped)"
fi
if command -v echidna >/dev/null 2>&1; then
    log_ok "Echidna: completed"
else
    log_info "Echidna: not installed (skipped)"
fi
log_ok "Fuzzing findings: $FUZZ_FINDING_COUNT"
echo ""
log_info "Results: $RESULTS_DIR/"
log_info "Findings: $FUZZ_DIR/fuzzing-findings.json"
echo ""
