#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
# BULWARK — Pass 1: Reconnaissance
# ════════════════════════════════════════════════════════════════════════
# Entirely deterministic. No AI. Produces the structural foundation
# that all subsequent passes consume.
#
# Outputs (all in audit-workspace/recon/):
#   entry-points.json       — external/public state-changing functions
#   storage-layouts.json    — per-contract storage slot assignments
#   slither-results.json    — full Slither detector results
#   dependency-graph.json   — inheritance and call relationships
#   math-operations.json    — arithmetic operations in sensitive code
#   access-control.json     — modifier and role mappings
#   proxy-mappings.json     — proxy-to-implementation relationships
#   recon-summary.json      — combined summary for downstream passes
# ════════════════════════════════════════════════════════════════════════

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib/common.sh"

RECON_DIR="$WORKSPACE/recon"
mkdir -p "$RECON_DIR"

timer_start
log_step "Pass 1: Reconnaissance"

# ── 1.1 Compile contracts ──────────────────────────────────────────────
log_info "Compiling contracts..."

COMPILE_OK=true
COMPILER_VERSION=""

for pkg in "${GRAPH_PACKAGES[@]}"; do
    pkg_path="$AUDIT_DIR/$pkg"
    if [ -d "$pkg_path" ]; then
        pkg_name=$(basename "$pkg")
        log_info "  Building $pkg_name..."
        if (cd "$pkg_path" && forge build --force 2>"$RECON_DIR/build-${pkg_name}-stderr.txt"); then
            log_ok "  $pkg_name compiled"
            # Capture compiler version from build output
            if [ -z "$COMPILER_VERSION" ]; then
                COMPILER_VERSION=$(cd "$pkg_path" && forge config --json 2>/dev/null | jq -r '.solc // "unknown"' || echo "unknown")
            fi
        else
            log_error "  $pkg_name build failed — see $RECON_DIR/build-${pkg_name}-stderr.txt"
            COMPILE_OK=false
        fi
    fi
done

# ── 1.2 Solidity version check ────────────────────────────────────────
log_info "Checking Solidity versions..."

PRAGMA_FILE="$RECON_DIR/pragma-versions.json"
echo '[]' > "$PRAGMA_FILE"

for pkg in "${GRAPH_PACKAGES[@]}"; do
    pkg_path="$AUDIT_DIR/$pkg"
    if [ -d "$pkg_path/contracts" ]; then
        while IFS= read -r sol_file; do
            pragma=$(grep -E '^\s*pragma\s+solidity' "$sol_file" | head -1 | sed 's/.*pragma solidity\s*//;s/\s*;.*//' || true)
            if [ -n "$pragma" ]; then
                rel_path="${sol_file#$AUDIT_DIR/}"
                jq --arg file "$rel_path" --arg ver "$pragma" \
                    '. + [{file: $file, version: $ver}]' "$PRAGMA_FILE" > "${PRAGMA_FILE}.tmp" \
                    && mv "${PRAGMA_FILE}.tmp" "$PRAGMA_FILE"
            fi
        done < <(find_sol_files "$pkg_path/contracts")
    fi
done

log_ok "Found $(jq length "$PRAGMA_FILE") files with pragma declarations"

# ── 1.3 Static analysis (Slither) ─────────────────────────────────────
log_info "Running Slither static analysis..."

SLITHER_COMBINED="$RECON_DIR/slither-results.json"
echo '{"packages":{}}' > "$SLITHER_COMBINED"

for pkg in "${GRAPH_PACKAGES[@]}"; do
    pkg_path="$AUDIT_DIR/$pkg"
    if [ -d "$pkg_path/contracts" ]; then
        pkg_name=$(basename "$pkg")
        log_info "  Analysing $pkg_name..."

        slither_out="$RECON_DIR/slither-${pkg_name}-raw.json"

        if slither "$pkg_path/contracts/" \
            --json "$slither_out" \
            2>"$RECON_DIR/slither-${pkg_name}-stderr.txt"; then
            log_ok "  Slither completed for $pkg_name"
        else
            # Slither returns non-zero when it finds issues — that's normal
            if [ -f "$slither_out" ]; then
                log_ok "  Slither completed for $pkg_name (findings detected)"
            else
                log_warn "  Slither failed for $pkg_name — check stderr"
            fi
        fi

        # Merge into combined output
        if [ -f "$slither_out" ]; then
            jq --arg pkg "$pkg_name" \
                --slurpfile raw "$slither_out" \
                '.packages[$pkg] = $raw[0]' \
                "$SLITHER_COMBINED" > "${SLITHER_COMBINED}.tmp" \
                && mv "${SLITHER_COMBINED}.tmp" "$SLITHER_COMBINED"
        fi
    fi
done

# Extract Slither summary counts
SLITHER_SUMMARY=$(jq '{
    high: [.packages[].results.detectors[]? | select(.impact == "High")] | length,
    medium: [.packages[].results.detectors[]? | select(.impact == "Medium")] | length,
    low: [.packages[].results.detectors[]? | select(.impact == "Low")] | length,
    informational: [.packages[].results.detectors[]? | select(.impact == "Informational")] | length,
    optimization: [.packages[].results.detectors[]? | select(.impact == "Optimization")] | length
}' "$SLITHER_COMBINED" 2>/dev/null || echo '{"high":0,"medium":0,"low":0,"informational":0,"optimization":0}')

log_ok "Slither summary: $(echo "$SLITHER_SUMMARY" | jq -c .)"

# ── 1.4 Entry point mapping ───────────────────────────────────────────
log_info "Mapping entry points..."

ENTRY_POINTS="$RECON_DIR/entry-points.json"
echo '{}' > "$ENTRY_POINTS"

for pkg in "${GRAPH_PACKAGES[@]}"; do
    pkg_path="$AUDIT_DIR/$pkg"
    if [ -d "$pkg_path" ]; then
        pkg_name=$(basename "$pkg")

        # Use forge inspect to get ABI for each core contract
        for contract in "${GRAPH_CORE_CONTRACTS[@]}"; do
            abi_json=$(cd "$pkg_path" && forge inspect "$contract" abi 2>/dev/null || true)

            if [ -n "$abi_json" ] && [ "$abi_json" != "null" ]; then
                # Extract external/public state-changing functions from ABI
                functions=$(echo "$abi_json" | jq '[
                    .[] | select(.type == "function")
                    | select(.stateMutability != "view" and .stateMutability != "pure")
                    | {
                        name: .name,
                        visibility: "external",
                        mutability: .stateMutability,
                        inputs: [.inputs[] | {name: .name, type: .type}],
                        outputs: [.outputs[]? | {name: .name, type: .type}]
                    }
                ]' 2>/dev/null || echo '[]')

                # Find the source file for this contract
                source_file=$(find "$pkg_path/contracts" -name "${contract}.sol" 2>/dev/null | head -1 || true)
                rel_source="${source_file#$AUDIT_DIR/}"

                # Add modifier info by grep (ABI doesn't include modifiers)
                if [ -n "$source_file" ] && [ -f "$source_file" ]; then
                    # For each function, try to find its modifiers
                    functions=$(echo "$functions" | jq --arg file "$source_file" '
                        [.[] | . as $fn |
                            .modifiers = (
                                # placeholder — populated by modifier grep below
                                []
                            )
                        ]
                    ')
                fi

                # Build the contract entry
                jq --arg contract "$contract" \
                   --arg file "$rel_source" \
                   --arg pkg "$pkg_name" \
                   --argjson fns "$functions" \
                   '.[$contract] = {file: $file, package: $pkg, functions: $fns}' \
                   "$ENTRY_POINTS" > "${ENTRY_POINTS}.tmp" \
                   && mv "${ENTRY_POINTS}.tmp" "$ENTRY_POINTS"

                log_info "  $contract: $(echo "$functions" | jq length) state-changing functions"
            fi
        done
    fi
done

# Enrich entry points with modifier information from source
log_info "  Enriching with modifier data..."
for pkg in "${GRAPH_PACKAGES[@]}"; do
    pkg_path="$AUDIT_DIR/$pkg"
    if [ -d "$pkg_path/contracts" ]; then
        while IFS= read -r sol_file; do
            # Extract function signatures with their modifiers
            # Match: function name(...) visibility modifiers {
            grep -nE '^\s+function\s+\w+\(' "$sol_file" 2>/dev/null | while IFS= read -r line; do
                line_num=$(echo "$line" | cut -d: -f1)
                # Get the full function declaration (may span multiple lines)
                func_name=$(echo "$line" | grep -oP 'function\s+\K\w+' || true)
                if [ -n "$func_name" ]; then
                    # Read up to 5 lines to capture modifiers
                    func_block=$(sed -n "${line_num},$((line_num + 5))p" "$sol_file" | tr '\n' ' ')
                    # Extract modifiers (words between ) and { that aren't keywords)
                    modifiers=$(echo "$func_block" | grep -oP '\)\s+\K[^{]+' | \
                        grep -oP '\b(?!external|public|internal|private|view|pure|payable|virtual|override|returns)\w+\b' | \
                        grep -v '^$' | tr '\n' ',' | sed 's/,$//' || true)

                    if [ -n "$modifiers" ]; then
                        # Update the entry points JSON with modifier data
                        rel_file="${sol_file#$AUDIT_DIR/}"
                        for contract_name in $(jq -r 'keys[]' "$ENTRY_POINTS"); do
                            contract_file=$(jq -r --arg c "$contract_name" '.[$c].file // ""' "$ENTRY_POINTS")
                            if [ "$contract_file" = "$rel_file" ]; then
                                IFS=',' read -ra mod_array <<< "$modifiers"
                                mod_json=$(printf '%s\n' "${mod_array[@]}" | jq -R . | jq -s .)
                                jq --arg c "$contract_name" \
                                   --arg fn "$func_name" \
                                   --argjson mods "$mod_json" \
                                   'if .[$c].functions then
                                        .[$c].functions = [.[$c].functions[] |
                                            if .name == $fn then .modifiers = $mods else . end
                                        ]
                                    else . end' \
                                   "$ENTRY_POINTS" > "${ENTRY_POINTS}.tmp" \
                                   && mv "${ENTRY_POINTS}.tmp" "$ENTRY_POINTS"
                            fi
                        done
                    fi
                fi
            done
        done < <(find_sol_files "$pkg_path/contracts")
    fi
done

TOTAL_FUNCTIONS=$(jq '[.[].functions | length] | add // 0' "$ENTRY_POINTS")
log_ok "Mapped $TOTAL_FUNCTIONS state-changing entry points across $(jq 'keys | length' "$ENTRY_POINTS") contracts"

# ── 1.5 Storage layouts ──────────────────────────────────────────────
log_info "Extracting storage layouts..."

STORAGE_FILE="$RECON_DIR/storage-layouts.json"
echo '{}' > "$STORAGE_FILE"

for pkg in "${GRAPH_PACKAGES[@]}"; do
    pkg_path="$AUDIT_DIR/$pkg"
    if [ -d "$pkg_path" ]; then
        for contract in "${GRAPH_CORE_CONTRACTS[@]}"; do
            layout=$(cd "$pkg_path" && forge inspect "$contract" storage-layout --json 2>/dev/null || true)
            if [ -n "$layout" ] && [ "$layout" != "null" ]; then
                # Check for storage gaps
                gap_count=$(echo "$layout" | jq '[.storage[]? | select(.label | startswith("__gap"))] | length' 2>/dev/null || echo "0")

                jq --arg contract "$contract" \
                   --argjson layout "$layout" \
                   --argjson gaps "$gap_count" \
                   '.[$contract] = {layout: $layout, gap_count: $gaps}' \
                   "$STORAGE_FILE" > "${STORAGE_FILE}.tmp" \
                   && mv "${STORAGE_FILE}.tmp" "$STORAGE_FILE"

                slot_count=$(echo "$layout" | jq '.storage | length' 2>/dev/null || echo "?")
                log_info "  $contract: $slot_count slots, $gap_count gaps"
            fi
        done
    fi
done

log_ok "Storage layouts for $(jq 'keys | length' "$STORAGE_FILE") contracts"

# ── 1.6 Dependency / call graph ──────────────────────────────────────
log_info "Building dependency graph..."

DEP_FILE="$RECON_DIR/dependency-graph.json"
echo '{"inheritance":{},"external_calls":{}}' > "$DEP_FILE"

for pkg in "${GRAPH_PACKAGES[@]}"; do
    pkg_path="$AUDIT_DIR/$pkg"
    if [ -d "$pkg_path/contracts" ]; then
        pkg_name=$(basename "$pkg")

        # Use Slither's inheritance printer
        slither "$pkg_path/contracts/" \
            --print inheritance-graph \
            2>"$RECON_DIR/dep-${pkg_name}-stderr.txt" || true

        # Parse inheritance from source (more reliable than Slither printer for JSON)
        while IFS= read -r sol_file; do
            rel_file="${sol_file#$AUDIT_DIR/}"
            # Extract: contract Foo is Bar, Baz, Qux {
            inheritance=$(grep -oP 'contract\s+\w+\s+is\s+\K[^{]+' "$sol_file" | \
                tr ',' '\n' | sed 's/^\s*//;s/\s*$//' | grep -v '^$' || true)

            if [ -n "$inheritance" ]; then
                contract_name=$(grep -oP 'contract\s+\K\w+' "$sol_file" | head -1)
                parents_json=$(echo "$inheritance" | jq -R . | jq -s .)
                jq --arg c "$contract_name" --argjson parents "$parents_json" \
                    '.inheritance[$c] = $parents' \
                    "$DEP_FILE" > "${DEP_FILE}.tmp" \
                    && mv "${DEP_FILE}.tmp" "$DEP_FILE"
            fi

            # Extract external calls (interface calls, contract.function() patterns)
            external_calls=$(grep -nE '\.\w+\(' "$sol_file" | \
                grep -vE '(msg\.|block\.|tx\.|abi\.|this\.|super\.|require\(|assert\(|emit )' | \
                grep -vE '^\s*//' || true)

            if [ -n "$external_calls" ]; then
                contract_name=$(grep -oP 'contract\s+\K\w+' "$sol_file" | head -1)
                calls_json=$(echo "$external_calls" | head -50 | jq -R . | jq -s .)
                jq --arg c "$contract_name" --argjson calls "$calls_json" \
                    '.external_calls[$c] = $calls' \
                    "$DEP_FILE" > "${DEP_FILE}.tmp" \
                    && mv "${DEP_FILE}.tmp" "$DEP_FILE"
            fi
        done < <(find_sol_files "$pkg_path/contracts")
    fi
done

log_ok "Dependency graph: $(jq '.inheritance | keys | length' "$DEP_FILE") contracts with inheritance data"

# ── 1.7 Graph-specific: Access control / role enumeration ─────────────
log_info "Enumerating access control roles..."

ACCESS_FILE="$RECON_DIR/access-control.json"
echo '{"modifiers":{},"roles":{}}' > "$ACCESS_FILE"

for pkg in "${GRAPH_PACKAGES[@]}"; do
    pkg_path="$AUDIT_DIR/$pkg"
    if [ -d "$pkg_path/contracts" ]; then
        while IFS= read -r sol_file; do
            rel_file="${sol_file#$AUDIT_DIR/}"

            # Find modifier definitions
            grep -nE '^\s+modifier\s+\w+' "$sol_file" 2>/dev/null | while IFS= read -r line; do
                mod_name=$(echo "$line" | grep -oP 'modifier\s+\K\w+' || true)
                line_num=$(echo "$line" | cut -d: -f1)
                if [ -n "$mod_name" ]; then
                    jq --arg file "$rel_file" --arg mod "$mod_name" --arg line "$line_num" \
                        '.modifiers[$mod] = (.modifiers[$mod] // []) + [{file: $file, line: ($line | tonumber)}]' \
                        "$ACCESS_FILE" > "${ACCESS_FILE}.tmp" \
                        && mv "${ACCESS_FILE}.tmp" "$ACCESS_FILE"
                fi
            done

            # Find role-like constants (bytes32 public constant ROLE_NAME = keccak256(...))
            grep -nE '(bytes32|uint256).*\b(ROLE|role|Role)\b' "$sol_file" 2>/dev/null | while IFS= read -r line; do
                role_name=$(echo "$line" | grep -oP '\b\w*(ROLE|role|Role)\w*\b' | head -1 || true)
                if [ -n "$role_name" ]; then
                    jq --arg file "$rel_file" --arg role "$role_name" \
                        '.roles[$role] = (.roles[$role] // []) + [$file]' \
                        "$ACCESS_FILE" > "${ACCESS_FILE}.tmp" \
                        && mv "${ACCESS_FILE}.tmp" "$ACCESS_FILE"
                fi
            done

            # Find onlyX modifier patterns used on functions
            grep -nE 'only\w+' "$sol_file" 2>/dev/null | grep -vE '^\s*//' | while IFS= read -r line; do
                mod_name=$(echo "$line" | grep -oP '\bonly\w+\b' | head -1 || true)
                if [ -n "$mod_name" ]; then
                    jq --arg file "$rel_file" --arg mod "$mod_name" \
                        '.modifiers[$mod] = (.modifiers[$mod] // []) + [{file: $file}]' \
                        "$ACCESS_FILE" > "${ACCESS_FILE}.tmp" \
                        && mv "${ACCESS_FILE}.tmp" "$ACCESS_FILE"
                fi
            done
        done < <(find_sol_files "$pkg_path/contracts")
    fi
done

# Deduplicate modifier entries
jq '.modifiers |= (to_entries | map(.value |= unique) | from_entries)' \
    "$ACCESS_FILE" > "${ACCESS_FILE}.tmp" && mv "${ACCESS_FILE}.tmp" "$ACCESS_FILE"

log_ok "Access control: $(jq '.modifiers | keys | length' "$ACCESS_FILE") modifiers, $(jq '.roles | keys | length' "$ACCESS_FILE") roles"

# ── 1.8 Graph-specific: Mathematical operations inventory ─────────────
log_info "Inventorying arithmetic operations in sensitive contracts..."

MATH_FILE="$RECON_DIR/math-operations.json"
echo '{}' > "$MATH_FILE"

# Focus on contracts with known math sensitivity
MATH_SENSITIVE_CONTRACTS=(
    "HorizonStaking"
    "HorizonStakingExtension"
    "GraphPayments"
    "PaymentsEscrow"
)

for pkg in "${GRAPH_PACKAGES[@]}"; do
    pkg_path="$AUDIT_DIR/$pkg"
    if [ -d "$pkg_path/contracts" ]; then
        for contract in "${MATH_SENSITIVE_CONTRACTS[@]}"; do
            sol_file=$(find "$pkg_path/contracts" -name "${contract}.sol" 2>/dev/null | head -1 || true)
            if [ -n "$sol_file" ] && [ -f "$sol_file" ]; then
                rel_file="${sol_file#$AUDIT_DIR/}"

                # Find division, multiplication, modulo operations
                ops=$(grep -nE '[^/](/[^//*]|%[^=]|\*[^*/=]|\.mul\(|\.div\(|\.mod\(|mulDiv|FullMath)' "$sol_file" 2>/dev/null | \
                    grep -vE '^\s*//' | head -100 || true)

                if [ -n "$ops" ]; then
                    ops_json=$(echo "$ops" | while IFS= read -r line; do
                        line_num=$(echo "$line" | cut -d: -f1)
                        content=$(echo "$line" | cut -d: -f2- | sed 's/^\s*//' | head -c 200)
                        # Classify the operation type
                        op_type="unknown"
                        echo "$content" | grep -qE '/' && op_type="division"
                        echo "$content" | grep -qE '\*' && op_type="multiplication"
                        echo "$content" | grep -qE '%' && op_type="modulo"
                        echo "$content" | grep -qiE 'mulDiv|FullMath' && op_type="mulDiv"
                        printf '{"line":%s,"operation":"%s","context":"%s"}\n' \
                            "$line_num" "$op_type" "$(echo "$content" | sed 's/"/\\"/g')"
                    done | jq -s '.')

                    jq --arg contract "$contract" \
                       --arg file "$rel_file" \
                       --argjson ops "$ops_json" \
                       '.[$contract] = {file: $file, operations: $ops}' \
                       "$MATH_FILE" > "${MATH_FILE}.tmp" \
                       && mv "${MATH_FILE}.tmp" "$MATH_FILE"

                    log_info "  $contract: $(echo "$ops_json" | jq length) arithmetic operations"
                fi
            fi
        done
    fi
done

log_ok "Math inventory complete"

# ── 1.9 Graph-specific: Proxy-implementation mappings ─────────────────
log_info "Identifying proxy-implementation relationships..."

PROXY_FILE="$RECON_DIR/proxy-mappings.json"
echo '[]' > "$PROXY_FILE"

for pkg in "${GRAPH_PACKAGES[@]}"; do
    pkg_path="$AUDIT_DIR/$pkg"
    if [ -d "$pkg_path/contracts" ]; then
        # Find proxy-related patterns
        while IFS= read -r sol_file; do
            rel_file="${sol_file#$AUDIT_DIR/}"

            # Check for proxy patterns: TransparentUpgradeableProxy, UUPSUpgradeable, delegatecall
            if grep -qE '(TransparentUpgradeableProxy|UUPSUpgradeable|ERC1967|delegatecall|_implementation)' "$sol_file" 2>/dev/null; then
                contract_name=$(grep -oP 'contract\s+\K\w+' "$sol_file" | head -1 || true)
                # Try to identify if it's a proxy or implementation
                proxy_type="unknown"
                grep -qE 'TransparentUpgradeableProxy' "$sol_file" && proxy_type="transparent"
                grep -qE 'UUPSUpgradeable' "$sol_file" && proxy_type="uups"
                grep -qE 'delegatecall' "$sol_file" && proxy_type="custom-delegatecall"

                # Find what it delegates to
                impl=$(grep -oP '_implementation\(\)|implementation\(\)' "$sol_file" | head -1 || true)

                jq --arg contract "$contract_name" \
                   --arg file "$rel_file" \
                   --arg type "$proxy_type" \
                   '. + [{contract: $contract, file: $file, proxy_type: $type}]' \
                   "$PROXY_FILE" > "${PROXY_FILE}.tmp" \
                   && mv "${PROXY_FILE}.tmp" "$PROXY_FILE"
            fi
        done < <(find_sol_files "$pkg_path/contracts")
    fi
done

log_ok "Found $(jq length "$PROXY_FILE") proxy-related contracts"

# ── 1.10 Assemble recon summary ──────────────────────────────────────
log_info "Assembling recon summary..."

DURATION=$(timer_elapsed)

jq -n \
    --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson duration "$DURATION" \
    --argjson compiled "$( [ "$COMPILE_OK" = true ] && echo true || echo false )" \
    --arg compiler "$COMPILER_VERSION" \
    --argjson slither_summary "$SLITHER_SUMMARY" \
    --argjson entry_point_count "$TOTAL_FUNCTIONS" \
    --argjson storage_contracts "$(jq 'keys | length' "$STORAGE_FILE")" \
    --argjson proxy_count "$(jq length "$PROXY_FILE")" \
    --argjson math_contracts "$(jq 'keys | length' "$MATH_FILE")" \
    --argjson access_modifiers "$(jq '.modifiers | keys | length' "$ACCESS_FILE")" \
    '{
        generated: $ts,
        pass: "recon",
        duration_seconds: $duration,
        contracts_compiled: $compiled,
        compiler_version: $compiler,
        slither_summary: $slither_summary,
        entry_point_count: $entry_point_count,
        storage_contracts: $storage_contracts,
        proxy_contracts: $proxy_count,
        math_operations_contracts: $math_contracts,
        access_control_modifiers: $access_modifiers,
        artefacts: [
            "recon/entry-points.json",
            "recon/storage-layouts.json",
            "recon/slither-results.json",
            "recon/dependency-graph.json",
            "recon/math-operations.json",
            "recon/access-control.json",
            "recon/proxy-mappings.json",
            "recon/pragma-versions.json"
        ]
    }' > "$RECON_DIR/recon-summary.json"

record_pass_status "recon" "completed" "$DURATION"

# ── Print summary ─────────────────────────────────────────────────────
echo ""
log_step "Pass 1 Complete ($(timer_human))"
echo ""
log_ok "Compilation:    $( [ "$COMPILE_OK" = true ] && echo "success (solc $COMPILER_VERSION)" || echo "FAILED" )"
log_ok "Slither:        $(echo "$SLITHER_SUMMARY" | jq -r '"H:\(.high) M:\(.medium) L:\(.low)"')"
log_ok "Entry points:   $TOTAL_FUNCTIONS state-changing functions"
log_ok "Storage:        $(jq 'keys | length' "$STORAGE_FILE") contracts inspected"
log_ok "Proxies:        $(jq length "$PROXY_FILE") proxy-related contracts"
log_ok "Math ops:       $(jq '[.[].operations | length] | add // 0' "$MATH_FILE") operations inventoried"
log_ok "Access control: $(jq '.modifiers | keys | length' "$ACCESS_FILE") modifiers found"
echo ""
log_info "Recon artefacts: $RECON_DIR/"
echo ""
