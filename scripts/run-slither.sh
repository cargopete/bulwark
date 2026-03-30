#!/usr/bin/env bash
set -euo pipefail

# Run Slither static analysis on Graph Protocol contracts.
# This runs WITHOUT Claude Code — pure static analysis.

AUDIT_DIR="/home/auditor/audits/graph-contracts"
OUTPUT_DIR="/home/auditor/audits/reports/slither-$(date +%Y%m%d-%H%M%S)"

mkdir -p "$OUTPUT_DIR"

echo "╔══════════════════════════════════════════════════════════╗"
echo "║  Slither Static Analysis — Graph Protocol Contracts      ║"
echo "╚══════════════════════════════════════════════════════════╝"
echo ""
echo "Output: $OUTPUT_DIR"
echo ""

cd "$AUDIT_DIR"

# ── Horizon package ──────────────────────────────────────────────────
if [ -d "packages/horizon" ]; then
    echo "━━━ Analysing packages/horizon ━━━"

    # Full Slither run — JSON output for machine parsing + human-readable
    slither packages/horizon/contracts/ \
        --json "$OUTPUT_DIR/horizon-full.json" \
        --sarif "$OUTPUT_DIR/horizon.sarif" \
        2>&1 | tee "$OUTPUT_DIR/horizon-stdout.txt" || true

    # Filtered: medium+ findings only
    echo ""
    echo "── Medium+ findings (Horizon) ──"
    if [ -f "$OUTPUT_DIR/horizon-full.json" ]; then
        jq -r '.results.detectors[] | select(.impact == "High" or .impact == "Medium") | "[\(.impact)] \(.check): \(.description[:120])"' \
            "$OUTPUT_DIR/horizon-full.json" 2>/dev/null | head -50 || echo "(no medium+ findings or JSON parse error)"
    fi
    echo ""
fi

# ── SubgraphService package ──────────────────────────────────────────
if [ -d "packages/subgraph-service" ]; then
    echo "━━━ Analysing packages/subgraph-service ━━━"

    slither packages/subgraph-service/contracts/ \
        --json "$OUTPUT_DIR/subgraph-service-full.json" \
        --sarif "$OUTPUT_DIR/subgraph-service.sarif" \
        2>&1 | tee "$OUTPUT_DIR/subgraph-service-stdout.txt" || true

    echo ""
    echo "── Medium+ findings (SubgraphService) ──"
    if [ -f "$OUTPUT_DIR/subgraph-service-full.json" ]; then
        jq -r '.results.detectors[] | select(.impact == "High" or .impact == "Medium") | "[\(.impact)] \(.check): \(.description[:120])"' \
            "$OUTPUT_DIR/subgraph-service-full.json" 2>/dev/null | head -50 || echo "(no medium+ findings or JSON parse error)"
    fi
    echo ""
fi

# ── Summary ──────────────────────────────────────────────────────────
echo "════════════════════════════════════════════════════════════"
echo "  Slither analysis complete."
echo "  Reports saved to: $OUTPUT_DIR"
echo ""
echo "  Files:"
ls -la "$OUTPUT_DIR/"
echo ""
echo "  Next steps:"
echo "    1. Review medium+ findings above"
echo "    2. Run: claude   (to start AI-assisted deep analysis)"
echo "    3. Or:  ./scripts/run-audit.sh  (full automated workflow)"
echo "════════════════════════════════════════════════════════════"
