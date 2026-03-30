#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
# DOYRAN — Pass 6: Adversarial Review & Final Report
# ════════════════════════════════════════════════════════════════════════
# Fresh Claude session reviews ALL pipeline output:
#   1. Challenge VERIFIED properties from BLUE agent
#   2. Challenge severity ratings (argue Medium → High)
#   3. Review discarded findings — reinstate legitimate hard-to-PoC issues
#   4. Cross-agent synthesis — compound attacks from overlapping findings
#   5. Identify fuzzing and verification blind spots
#
# Then assembles the final report (markdown + JSON).
# ════════════════════════════════════════════════════════════════════════

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib/common.sh"

PROMPTS_DIR="$DOYRAN_ROOT/prompts"
REVIEW_DIR="$WORKSPACE/review"
LOGS_DIR="$REVIEW_DIR/logs"
MAX_TURNS="${DOYRAN_REVIEW_MAX_TURNS:-60}"

mkdir -p "$REVIEW_DIR" "$LOGS_DIR"

timer_start
log_step "Pass 6: Adversarial Review"

# ── Step 1: Run adversarial review ────────────────────────────────────

if ! command -v claude >/dev/null 2>&1; then
    log_warn "Claude not available — skipping adversarial review"
    log_info "Proceeding directly to report assembly"
else
    log_info "Launching adversarial reviewer (max-turns=$MAX_TURNS)..."

    REVIEW_PROMPT="$(cat "$PROMPTS_DIR/adversarial-reviewer.md")

---

## Pipeline Run Context

This audit was run on $(date -u +%Y-%m-%d). Read all the files listed in the
'Before You Start' section above. They are all present in the working directory.

Write your review to: audit-workspace/review/adversarial-review.json
"

    cd "$AUDIT_DIR"
    claude -p "$REVIEW_PROMPT" \
        --max-turns "$MAX_TURNS" \
        > "$LOGS_DIR/adversarial-review.log" 2>&1 || true

    if [ -f "$REVIEW_DIR/adversarial-review.json" ]; then
        if jq empty "$REVIEW_DIR/adversarial-review.json" 2>/dev/null; then
            log_ok "Adversarial review complete"

            # Summary of review actions
            CHALLENGES=$(jq '[.property_challenges[]? | select(.reviewer_verdict == "CHALLENGE")] | length' \
                "$REVIEW_DIR/adversarial-review.json" 2>/dev/null || echo "0")
            UPGRADES=$(jq '[.severity_challenges[]?] | length' \
                "$REVIEW_DIR/adversarial-review.json" 2>/dev/null || echo "0")
            REINSTATED=$(jq '[.reinstatements[]? | select(.verdict == "REINSTATE")] | length' \
                "$REVIEW_DIR/adversarial-review.json" 2>/dev/null || echo "0")
            COMPOUNDS=$(jq '[.compound_attacks[]?] | length' \
                "$REVIEW_DIR/adversarial-review.json" 2>/dev/null || echo "0")
            BLINDSPOTS=$(jq '[.blind_spots[]?] | length' \
                "$REVIEW_DIR/adversarial-review.json" 2>/dev/null || echo "0")

            log_info "  Property challenges: $CHALLENGES"
            log_info "  Severity upgrades proposed: $UPGRADES"
            log_info "  Findings reinstated: $REINSTATED"
            log_info "  Compound attacks identified: $COMPOUNDS"
            log_info "  Blind spots flagged: $BLINDSPOTS"
        else
            log_warn "Adversarial review wrote invalid JSON"
        fi
    else
        log_warn "Adversarial review did not write output — check $LOGS_DIR/adversarial-review.log"
        echo '{}' > "$REVIEW_DIR/adversarial-review.json"
    fi
fi

# ── Step 2: Assemble final report (JSON) ──────────────────────────────

log_info "Assembling final report..."

# Collect all findings from all passes
VALIDATED="$(cat "$WORKSPACE/pocs/validated-findings.json" 2>/dev/null || echo '[]')"
FUZZ_FINDINGS="$(cat "$WORKSPACE/fuzzing/fuzzing-findings.json" 2>/dev/null || echo '[]')"
FORMAL_FINDINGS="$(cat "$WORKSPACE/formal/formal-findings.json" 2>/dev/null || echo '[]')"

# Merge all finding sources
ALL_FINDINGS=$(jq -n \
    --argjson validated "$VALIDATED" \
    --argjson fuzz "$FUZZ_FINDINGS" \
    --argjson formal "$FORMAL_FINDINGS" \
    '$validated + $fuzz + $formal')

# Apply severity upgrades from adversarial review
if [ -f "$REVIEW_DIR/adversarial-review.json" ]; then
    REVIEW_DATA=$(cat "$REVIEW_DIR/adversarial-review.json")

    # Apply severity upgrades
    UPGRADES=$(echo "$REVIEW_DATA" | jq '[.severity_challenges[]? | select(.proposed_severity != null)]' 2>/dev/null || echo '[]')
    if [ "$(echo "$UPGRADES" | jq length)" -gt 0 ]; then
        ALL_FINDINGS=$(echo "$ALL_FINDINGS" | jq --argjson upgrades "$UPGRADES" '
            [.[] | . as $f |
                ($upgrades | map(select(.finding_id == $f.id)) | first) as $upgrade |
                if $upgrade then
                    . + {
                        severity: $upgrade.proposed_severity,
                        severity_upgraded_by: "adversarial_review",
                        original_severity: .severity
                    }
                else .
                end
            ]
        ')
    fi

    # Reinstate discarded findings
    REINSTATED=$(echo "$REVIEW_DATA" | jq '[.reinstatements[]? | select(.verdict == "REINSTATE")]' 2>/dev/null || echo '[]')
    if [ "$(echo "$REINSTATED" | jq length)" -gt 0 ] && [ -f "$WORKSPACE/pocs/discarded-findings.json" ]; then
        DISCARDED=$(cat "$WORKSPACE/pocs/discarded-findings.json")
        REINSTATED_FINDINGS=$(echo "$DISCARDED" | jq --argjson reinstated "$REINSTATED" '
            [.[] | . as $f |
                ($reinstated | map(select(.finding_id == $f.id)) | first) as $r |
                if $r then
                    . + {poc_status: "reinstated_by_review", reinstated_reason: $r.justification}
                else
                    empty
                end
            ]
        ' 2>/dev/null || echo '[]')
        ALL_FINDINGS=$(echo "$ALL_FINDINGS" | jq --argjson reinstated "$REINSTATED_FINDINGS" '. + $reinstated')
    fi
fi

# Sort by severity
SEVERITY_RANK='{"Critical":5,"High":4,"Medium":3,"Low":2,"Informational":1}'
ALL_FINDINGS=$(echo "$ALL_FINDINGS" | jq --argjson rank "$SEVERITY_RANK" '
    sort_by(-($rank[.severity] // 0))
')

TOTAL_FINDINGS=$(echo "$ALL_FINDINGS" | jq length)

# Build the machine-readable report
jq -n \
    --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson findings "$ALL_FINDINGS" \
    --argjson recon "$(cat "$WORKSPACE/recon/recon-summary.json" 2>/dev/null || echo '{}')" \
    --argjson verification "$(cat "$WORKSPACE/formal/verification-summary.json" 2>/dev/null || echo '{}')" \
    --argjson review "$(cat "$REVIEW_DIR/adversarial-review.json" 2>/dev/null || echo '{}')" \
    --argjson pipeline "$(cat "$WORKSPACE/pipeline-status.json" 2>/dev/null || echo '{}')" \
    '{
        generated: $ts,
        pipeline_status: $pipeline,
        recon_summary: $recon,
        total_findings: ($findings | length),
        severity_breakdown: {
            critical: [$findings[] | select(.severity == "Critical")] | length,
            high: [$findings[] | select(.severity == "High")] | length,
            medium: [$findings[] | select(.severity == "Medium")] | length,
            low: [$findings[] | select(.severity == "Low")] | length
        },
        findings: $findings,
        verification: $verification,
        adversarial_review: $review
    }' > "$WORKSPACE/final-report.json"

log_ok "Machine-readable report: $WORKSPACE/final-report.json"

# ── Step 3: Assemble final report (Markdown) ─────────────────────────

REPORT="$WORKSPACE/final-report.md"

# Counts
CRITICAL=$(echo "$ALL_FINDINGS" | jq '[.[] | select(.severity == "Critical")] | length')
HIGH=$(echo "$ALL_FINDINGS" | jq '[.[] | select(.severity == "High")] | length')
MEDIUM=$(echo "$ALL_FINDINGS" | jq '[.[] | select(.severity == "Medium")] | length')
LOW=$(echo "$ALL_FINDINGS" | jq '[.[] | select(.severity == "Low")] | length')

cat > "$REPORT" << HEADER
# Doyran Audit Report

**Target**: The Graph Protocol (Horizon Contracts)
**Date**: $(date -u +%Y-%m-%d)
**Pipeline**: 6-pass (Recon → Multi-Agent → PoC Gate → Fuzzing → Formal Verification → Adversarial Review)

---

## Executive Summary

HEADER

# Add adversarial reviewer's final assessment if available
if [ -f "$REVIEW_DIR/adversarial-review.json" ]; then
    ASSESSMENT=$(jq -r '.final_assessment // empty' "$REVIEW_DIR/adversarial-review.json" 2>/dev/null || true)
    if [ -n "$ASSESSMENT" ]; then
        echo "$ASSESSMENT" >> "$REPORT"
    else
        echo "Automated audit completed. $TOTAL_FINDINGS findings identified across all passes." >> "$REPORT"
    fi
else
    echo "Automated audit completed. $TOTAL_FINDINGS findings identified across all passes." >> "$REPORT"
fi

cat >> "$REPORT" << SEVERITY

## Findings Summary

| Severity | Count |
|----------|-------|
| Critical | $CRITICAL |
| High | $HIGH |
| Medium | $MEDIUM |
| Low | $LOW |
| **Total** | **$TOTAL_FINDINGS** |

SEVERITY

# Findings table
if [ "$TOTAL_FINDINGS" -gt 0 ]; then
    echo "## Findings" >> "$REPORT"
    echo "" >> "$REPORT"
    echo "| ID | Severity | Title | Contract | PoC Status |" >> "$REPORT"
    echo "|----|----------|-------|----------|------------|" >> "$REPORT"

    echo "$ALL_FINDINGS" | jq -r '.[] | "| \(.id) | \(.severity) | \(.title) | \(.contract) | \(.poc_status) |"' >> "$REPORT"

    echo "" >> "$REPORT"

    # Detailed findings
    echo "## Detailed Findings" >> "$REPORT"
    echo "" >> "$REPORT"

    echo "$ALL_FINDINGS" | jq -r '.[] | "### \(.id): \(.title)\n\n**Severity**: \(.severity) | **Confidence**: \(.confidence // "—") | **Source**: \(.source)\n**Contract**: \(.contract) | **Function**: \(.function)\n**Property violated**: \(.property_violated // "—")\n\n**Attack scenario**: \(.attack_scenario)\n\n**PoC status**: \(.poc_status)\(.poc_file // "" | if . != "" then " (`\(.)`)" else "" end)\n\n---\n"' >> "$REPORT"
fi

# Property verification status
if [ -f "$WORKSPACE/findings/property-verifications.json" ]; then
    PROP_COUNT=$(jq 'length' "$WORKSPACE/findings/property-verifications.json" 2>/dev/null || echo "0")
    if [ "$PROP_COUNT" -gt 0 ]; then
        echo "## Property Verification Status (BLUE Agent)" >> "$REPORT"
        echo "" >> "$REPORT"
        echo "| Property | Status | Reviewer |" >> "$REPORT"
        echo "|----------|--------|----------|" >> "$REPORT"

        jq -r '.[] | "| \(.property) | \(.status) | — |"' \
            "$WORKSPACE/findings/property-verifications.json" >> "$REPORT" 2>/dev/null || true

        # Add reviewer challenges
        if [ -f "$REVIEW_DIR/adversarial-review.json" ]; then
            # This is a simplified view — the full data is in the JSON report
            CHALLENGE_COUNT=$(jq '[.property_challenges[]? | select(.reviewer_verdict == "CHALLENGE")] | length' \
                "$REVIEW_DIR/adversarial-review.json" 2>/dev/null || echo "0")
            if [ "$CHALLENGE_COUNT" -gt 0 ]; then
                echo "" >> "$REPORT"
                echo "**Note**: The adversarial reviewer challenged $CHALLENGE_COUNT property verification(s). See the JSON report for details." >> "$REPORT"
            fi
        fi
        echo "" >> "$REPORT"
    fi
fi

# Formal verification results
if [ -f "$WORKSPACE/formal/verification-summary.json" ]; then
    echo "## Formal Verification (Halmos)" >> "$REPORT"
    echo "" >> "$REPORT"
    echo "| Property | Status | Duration | Bound |" >> "$REPORT"
    echo "|----------|--------|----------|-------|" >> "$REPORT"

    jq -r '.properties | to_entries[] | "| \(.key) | \(.value.status) | \(.value.duration // 0)s | loop=\(.value.loop_bound // "—") |"' \
        "$WORKSPACE/formal/verification-summary.json" >> "$REPORT" 2>/dev/null || true
    echo "" >> "$REPORT"
fi

# Fuzzing results
if [ -f "$WORKSPACE/fuzzing/fuzzing-findings.json" ]; then
    FUZZ_COUNT=$(jq 'length' "$WORKSPACE/fuzzing/fuzzing-findings.json" 2>/dev/null || echo "0")
    echo "## Fuzzing Campaign" >> "$REPORT"
    echo "" >> "$REPORT"
    if [ "$FUZZ_COUNT" -gt 0 ]; then
        echo "**$FUZZ_COUNT broken invariant(s) detected.** See findings above." >> "$REPORT"
    else
        echo "All invariant tests held. No broken invariants detected." >> "$REPORT"
    fi
    echo "" >> "$REPORT"
fi

# Compound attacks from adversarial review
if [ -f "$REVIEW_DIR/adversarial-review.json" ]; then
    COMPOUND_COUNT=$(jq '[.compound_attacks[]?] | length' "$REVIEW_DIR/adversarial-review.json" 2>/dev/null || echo "0")
    if [ "$COMPOUND_COUNT" -gt 0 ]; then
        echo "## Compound Attack Scenarios" >> "$REPORT"
        echo "" >> "$REPORT"
        jq -r '.compound_attacks[]? | "### \(.title)\n\n**Severity**: \(.combined_severity) | **Findings combined**: \(.findings_combined | join(", "))\n\n\(.attack_scenario)\n"' \
            "$REVIEW_DIR/adversarial-review.json" >> "$REPORT" 2>/dev/null || true
    fi

    # Blind spots
    BLIND_COUNT=$(jq '[.blind_spots[]?] | length' "$REVIEW_DIR/adversarial-review.json" 2>/dev/null || echo "0")
    if [ "$BLIND_COUNT" -gt 0 ]; then
        echo "## Blind Spots" >> "$REPORT"
        echo "" >> "$REPORT"
        echo "Areas not fully covered by the automated pipeline:" >> "$REPORT"
        echo "" >> "$REPORT"
        jq -r '.blind_spots[]? | "- **\(.area)** (\(.risk) risk): \(.recommendation)"' \
            "$REVIEW_DIR/adversarial-review.json" >> "$REPORT" 2>/dev/null || true
        echo "" >> "$REPORT"
    fi
fi

# Pipeline metadata
cat >> "$REPORT" << 'FOOTER'
---

## Pipeline Metadata

FOOTER

if [ -f "$WORKSPACE/pipeline-status.json" ]; then
    echo "| Pass | Status | Duration |" >> "$REPORT"
    echo "|------|--------|----------|" >> "$REPORT"
    jq -r '.passes | to_entries[] | "| \(.key) | \(.value.status) | \(.value.duration)s |"' \
        "$WORKSPACE/pipeline-status.json" >> "$REPORT" 2>/dev/null || true
    echo "" >> "$REPORT"
fi

echo "*Generated by [Doyran](https://github.com/cargopete/doyran) — multi-pass smart contract audit pipeline.*" >> "$REPORT"

log_ok "Markdown report: $WORKSPACE/final-report.md"

# ── Summary ──────────────────────────────────────────────────────────

DURATION=$(timer_elapsed)
record_pass_status "review" "completed" "$DURATION"

echo ""
log_step "Pass 6 Complete ($(timer_human))"
echo ""
log_ok "Total findings: $TOTAL_FINDINGS"
log_ok "  Critical: $CRITICAL"
log_ok "  High:     $HIGH"
log_ok "  Medium:   $MEDIUM"
log_ok "  Low:      $LOW"
echo ""
log_info "Reports:"
log_info "  $WORKSPACE/final-report.md   (human-readable)"
log_info "  $WORKSPACE/final-report.json (machine-readable)"
log_info "  $REVIEW_DIR/adversarial-review.json (review details)"
echo ""
