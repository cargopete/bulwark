#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
# DOYRAN — Pipeline Orchestrator
# ════════════════════════════════════════════════════════════════════════
# Runs the six-pass audit pipeline sequentially.
#
# Usage:
#   ./pipeline/doyran-pipeline.sh                    # Full pipeline
#   ./pipeline/doyran-pipeline.sh --pass 1           # Recon only
#   ./pipeline/doyran-pipeline.sh --pass 1-3         # Passes 1 through 3
#   ./pipeline/doyran-pipeline.sh --resume 3         # Resume from Pass 3
#
# Each pass produces structured JSON in audit-workspace/.
# All inter-pass communication is via files, not in-context conversation.
# ════════════════════════════════════════════════════════════════════════

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib/common.sh"

# ── Parse arguments ──────────────────────────────────────────────────
START_PASS=1
END_PASS=6

while [[ $# -gt 0 ]]; do
    case "$1" in
        --pass)
            if [[ "$2" =~ ^([0-9]+)-([0-9]+)$ ]]; then
                START_PASS="${BASH_REMATCH[1]}"
                END_PASS="${BASH_REMATCH[2]}"
            else
                START_PASS="$2"
                END_PASS="$2"
            fi
            shift 2
            ;;
        --resume)
            START_PASS="$2"
            shift 2
            ;;
        --workspace)
            WORKSPACE="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: doyran-pipeline.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --pass N       Run only pass N"
            echo "  --pass N-M     Run passes N through M"
            echo "  --resume N     Resume from pass N (reuses earlier outputs)"
            echo "  --workspace P  Custom workspace path"
            echo "  -h, --help     Show this help"
            echo ""
            echo "Passes:"
            echo "  1  Reconnaissance (deterministic, no AI)"
            echo "  2  Multi-Agent Adversarial Analysis (3x Claude sessions)"
            echo "  3  PoC Generation & Validation (Claude + forge test)"
            echo "  4  Fuzzing Campaign (Foundry + Medusa + Echidna)"
            echo "  5  Formal Verification (Halmos)"
            echo "  6  Adversarial Review (fresh Claude session)"
            exit 0
            ;;
        *)
            log_error "Unknown option: $1"
            exit 1
            ;;
    esac
done

# ── Banner ────────────────────────────────────────────────────────────
echo ""
printf "${BOLD}${CYAN}"
cat << 'BANNER'
    ╔═══════════════════════════════════════════════════════════╗
    ║                                                           ║
    ║   ██████╗  ██████╗ ██╗   ██╗██████╗  █████╗ ███╗   ██╗   ║
    ║   ██╔══██╗██╔═══██╗╚██╗ ██╔╝██╔══██╗██╔══██╗████╗  ██║   ║
    ║   ██║  ██║██║   ██║ ╚████╔╝ ██████╔╝███████║██╔██╗ ██║   ║
    ║   ██║  ██║██║   ██║  ╚██╔╝  ██╔══██╗██╔══██║██║╚██╗██║   ║
    ║   ██████╔╝╚██████╔╝   ██║   ██║  ██║██║  ██║██║ ╚████║   ║
    ║   ╚═════╝  ╚═════╝    ╚═╝   ╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═══╝   ║
    ║                                                           ║
    ║   Multi-Pass Smart Contract Audit Pipeline                ║
    ║   The Graph Protocol                                      ║
    ║                                                           ║
    ╚═══════════════════════════════════════════════════════════╝
BANNER
printf "${RESET}"
echo ""

# ── Preflight ─────────────────────────────────────────────────────────
log_info "Audit directory: $AUDIT_DIR"
log_info "Workspace: $WORKSPACE"
log_info "Passes: $START_PASS through $END_PASS"
echo ""

# Check we're in the right place
if [ ! -d "$AUDIT_DIR" ]; then
    log_error "Audit directory not found: $AUDIT_DIR"
    log_error "Run the entrypoint script first to clone the target repo."
    exit 1
fi

# Check Claude auth for AI passes
CLAUDE_AUTH=false
if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
    CLAUDE_AUTH=true
elif [ -f "$DOYRAN_ROOT/.claude/.credentials.json" ]; then
    CLAUDE_AUTH=true
elif command -v claude >/dev/null 2>&1; then
    # Check if claude can authenticate
    if claude --version >/dev/null 2>&1; then
        CLAUDE_AUTH=true
    fi
fi

if [ "$CLAUDE_AUTH" = false ] && [ "$END_PASS" -ge 2 ]; then
    log_warn "Claude Code not authenticated — AI passes (2, 3, 6) will be skipped."
    log_warn "Authenticate via: export ANTHROPIC_API_KEY=... or: claude login"
    echo ""
fi

# Initialise workspace
init_workspace

PIPELINE_START=$(date +%s)

# ── Run passes ────────────────────────────────────────────────────────

run_pass() {
    local pass_num="$1"
    local pass_name="$2"
    local pass_script="$3"
    local requires_ai="${4:-false}"

    if [ "$pass_num" -lt "$START_PASS" ] || [ "$pass_num" -gt "$END_PASS" ]; then
        return 0
    fi

    # Skip AI passes if not authenticated
    if [ "$requires_ai" = true ] && [ "$CLAUDE_AUTH" = false ]; then
        log_warn "Skipping Pass $pass_num ($pass_name) — Claude Code not authenticated"
        record_pass_status "$pass_name" "skipped" "0"
        return 0
    fi

    log_step "Pass $pass_num: $pass_name"

    local pass_start
    pass_start=$(date +%s)

    if [ -x "$pass_script" ]; then
        if "$pass_script"; then
            local pass_duration=$(( $(date +%s) - pass_start ))
            record_pass_status "$pass_name" "completed" "$pass_duration"
            log_ok "Pass $pass_num completed in ${pass_duration}s"
        else
            local pass_duration=$(( $(date +%s) - pass_start ))
            record_pass_status "$pass_name" "failed" "$pass_duration"
            log_error "Pass $pass_num FAILED after ${pass_duration}s"
            return 1
        fi
    else
        log_warn "Pass $pass_num script not found or not executable: $pass_script"
        record_pass_status "$pass_name" "not_implemented" "0"
    fi
}

# Pass 1: Reconnaissance (deterministic, no AI)
run_pass 1 "recon" "$SCRIPT_DIR/pass1-recon.sh" false

# Pass 2: Multi-Agent Adversarial Analysis (requires AI)
run_pass 2 "agents" "$SCRIPT_DIR/pass2-agents.sh" true

# Pass 3: PoC Generation & Validation (requires AI)
run_pass 3 "poc" "$SCRIPT_DIR/pass3-poc.sh" true

# Pass 4: Fuzzing Campaign (deterministic tools, but setup may use AI)
run_pass 4 "fuzzing" "$SCRIPT_DIR/pass4-fuzzing.sh" false

# Pass 5: Formal Verification (deterministic)
run_pass 5 "formal" "$SCRIPT_DIR/pass5-formal.sh" false

# Pass 6: Adversarial Review (requires AI)
run_pass 6 "review" "$SCRIPT_DIR/pass6-review.sh" true

# ── Pipeline summary ─────────────────────────────────────────────────
PIPELINE_DURATION=$(( $(date +%s) - PIPELINE_START ))
echo ""
log_step "Pipeline Complete"
echo ""

if [ -f "$PASS_STATUS_FILE" ]; then
    log_info "Pass results:"
    jq -r '.passes | to_entries[] | "  \(.key): \(.value.status) (\(.value.duration)s)"' \
        "$PASS_STATUS_FILE"
fi

echo ""
log_info "Total duration: $(( PIPELINE_DURATION / 60 ))m $(( PIPELINE_DURATION % 60 ))s"
log_info "Workspace: $WORKSPACE"

# Check for findings
if [ -f "$WORKSPACE/findings/merged-deduplicated.json" ]; then
    FINDING_COUNT=$(jq length "$WORKSPACE/findings/merged-deduplicated.json" 2>/dev/null || echo "0")
    log_info "Findings: $FINDING_COUNT"
fi

if [ -f "$WORKSPACE/pocs/validated-findings.json" ]; then
    VALIDATED_COUNT=$(jq '[.[] | select(.poc_status == "compiles_and_demonstrates")] | length' \
        "$WORKSPACE/pocs/validated-findings.json" 2>/dev/null || echo "0")
    log_info "Validated with PoC: $VALIDATED_COUNT"
fi

if [ -f "$WORKSPACE/final-report.md" ]; then
    log_ok "Final report: $WORKSPACE/final-report.md"
fi

echo ""
