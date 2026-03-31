#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
# BULWARK — Finding Merge & Deduplication Utility
# ════════════════════════════════════════════════════════════════════════
# Merges findings from multiple agents, deduplicates by dedup_hash,
# and resolves severity conflicts.
#
# Usage: merge-findings.sh <output.json> <input1.json> [input2.json] ...
# ════════════════════════════════════════════════════════════════════════

MERGE_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source common.sh if not already loaded (check for log_info function)
if ! declare -f log_info >/dev/null 2>&1; then
    source "$MERGE_SCRIPT_DIR/common.sh"
fi

if [ $# -lt 2 ]; then
    echo "Usage: merge-findings.sh <output.json> <input1.json> [input2.json] ..."
    exit 1
fi

OUTPUT="$1"
shift

log_info "Merging findings from $# sources..."

# Severity ranking for conflict resolution (higher = more severe)
# When same finding found by multiple agents, keep the highest severity
SEVERITY_RANK='{"Critical":5,"High":4,"Medium":3,"Low":2,"Informational":1}'

# Step 1: Concatenate all findings and compute dedup_hash where missing
MERGED=$(mktemp)
echo '[]' > "$MERGED"

TOTAL_INPUT=0
for input_file in "$@"; do
    if [ -f "$input_file" ] && [ "$(jq length "$input_file" 2>/dev/null)" -gt 0 ]; then
        count=$(jq length "$input_file")
        TOTAL_INPUT=$((TOTAL_INPUT + count))
        log_info "  $(basename "$input_file"): $count findings"

        # Add dedup_hash if missing, then append
        jq --slurpfile existing "$MERGED" '
            [.[] | . + (
                if .dedup_hash == null or .dedup_hash == "" then
                    {dedup_hash: ((.contract // "") + "|" + (.function // "") + "|" + (.property_violated // "") + "|" + (.title // "") | @base64)}
                else
                    {}
                end
            )] + $existing[0]
        ' "$input_file" > "${MERGED}.tmp" && mv "${MERGED}.tmp" "$MERGED"
    fi
done

log_info "Total input findings: $TOTAL_INPUT"

# Step 2: Deduplicate by dedup_hash, keeping highest severity
jq --argjson rank "$SEVERITY_RANK" '
    group_by(.dedup_hash)
    | [.[] | {
        group: .,
        sources: [.[].source] | unique,
        severities: [.[].severity] | unique,
        best: (sort_by(-($rank[.severity] // 0)) | first)
      }
      | .best + {
          found_by: .sources,
          severity_disagreements: (if (.severities | length) > 1 then .severities else null end)
        }
    ]
    | sort_by(-($rank[.severity] // 0))
    | to_entries
    | map(.value + {
        id: ("F-" + ((.key + 1) | tostring | if length < 3 then ("0" * (3 - length)) + . else . end))
      })
' "$MERGED" > "$OUTPUT" 2>/dev/null || {
    # Fallback: simpler dedup if complex jq fails
    log_warn "Complex dedup failed, using simple dedup"
    jq --argjson rank "$SEVERITY_RANK" '
        unique_by(.dedup_hash)
        | sort_by(-($rank[.severity] // 0))
    ' "$MERGED" > "$OUTPUT"
}

DEDUPED_COUNT=$(jq length "$OUTPUT" 2>/dev/null || echo "0")
DISAGREEMENTS=$(jq '[.[] | select(.severity_disagreements != null)] | length' "$OUTPUT" 2>/dev/null || echo "0")

log_ok "After deduplication: $DEDUPED_COUNT findings ($((TOTAL_INPUT - DEDUPED_COUNT)) duplicates removed)"
if [ "$DISAGREEMENTS" -gt 0 ]; then
    log_warn "Severity disagreements: $DISAGREEMENTS findings — flagged for Pass 6 review"
fi

rm -f "$MERGED"
