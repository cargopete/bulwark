#!/usr/bin/env bash
set -euo pipefail

# Full AI audit workflow for Graph Protocol contracts.
# Runs static analysis first, then launches Claude Code with a structured audit prompt.
#
# Requires: ANTHROPIC_API_KEY set in environment.

AUDIT_DIR="/home/auditor/audits/graph-contracts"
REPORT_DIR="/home/auditor/audits/reports/full-audit-$(date +%Y%m%d-%H%M%S)"

mkdir -p "$REPORT_DIR"

echo "╔══════════════════════════════════════════════════════════╗"
echo "║  Full AI Audit Workflow — Graph Protocol                 ║"
echo "║  Phase 1: Static Analysis  →  Phase 2: AI Deep Audit    ║"
echo "╚══════════════════════════════════════════════════════════╝"
echo ""

# ── Preflight checks ────────────────────────────────────────────────
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "✗ ANTHROPIC_API_KEY not set. Cannot run AI audit phase."
    echo "  Set it in .env or: export ANTHROPIC_API_KEY=sk-ant-..."
    echo ""
    echo "  Running static analysis only..."
    echo ""
fi

cd "$AUDIT_DIR"

# ── Phase 1: Static Analysis (no AI needed) ──────────────────────────
echo "━━━ PHASE 1: Static Analysis ━━━"
echo ""

# 1a. Slither
echo "→ Running Slither..."
for pkg in packages/horizon packages/subgraph-service; do
    if [ -d "$pkg/contracts" ]; then
        pkg_name=$(basename "$pkg")
        echo "  Analysing $pkg_name..."
        slither "$pkg/contracts/" \
            --json "$REPORT_DIR/slither-${pkg_name}.json" \
            2>"$REPORT_DIR/slither-${pkg_name}-stderr.txt" || true

        # Extract summary
        if [ -f "$REPORT_DIR/slither-${pkg_name}.json" ]; then
            HIGH=$(jq '[.results.detectors[] | select(.impact == "High")] | length' "$REPORT_DIR/slither-${pkg_name}.json" 2>/dev/null || echo "?")
            MED=$(jq '[.results.detectors[] | select(.impact == "Medium")] | length' "$REPORT_DIR/slither-${pkg_name}.json" 2>/dev/null || echo "?")
            LOW=$(jq '[.results.detectors[] | select(.impact == "Low")] | length' "$REPORT_DIR/slither-${pkg_name}.json" 2>/dev/null || echo "?")
            echo "    $pkg_name: $HIGH high, $MED medium, $LOW low"
        fi
    fi
done
echo ""

# 1b. Entry point mapping (using grep — no AI needed)
echo "→ Mapping external entry points..."
ENTRY_POINTS="$REPORT_DIR/entry-points.txt"
echo "# External/Public State-Changing Functions" > "$ENTRY_POINTS"
echo "# Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$ENTRY_POINTS"
echo "" >> "$ENTRY_POINTS"

for pkg in packages/horizon packages/subgraph-service; do
    if [ -d "$pkg/contracts" ]; then
        echo "## $(basename "$pkg")" >> "$ENTRY_POINTS"
        echo "" >> "$ENTRY_POINTS"
        # Find external/public functions (rough grep — Claude will do proper analysis)
        find "$pkg/contracts" -name "*.sol" -not -path "*/test/*" -not -path "*/mock/*" | sort | while read -r sol_file; do
            matches=$(grep -nE '^\s+function\s+\w+\(' "$sol_file" | grep -E '(external|public)' | grep -vE '(view|pure)' || true)
            if [ -n "$matches" ]; then
                echo "### $sol_file" >> "$ENTRY_POINTS"
                echo "$matches" >> "$ENTRY_POINTS"
                echo "" >> "$ENTRY_POINTS"
            fi
        done
    fi
done
echo "  ✓ Entry points saved to $ENTRY_POINTS"
echo ""

# 1c. Storage layout check (if forge is available)
echo "→ Checking storage layouts..."
for pkg in packages/horizon packages/subgraph-service; do
    if [ -d "$pkg" ]; then
        pkg_name=$(basename "$pkg")
        (cd "$pkg" && forge inspect HorizonStaking storage-layout 2>/dev/null > "$REPORT_DIR/storage-${pkg_name}.txt") || true
    fi
done
echo "  ✓ Storage layouts saved"
echo ""

echo "━━━ Phase 1 Complete ━━━"
echo ""

# ── Phase 2: AI Deep Audit ───────────────────────────────────────────
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "Skipping Phase 2 (no API key)."
    echo ""
    echo "To run AI analysis manually:"
    echo "  export ANTHROPIC_API_KEY=sk-ant-..."
    echo "  claude"
    echo ""
    echo "Then paste the audit prompt from: /home/auditor/scripts/audit-prompt.txt"
    exit 0
fi

echo "━━━ PHASE 2: AI Deep Audit ━━━"
echo ""

# Generate the structured audit prompt
AUDIT_PROMPT="$REPORT_DIR/audit-prompt.md"
cat > "$AUDIT_PROMPT" << 'PROMPT'
# Audit Task: The Graph Protocol (Horizon) — Full Security Review

Read AUDIT_CONTEXT.md, PROPERTIES.md, and KNOWN_ISSUES.md before proceeding.

## Phase 1 results are available at:
Check the reports directory for Slither JSON output and entry point mapping.

## Your task — execute these steps in order:

### Step 1: Entry Point Analysis
Map all externally callable state-changing functions in packages/horizon/contracts/
and packages/subgraph-service/contracts/. Categorise by access level:
- Public (anyone can call)
- Role-restricted (onlyOperator, onlyAuthorizedVerifier, etc.)
- Admin (onlyGovernor, onlyArbitrator)

### Step 2: Review Slither Output
Parse the Slither JSON reports in the reports directory. For each medium+ finding:
- Is it a true positive or false positive?
- Is it in KNOWN_ISSUES.md? If so, skip.
- What is the actual impact?

### Step 3: Deep Audit — Critical Areas
Focus your analysis on these high-value targets (per KNOWN_ISSUES.md):

a) **Delegation pool math** — all division operations in HorizonStaking delegation:
   - Rounding direction (must favour protocol, never user)
   - First-depositor inflation attacks (donation attack on empty pool)
   - Accumulated rounding over many operations
   - Zero-share minting on small delegations

b) **Slashing-delegation interaction** — HorizonStaking.slash():
   - Provider-first ordering under all edge cases (P-10)
   - Slash amount == provider stake exactly
   - Concurrent slashes in same block
   - Interaction with thawing delegations (P-8)

c) **PaymentsEscrow race conditions**:
   - Thaw-then-collect ordering (P-18)
   - Partial collection during thawing
   - Re-deposit after thaw to reset state

d) **GraphTallyCollector RAV handling**:
   - RAV replay across data services (P-17)
   - valueAggregate monotonicity (P-16)
   - Signature verification completeness

e) **Operator invariant** (P-19):
   - Can any sequence of operator-callable functions extract value?
   - Operator scope isolation across data services (P-20)

### Step 4: Verify Each Finding
For every finding:
1. Trace the full call path from external entry point to vulnerable code
2. Confirm all require/assert statements can be satisfied
3. Verify the caller has necessary permissions
4. Write a 3-sentence attack scenario
5. Classify severity: Critical / High / Medium / Low

### Step 5: Generate PoCs
For each confirmed medium+ finding, write a Foundry test that demonstrates
the issue. Save to test/ directory.

### Step 6: Report
Produce a final report with:
- Executive summary (1 paragraph)
- Findings table (severity, title, contract, function, line)
- Detailed finding descriptions with PoCs
- Properties checked (reference PROPERTIES.md P-numbers)
- Known issues confirmed as still mitigated
PROMPT

echo "  Audit prompt generated: $AUDIT_PROMPT"
echo ""
echo "  Launching Claude Code with audit prompt..."
echo "  (This will be interactive — Claude will work through the steps)"
echo ""

# Launch Claude with the audit prompt piped in
# Using --print for non-interactive mode, or interactive if tty available
if [ -t 0 ]; then
    echo "  Interactive mode — Claude Code will guide you through the audit."
    echo "  Paste or reference: $AUDIT_PROMPT"
    echo ""
    cd "$AUDIT_DIR"
    exec claude
else
    echo "  Non-interactive mode — running audit prompt through Claude..."
    cd "$AUDIT_DIR"
    claude --print < "$AUDIT_PROMPT" | tee "$REPORT_DIR/ai-audit-output.md"
    echo ""
    echo "  AI audit output saved to: $REPORT_DIR/ai-audit-output.md"
fi
