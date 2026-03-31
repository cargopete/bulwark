#!/usr/bin/env bash
# Shared utilities for the bulwark pipeline.
# Source this file from any pass script: source "$(dirname "$0")/lib/common.sh"

set -euo pipefail

# ── Paths ──────────────────────────────────────────────────────────────
BULWARK_ROOT="${BULWARK_ROOT:-/home/auditor}"
AUDIT_DIR="${AUDIT_DIR:-$BULWARK_ROOT/audits/graph-contracts}"
WORKSPACE="${WORKSPACE:-$AUDIT_DIR/audit-workspace}"
CONTEXT_DIR="${CONTEXT_DIR:-$BULWARK_ROOT/context}"

# ── Colours (disabled if not a terminal) ───────────────────────────────
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    BLUE='\033[0;34m'
    CYAN='\033[0;36m'
    BOLD='\033[1m'
    DIM='\033[2m'
    RESET='\033[0m'
else
    RED='' GREEN='' YELLOW='' BLUE='' CYAN='' BOLD='' DIM='' RESET=''
fi

# ── Logging ────────────────────────────────────────────────────────────
_log() {
    local level="$1" colour="$2"
    shift 2
    printf "${colour}[%s] [%-5s]${RESET} %s\n" "$(date +%H:%M:%S)" "$level" "$*"
}

log_info()  { _log "INFO"  "$CYAN"   "$@"; }
log_ok()    { _log "OK"    "$GREEN"  "$@"; }
log_warn()  { _log "WARN"  "$YELLOW" "$@"; }
log_error() { _log "ERROR" "$RED"    "$@"; }
log_step()  { printf "\n${BOLD}${BLUE}━━━ %s ━━━${RESET}\n\n" "$*"; }

# ── Timing ─────────────────────────────────────────────────────────────
_TIMER_START=""

timer_start() {
    _TIMER_START=$(date +%s)
}

timer_elapsed() {
    local now
    now=$(date +%s)
    echo $(( now - _TIMER_START ))
}

timer_human() {
    local secs
    secs=$(timer_elapsed)
    if [ "$secs" -lt 60 ]; then
        echo "${secs}s"
    else
        echo "$(( secs / 60 ))m $(( secs % 60 ))s"
    fi
}

# ── Workspace management ──────────────────────────────────────────────
init_workspace() {
    mkdir -p \
        "$WORKSPACE/recon" \
        "$WORKSPACE/findings" \
        "$WORKSPACE/pocs" \
        "$WORKSPACE/fuzzing/invariant-tests" \
        "$WORKSPACE/fuzzing/fuzzing-campaign-results" \
        "$WORKSPACE/formal" \
        "$WORKSPACE/review"
    log_info "Workspace initialised: $WORKSPACE"
}

# ── JSON helpers ──────────────────────────────────────────────────────
# Wrap a value in a JSON object with metadata
json_wrap() {
    local pass="$1" file="$2"
    local timestamp
    timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    # Add generated timestamp and pass name to the JSON
    jq --arg ts "$timestamp" --arg pass "$pass" \
        '. + {generated: $ts, pass: $pass}' "$file" > "${file}.tmp" \
        && mv "${file}.tmp" "$file"
}

# Validate JSON file exists and is valid
json_validate() {
    local file="$1"
    if [ ! -f "$file" ]; then
        log_error "Expected JSON file not found: $file"
        return 1
    fi
    if ! jq empty "$file" 2>/dev/null; then
        log_error "Invalid JSON: $file"
        return 1
    fi
    return 0
}

# ── Contract discovery ─────────────────────────────────────────────────
# Find all .sol files in scope (excludes tests, mocks, libraries)
find_sol_files() {
    local scope="${1:-$AUDIT_DIR}"
    find "$scope" \
        -name "*.sol" \
        -not -path "*/test/*" \
        -not -path "*/tests/*" \
        -not -path "*/mock/*" \
        -not -path "*/mocks/*" \
        -not -path "*/node_modules/*" \
        -not -path "*/lib/*" \
        -not -path "*/dependencies/*" \
        | sort
}

# Get contract names from forge build artefacts
# Reads from out/ directory after forge build
find_contract_names() {
    local build_dir="${1:-$AUDIT_DIR}"
    if [ -d "$build_dir/out" ]; then
        find "$build_dir/out" -maxdepth 1 -type d -not -name "out" \
            | xargs -I{} basename {} .sol \
            | sort
    fi
}

# ── Pass status tracking ──────────────────────────────────────────────
PASS_STATUS_FILE="${WORKSPACE:-/tmp}/pipeline-status.json"

record_pass_status() {
    local pass="$1" status="$2" duration="$3"
    local timestamp
    timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    # Create or update the status file
    if [ ! -f "$PASS_STATUS_FILE" ]; then
        echo '{"passes":{}}' > "$PASS_STATUS_FILE"
    fi

    jq --arg pass "$pass" \
       --arg status "$status" \
       --arg duration "$duration" \
       --arg ts "$timestamp" \
       '.passes[$pass] = {status: $status, duration: ($duration | tonumber), completed: $ts}' \
       "$PASS_STATUS_FILE" > "${PASS_STATUS_FILE}.tmp" \
       && mv "${PASS_STATUS_FILE}.tmp" "$PASS_STATUS_FILE"
}

# ── Scope helpers ─────────────────────────────────────────────────────
# Graph-specific: known package paths
GRAPH_PACKAGES=(
    "packages/horizon"
    "packages/subgraph-service"
)

# Key contracts for focused analysis
GRAPH_CORE_CONTRACTS=(
    "HorizonStaking"
    "HorizonStakingExtension"
    "GraphPayments"
    "PaymentsEscrow"
    "GraphTallyCollector"
    "SubgraphService"
    "DisputeManager"
)

# Iterate over in-scope packages
for_each_package() {
    local callback="$1"
    shift
    for pkg in "${GRAPH_PACKAGES[@]}"; do
        local pkg_path="$AUDIT_DIR/$pkg"
        if [ -d "$pkg_path" ]; then
            "$callback" "$pkg_path" "$@"
        else
            log_warn "Package not found: $pkg"
        fi
    done
}
