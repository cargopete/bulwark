use super::{Finding, Severity};
use base64::Engine;
use std::collections::HashMap;

/// Merge findings from multiple agent sources, deduplicate by hash,
/// resolve severity conflicts (keep highest), and assign sequential IDs.
pub fn merge_and_dedup(sources: Vec<(&str, Vec<Finding>)>) -> MergeResult {
    let total_input: usize = sources.iter().map(|(_, f)| f.len()).sum();

    // Flatten all findings with computed dedup hashes
    let mut all_findings: Vec<Finding> = Vec::with_capacity(total_input);
    for (_source_name, findings) in sources {
        for mut finding in findings {
            if finding.dedup_hash.is_empty() {
                finding.dedup_hash = compute_dedup_hash(&finding);
            }
            all_findings.push(finding);
        }
    }

    // Group by dedup_hash
    let mut groups: HashMap<String, Vec<Finding>> = HashMap::new();
    for finding in all_findings {
        groups
            .entry(finding.dedup_hash.clone())
            .or_default()
            .push(finding);
    }

    // Deduplicate: keep highest severity, track sources and disagreements
    let mut deduped: Vec<Finding> = groups
        .into_values()
        .map(|group| {
            let sources: Vec<String> = group.iter().map(|f| f.source.clone()).collect();
            let unique_sources: Vec<String> = {
                let mut s = sources.clone();
                s.sort();
                s.dedup();
                s
            };

            let severities: Vec<Severity> = {
                let mut s: Vec<Severity> = group.iter().map(|f| f.severity).collect();
                s.sort();
                s.dedup();
                s
            };

            // Pick the finding with highest severity
            let mut best = group
                .into_iter()
                .max_by_key(|f| f.severity.rank())
                .expect("group is non-empty");

            best.found_by = unique_sources;

            if severities.len() > 1 {
                best.severity_disagreements = Some(severities);
            }

            best
        })
        .collect();

    // Sort by severity descending, then by title for stability
    deduped.sort_by(|a, b| {
        b.severity
            .rank()
            .cmp(&a.severity.rank())
            .then_with(|| a.title.cmp(&b.title))
    });

    // Assign sequential IDs
    for (i, finding) in deduped.iter_mut().enumerate() {
        finding.id = format!("F-{:03}", i + 1);
    }

    let duplicates_removed = total_input - deduped.len();
    let disagreements = deduped
        .iter()
        .filter(|f| f.severity_disagreements.is_some())
        .count();

    MergeResult {
        findings: deduped,
        total_input,
        duplicates_removed,
        severity_disagreements: disagreements,
    }
}

pub struct MergeResult {
    pub findings: Vec<Finding>,
    pub total_input: usize,
    pub duplicates_removed: usize,
    pub severity_disagreements: usize,
}

/// Compute dedup hash matching the bash approach:
/// base64(contract|function|property_violated|title)
fn compute_dedup_hash(finding: &Finding) -> String {
    let input = format!(
        "{}|{}|{}|{}",
        finding.contract,
        finding.function,
        finding.property_violated.as_deref().unwrap_or(""),
        finding.title,
    );
    base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::{Confidence, PocStatus};

    fn make_finding(title: &str, severity: Severity, contract: &str, hash: &str) -> Finding {
        Finding {
            id: String::new(),
            source: "test".into(),
            severity,
            confidence: Confidence::High,
            title: title.into(),
            contract: contract.into(),
            function: "test()".into(),
            lines: vec![],
            property_violated: None,
            attack_scenario: "test scenario".into(),
            poc_file: None,
            poc_status: PocStatus::NotAttempted,
            dedup_hash: hash.into(),
            found_by: vec![],
            severity_disagreements: None,
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn single_source_no_duplicates_preserves_all_with_sequential_ids() {
        let findings = vec![
            make_finding("Bug A", Severity::High, "Token", "hash_a"),
            make_finding("Bug B", Severity::Medium, "Staking", "hash_b"),
            make_finding("Bug C", Severity::Low, "Rewards", "hash_c"),
        ];

        let result = merge_and_dedup(vec![("red", findings)]);

        assert_eq!(result.findings.len(), 3);
        assert_eq!(result.total_input, 3);
        assert_eq!(result.duplicates_removed, 0);
        assert_eq!(result.severity_disagreements, 0);

        // IDs should be sequential, sorted by severity descending
        assert_eq!(result.findings[0].id, "F-001");
        assert_eq!(result.findings[1].id, "F-002");
        assert_eq!(result.findings[2].id, "F-003");

        // Highest severity first
        assert_eq!(result.findings[0].severity, Severity::High);
        assert_eq!(result.findings[1].severity, Severity::Medium);
        assert_eq!(result.findings[2].severity, Severity::Low);
    }

    #[test]
    fn two_sources_overlapping_hash_deduplicates_highest_severity_wins() {
        let red_findings = vec![
            make_finding("Reentrancy", Severity::Medium, "Token", "hash_dup"),
        ];
        let blue_findings = vec![
            make_finding("Reentrancy", Severity::High, "Token", "hash_dup"),
        ];

        let result = merge_and_dedup(vec![
            ("red", red_findings),
            ("blue", blue_findings),
        ]);

        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.total_input, 2);
        assert_eq!(result.duplicates_removed, 1);

        // Highest severity wins
        assert_eq!(result.findings[0].severity, Severity::High);
    }

    #[test]
    fn severity_disagreements_tracked_when_agents_disagree() {
        let red_findings = vec![
            make_finding("Bug X", Severity::Low, "Token", "hash_x"),
        ];
        let blue_findings = vec![
            make_finding("Bug X", Severity::Critical, "Token", "hash_x"),
        ];

        let result = merge_and_dedup(vec![
            ("red", red_findings),
            ("blue", blue_findings),
        ]);

        assert_eq!(result.severity_disagreements, 1);
        let finding = &result.findings[0];
        assert!(finding.severity_disagreements.is_some());
        let disagreements = finding.severity_disagreements.as_ref().unwrap();
        assert!(disagreements.contains(&Severity::Low));
        assert!(disagreements.contains(&Severity::Critical));
    }

    #[test]
    fn found_by_populated_correctly_after_merge() {
        let mut red_f = make_finding("Bug", Severity::High, "Token", "hash_fb");
        red_f.source = "red".into();
        let mut blue_f = make_finding("Bug", Severity::High, "Token", "hash_fb");
        blue_f.source = "blue".into();

        let result = merge_and_dedup(vec![
            ("red", vec![red_f]),
            ("blue", vec![blue_f]),
        ]);

        let finding = &result.findings[0];
        assert!(finding.found_by.contains(&"blue".to_string()));
        assert!(finding.found_by.contains(&"red".to_string()));
        assert_eq!(finding.found_by.len(), 2);
    }

    #[test]
    fn empty_input_returns_empty_output_zero_stats() {
        let result = merge_and_dedup(vec![]);

        assert!(result.findings.is_empty());
        assert_eq!(result.total_input, 0);
        assert_eq!(result.duplicates_removed, 0);
        assert_eq!(result.severity_disagreements, 0);
    }

    #[test]
    fn empty_sources_return_empty_output() {
        let result = merge_and_dedup(vec![("red", vec![]), ("blue", vec![])]);

        assert!(result.findings.is_empty());
        assert_eq!(result.total_input, 0);
        assert_eq!(result.duplicates_removed, 0);
    }

    #[test]
    fn auto_computed_dedup_hash_when_empty() {
        // Two findings with empty dedup_hash but same contract/function/title
        // should be deduplicated because their computed hashes will match
        let f1 = make_finding("Same Bug", Severity::Low, "Token", "");
        let f2 = make_finding("Same Bug", Severity::High, "Token", "");

        let result = merge_and_dedup(vec![
            ("red", vec![f1]),
            ("blue", vec![f2]),
        ]);

        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.duplicates_removed, 1);
        // Hash should have been computed and should not be empty
        assert!(!result.findings[0].dedup_hash.is_empty());
    }

    #[test]
    fn findings_sorted_by_severity_desc_then_title_asc() {
        let findings = vec![
            make_finding("Zebra", Severity::Medium, "A", "h1"),
            make_finding("Alpha", Severity::Medium, "A", "h2"),
            make_finding("Beta", Severity::Critical, "A", "h3"),
        ];

        let result = merge_and_dedup(vec![("red", findings)]);

        assert_eq!(result.findings[0].title, "Beta"); // Critical
        assert_eq!(result.findings[1].title, "Alpha"); // Medium, alphabetically first
        assert_eq!(result.findings[2].title, "Zebra"); // Medium, alphabetically second
    }
}
