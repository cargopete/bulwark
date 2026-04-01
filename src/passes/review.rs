use crate::error::Result;

use crate::pipeline::pass::PipelineContext;
use crate::tools::claude::ClaudeSession;
use chrono::Utc;
use console::style;
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;

/// Pass 6: Adversarial Review & Final Report.
///
/// 1. Fresh Claude session challenges all pipeline output
/// 2. Assembles final report (JSON + Markdown)
pub async fn run(ctx: &PipelineContext) -> Result<String> {
    let review_dir = ctx.workspace.review_dir();
    let logs_dir = review_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;

    // ── Step 1: Adversarial review ──────────────────────────────────
    let review_data = run_adversarial_review(ctx, &review_dir, &logs_dir).await;

    // ── Step 2: Assemble all findings ───────────────────────────────
    eprintln!("  Assembling final report...");

    let validated = read_json_array(&ctx.workspace.validated_findings());
    let fuzz_findings = read_json_array(&ctx.workspace.fuzzing_dir().join("fuzzing-findings.json"));
    let formal_findings =
        read_json_array(&ctx.workspace.formal_dir().join("formal-findings.json"));

    let mut all_findings: Vec<Value> = Vec::new();
    all_findings.extend(validated);
    all_findings.extend(fuzz_findings);
    all_findings.extend(formal_findings);

    // Apply severity upgrades from adversarial review
    if let Some(ref review) = review_data {
        apply_review_adjustments(&mut all_findings, review, ctx);
    }

    // Sort by severity descending
    all_findings.sort_by(|a, b| {
        let sa = severity_rank(a.get("severity").and_then(|s| s.as_str()).unwrap_or(""));
        let sb = severity_rank(b.get("severity").and_then(|s| s.as_str()).unwrap_or(""));
        sb.cmp(&sa)
    });

    // ── Step 3: Write JSON report ───────────────────────────────────
    let recon_summary = read_json_object(&ctx.workspace.recon_summary());
    let verification =
        read_json_object(&ctx.workspace.formal_dir().join("verification-summary.json"));
    let pipeline_status = read_json_object(&ctx.workspace.pipeline_status_file());

    let json_report = json!({
        "generated": Utc::now().to_rfc3339(),
        "pipeline_status": pipeline_status,
        "recon_summary": recon_summary,
        "total_findings": all_findings.len(),
        "severity_breakdown": {
            "critical": count_severity(&all_findings, "Critical"),
            "high": count_severity(&all_findings, "High"),
            "medium": count_severity(&all_findings, "Medium"),
            "low": count_severity(&all_findings, "Low"),
        },
        "findings": all_findings,
        "verification": verification,
        "adversarial_review": review_data.unwrap_or(json!({})),
    });

    write_json(&ctx.workspace.final_report_json(), &json_report)?;
    eprintln!(
        "  {} Machine-readable report: {}",
        style("✓").green(),
        ctx.workspace.final_report_json().display()
    );

    // ── Step 4: Write Markdown report ───────────────────────────────
    let report_path = ctx.workspace.final_report_md();
    write_markdown_report(&report_path, &all_findings, &json_report, ctx)?;
    eprintln!(
        "  {} Markdown report: {}",
        style("✓").green(),
        report_path.display()
    );

    let critical = count_severity(&all_findings, "Critical");
    let high = count_severity(&all_findings, "High");
    let medium = count_severity(&all_findings, "Medium");
    let low = count_severity(&all_findings, "Low");

    Ok(format!(
        "{} findings — C:{critical} H:{high} M:{medium} L:{low}",
        all_findings.len()
    ))
}

/// Generate report from existing workspace data without running the adversarial review.
/// Used by the `bulwark report` subcommand.
pub fn generate_report(ctx: &PipelineContext) -> Result<String> {
    let review_dir = ctx.workspace.review_dir();
    std::fs::create_dir_all(&review_dir)?;

    // Load existing adversarial review if present
    let review_file = review_dir.join("adversarial-review.json");
    let review_data = if review_file.exists() {
        std::fs::read_to_string(&review_file)
            .ok()
            .and_then(|c| serde_json::from_str::<Value>(&c).ok())
    } else {
        None
    };

    // Assemble all findings
    let validated = read_json_array(&ctx.workspace.validated_findings());
    let fuzz_findings = read_json_array(&ctx.workspace.fuzzing_dir().join("fuzzing-findings.json"));
    let formal_findings =
        read_json_array(&ctx.workspace.formal_dir().join("formal-findings.json"));

    let mut all_findings: Vec<Value> = Vec::new();
    all_findings.extend(validated);
    all_findings.extend(fuzz_findings);
    all_findings.extend(formal_findings);

    // If no validated findings yet, fall back to merged findings
    if all_findings.is_empty() {
        all_findings = read_json_array(&ctx.workspace.merged_findings());
    }

    if let Some(ref review) = review_data {
        apply_review_adjustments(&mut all_findings, review, ctx);
    }

    all_findings.sort_by(|a, b| {
        let sa = severity_rank(a.get("severity").and_then(|s| s.as_str()).unwrap_or(""));
        let sb = severity_rank(b.get("severity").and_then(|s| s.as_str()).unwrap_or(""));
        sb.cmp(&sa)
    });

    let recon_summary = read_json_object(&ctx.workspace.recon_summary());
    let verification =
        read_json_object(&ctx.workspace.formal_dir().join("verification-summary.json"));
    let pipeline_status = read_json_object(&ctx.workspace.pipeline_status_file());

    let json_report = json!({
        "generated": Utc::now().to_rfc3339(),
        "pipeline_status": pipeline_status,
        "recon_summary": recon_summary,
        "total_findings": all_findings.len(),
        "severity_breakdown": {
            "critical": count_severity(&all_findings, "Critical"),
            "high": count_severity(&all_findings, "High"),
            "medium": count_severity(&all_findings, "Medium"),
            "low": count_severity(&all_findings, "Low"),
        },
        "findings": all_findings,
        "verification": verification,
        "adversarial_review": review_data.clone().unwrap_or(json!({})),
    });

    write_json(&ctx.workspace.final_report_json(), &json_report)?;

    let report_path = ctx.workspace.final_report_md();
    write_markdown_report(&report_path, &all_findings, &json_report, ctx)?;

    let critical = count_severity(&all_findings, "Critical");
    let high = count_severity(&all_findings, "High");
    let medium = count_severity(&all_findings, "Medium");
    let low = count_severity(&all_findings, "Low");

    Ok(format!(
        "Report written — {} findings (C:{critical} H:{high} M:{medium} L:{low})\n  JSON: {}\n  Markdown: {}",
        all_findings.len(),
        ctx.workspace.final_report_json().display(),
        report_path.display()
    ))
}

async fn run_adversarial_review(
    ctx: &PipelineContext,
    review_dir: &Path,
    logs_dir: &Path,
) -> Option<Value> {
    let prompts_dir = ctx.bulwark_root.join(&ctx.config.prompts.dir);
    let prompt_path = prompts_dir.join("adversarial-reviewer.md");

    if !prompt_path.exists() || !ctx.config.has_tool("claude") {
        eprintln!(
            "  {} Claude or prompt not available — skipping adversarial review",
            style("⚠").yellow()
        );
        return None;
    }

    let claude_bin = match ctx.config.resolve_tool("claude") {
        Ok(b) => b,
        Err(_) => return None,
    };

    let base_prompt = match std::fs::read_to_string(&prompt_path) {
        Ok(p) => p,
        Err(_) => return None,
    };

    let date = Utc::now().format("%Y-%m-%d");
    let prompt = format!(
        "{base_prompt}\n\n---\n\n## Pipeline Run Context\n\n\
         This audit was run on {date}. Read all the files listed in the \
         'Before You Start' section above. They are all present in the working directory.\n\n\
         Write your review to: audit-workspace/review/adversarial-review.json\n"
    );

    let session = ClaudeSession {
        claude_bin,
        prompt,
        max_turns: ctx.config.passes.review.max_turns,
        working_dir: ctx.audit_dir.clone(),
        log_file: logs_dir.join("adversarial-review.log"),
        model: Some(ctx.config.model.clone()),
        allowed_tools: vec![],
        timeout_minutes: Some(90),
    };

    let _ = session
        .run_with_spinner(&format!(
            "Adversarial review (max-turns={})...",
            ctx.config.passes.review.max_turns
        ))
        .await;

    let review_file = review_dir.join("adversarial-review.json");
    if review_file.exists() {
        if let Ok(content) = std::fs::read_to_string(&review_file) {
            if let Ok(data) = serde_json::from_str::<Value>(&content) {
                eprintln!("  {} Adversarial review complete", style("✓").green());

                // Print review summary
                let challenges = data
                    .pointer("/property_challenges")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter(|v| v.get("reviewer_verdict").and_then(|s| s.as_str()) == Some("CHALLENGE")).count())
                    .unwrap_or(0);
                let upgrades = data
                    .pointer("/severity_challenges")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let reinstated = data
                    .pointer("/reinstatements")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter(|v| v.get("verdict").and_then(|s| s.as_str()) == Some("REINSTATE")).count())
                    .unwrap_or(0);
                let compounds = data
                    .pointer("/compound_attacks")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let blind_spots = data
                    .pointer("/blind_spots")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);

                eprintln!("    Property challenges: {challenges}");
                eprintln!("    Severity upgrades proposed: {upgrades}");
                eprintln!("    Findings reinstated: {reinstated}");
                eprintln!("    Compound attacks: {compounds}");
                eprintln!("    Blind spots flagged: {blind_spots}");

                return Some(data);
            }
        }
        eprintln!(
            "  {} Adversarial review wrote invalid JSON",
            style("⚠").yellow()
        );
    } else {
        eprintln!(
            "  {} Adversarial review did not write output",
            style("⚠").yellow()
        );
    }

    None
}

fn apply_review_adjustments(
    findings: &mut Vec<Value>,
    review: &Value,
    ctx: &PipelineContext,
) {
    // Apply severity upgrades
    if let Some(upgrades) = review.get("severity_challenges").and_then(|v| v.as_array()) {
        for upgrade in upgrades {
            let Some(finding_id) = upgrade.get("finding_id").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(proposed) = upgrade.get("proposed_severity").and_then(|v| v.as_str()) else {
                continue;
            };

            for f in findings.iter_mut() {
                if f.get("id").and_then(|v| v.as_str()) == Some(finding_id) {
                    f["original_severity"] = f["severity"].clone();
                    f["severity"] = json!(proposed);
                    f["severity_upgraded_by"] = json!("adversarial_review");
                }
            }
        }
    }

    // Reinstate discarded findings
    if let Some(reinstatements) = review.get("reinstatements").and_then(|v| v.as_array()) {
        let reinstated_ids: Vec<&str> = reinstatements
            .iter()
            .filter(|r| r.get("verdict").and_then(|v| v.as_str()) == Some("REINSTATE"))
            .filter_map(|r| r.get("finding_id").and_then(|v| v.as_str()))
            .collect();

        if !reinstated_ids.is_empty() {
            let discarded = read_json_array(&ctx.workspace.discarded_findings());
            for df in discarded {
                let id = df.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
                if let Some(id) = id {
                    if reinstated_ids.contains(&id.as_str()) {
                        let mut reinstated = df;
                        reinstated["poc_status"] = json!("reinstated_by_review");
                        let justification = reinstatements
                            .iter()
                            .find(|r| r.get("finding_id").and_then(|v| v.as_str()) == Some(id.as_str()))
                            .and_then(|r| r.get("justification").and_then(|v| v.as_str()))
                            .unwrap_or("");
                        reinstated["reinstated_reason"] = json!(justification);
                        findings.push(reinstated);
                    }
                }
            }
        }
    }
}

fn write_markdown_report(
    path: &Path,
    findings: &[Value],
    json_report: &Value,
    ctx: &PipelineContext,
) -> Result<()> {
    let mut f = std::fs::File::create(path)?;

    let date = Utc::now().format("%Y-%m-%d");
    let total = findings.len();
    let critical = count_severity(findings, "Critical");
    let high = count_severity(findings, "High");
    let medium = count_severity(findings, "Medium");
    let low = count_severity(findings, "Low");

    writeln!(f, "# Bulwark Audit Report")?;
    writeln!(f)?;
    let target_name = ctx.config.target.repo
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .unwrap_or("unknown");
    writeln!(f, "**Target**: {target_name}")?;
    writeln!(f, "**Date**: {date}")?;
    writeln!(f, "**Pipeline**: 6-pass (Recon -> Multi-Agent -> PoC Gate -> Fuzzing -> Formal Verification -> Adversarial Review)")?;
    writeln!(f)?;
    writeln!(f, "---")?;
    writeln!(f)?;

    // Executive summary
    writeln!(f, "## Executive Summary")?;
    writeln!(f)?;

    let assessment = json_report
        .pointer("/adversarial_review/final_assessment")
        .and_then(|v| v.as_str());
    if let Some(text) = assessment {
        writeln!(f, "{text}")?;
    } else {
        writeln!(
            f,
            "Automated audit completed. {total} findings identified across all passes."
        )?;
    }
    writeln!(f)?;

    // Severity table
    writeln!(f, "## Findings Summary")?;
    writeln!(f)?;
    writeln!(f, "| Severity | Count |")?;
    writeln!(f, "|----------|-------|")?;
    writeln!(f, "| Critical | {critical} |")?;
    writeln!(f, "| High | {high} |")?;
    writeln!(f, "| Medium | {medium} |")?;
    writeln!(f, "| Low | {low} |")?;
    writeln!(f, "| **Total** | **{total}** |")?;
    writeln!(f)?;

    // Findings table
    if total > 0 {
        writeln!(f, "## Findings")?;
        writeln!(f)?;
        writeln!(f, "| ID | Severity | Title | Contract | PoC Status |")?;
        writeln!(f, "|----|----------|-------|----------|------------|")?;

        for finding in findings {
            let id = finding.get("id").and_then(|v| v.as_str()).unwrap_or("-");
            let sev = finding
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let title = finding
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let contract = finding
                .get("contract")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let poc = finding
                .get("poc_status")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            writeln!(f, "| {id} | {sev} | {title} | {contract} | {poc} |")?;
        }
        writeln!(f)?;

        // Detailed findings
        writeln!(f, "## Detailed Findings")?;
        writeln!(f)?;

        for finding in findings {
            let id = finding.get("id").and_then(|v| v.as_str()).unwrap_or("-");
            let title = finding
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let sev = finding
                .get("severity")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let confidence = finding
                .get("confidence")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let source = finding
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let contract = finding
                .get("contract")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let function = finding
                .get("function")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let prop = finding
                .get("property_violated")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let scenario = finding
                .get("attack_scenario")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let poc_status = finding
                .get("poc_status")
                .and_then(|v| v.as_str())
                .unwrap_or("-");

            writeln!(f, "### {id}: {title}")?;
            writeln!(f)?;
            writeln!(
                f,
                "**Severity**: {sev} | **Confidence**: {confidence} | **Source**: {source}"
            )?;
            writeln!(
                f,
                "**Contract**: {contract} | **Function**: {function}"
            )?;
            writeln!(f, "**Property violated**: {prop}")?;
            writeln!(f)?;
            writeln!(f, "**Attack scenario**: {scenario}")?;
            writeln!(f)?;
            writeln!(f, "**PoC status**: {poc_status}")?;
            writeln!(f)?;
            writeln!(f, "---")?;
            writeln!(f)?;
        }
    }

    // Property verification status
    let prop_file = ctx.workspace.property_verifications();
    if prop_file.exists() {
        let props = read_json_array(&prop_file);
        if !props.is_empty() {
            writeln!(f, "## Property Verification Status (BLUE Agent)")?;
            writeln!(f)?;
            writeln!(f, "| Property | Status |")?;
            writeln!(f, "|----------|--------|")?;
            for p in &props {
                let prop = p.get("property").and_then(|v| v.as_str()).unwrap_or("-");
                let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("-");
                writeln!(f, "| {prop} | {status} |")?;
            }
            writeln!(f)?;
        }
    }

    // Formal verification results
    let ver_file = ctx
        .workspace
        .formal_dir()
        .join("verification-summary.json");
    if ver_file.exists() {
        if let Some(ver) = read_json_object_opt(&ver_file) {
            if let Some(props) = ver.get("properties").and_then(|v| v.as_object()) {
                writeln!(f, "## Formal Verification (Halmos)")?;
                writeln!(f)?;
                writeln!(f, "| Property | Status | Duration | Bound |")?;
                writeln!(f, "|----------|--------|----------|-------|")?;
                for (prop, data) in props {
                    let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("-");
                    let dur = data.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
                    let bound = data.get("loop_bound").and_then(|v| v.as_u64());
                    let bound_str = bound.map_or("-".into(), |b| format!("loop={b}"));
                    writeln!(f, "| {prop} | {status} | {dur}s | {bound_str} |")?;
                }
                writeln!(f)?;
            }
        }
    }

    // Compound attacks
    if let Some(compounds) = json_report
        .pointer("/adversarial_review/compound_attacks")
        .and_then(|v| v.as_array())
    {
        if !compounds.is_empty() {
            writeln!(f, "## Compound Attack Scenarios")?;
            writeln!(f)?;
            for c in compounds {
                let title = c.get("title").and_then(|v| v.as_str()).unwrap_or("-");
                let sev = c
                    .get("combined_severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let scenario = c
                    .get("attack_scenario")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                writeln!(f, "### {title}")?;
                writeln!(f)?;
                writeln!(f, "**Severity**: {sev}")?;
                writeln!(f)?;
                writeln!(f, "{scenario}")?;
                writeln!(f)?;
            }
        }
    }

    // Pipeline metadata
    writeln!(f, "---")?;
    writeln!(f)?;
    writeln!(f, "## Pipeline Metadata")?;
    writeln!(f)?;

    let status_file = ctx.workspace.pipeline_status_file();
    if status_file.exists() {
        if let Some(status) = read_json_object_opt(&status_file) {
            if let Some(passes) = status.get("passes").and_then(|v| v.as_object()) {
                writeln!(f, "| Pass | Status | Duration |")?;
                writeln!(f, "|------|--------|----------|")?;
                for (name, data) in passes {
                    let s = data.get("status").and_then(|v| v.as_str()).unwrap_or("-");
                    let d = data.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
                    writeln!(f, "| {name} | {s} | {d}s |")?;
                }
                writeln!(f)?;
            }
        }
    }

    writeln!(
        f,
        "*Generated by [Bulwark](https://github.com/cargopete/bulwark) — multi-pass smart contract audit pipeline.*"
    )?;

    Ok(())
}

fn read_json_array(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

fn read_json_object(path: &Path) -> Value {
    read_json_object_opt(path).unwrap_or(json!({}))
}

fn read_json_object_opt(path: &Path) -> Option<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
}

fn count_severity(findings: &[Value], severity: &str) -> usize {
    findings
        .iter()
        .filter(|f| f.get("severity").and_then(|s| s.as_str()) == Some(severity))
        .count()
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "Critical" => 5,
        "High" => 4,
        "Medium" => 3,
        "Low" => 2,
        "Informational" => 1,
        _ => 0,
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let content = serde_json::to_string_pretty(value)?;
    std::fs::write(path, content)?;
    Ok(())
}
