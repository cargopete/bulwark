//! Run-wide dollar budget for Claude sessions, recorded in `cost.json`.

use crate::error::{BulwarkError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The CLI checks its cap only after each response, so one response can overshoot.
/// It gets this fraction of the session's cap; the ledger reserves the whole cap.
pub const CLI_CAP_FRACTION: f64 = 0.85;

/// Below this a session cannot do useful work, so it is not started.
pub const MIN_SESSION_USD: f64 = 0.25;

/// Pass order and each pass's share of the money. Unspent money flows to later passes.
pub const PASS_SHARES: &[(&str, f64)] = &[
    ("recon", 0.05),
    ("agents", 0.35),
    ("poc", 0.40),
    ("fuzzing", 0.07),
    ("formal", 0.05),
    ("review", 0.08),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub label: String,
    pub model: Option<String>,
    pub cost_usd: f64,
    /// False when the CLI reported no cost and the session's cap was charged instead.
    pub measured: bool,
    pub cap_usd: Option<f64>,
    pub turns: Option<u64>,
    pub outcome: String,
    pub at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LedgerFile {
    max_usd: Option<f64>,
    entries: Vec<Entry>,
}

pub struct Ledger {
    max_usd: Option<f64>,
    path: PathBuf,
    file: Mutex<LedgerFile>,
}

impl Ledger {
    /// A `fresh` ledger starts a new run; otherwise earlier spend in `path` counts.
    pub fn open(path: PathBuf, max_usd: Option<f64>, fresh: bool) -> Result<Arc<Self>> {
        let mut file = if !fresh && path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&path)?)?
        } else {
            LedgerFile::default()
        };
        file.max_usd = max_usd;
        Ok(Arc::new(Self {
            max_usd,
            path,
            file: Mutex::new(file),
        }))
    }

    pub fn spent(&self) -> f64 {
        self.lock().entries.iter().map(|e| e.cost_usd).sum()
    }

    pub fn remaining(&self) -> Option<f64> {
        self.max_usd.map(|m| (m - self.spent()).max(0.0))
    }

    pub fn record(&self, entry: Entry) -> Result<()> {
        let mut file = self.lock();
        file.entries.push(entry);
        self.save(&file)
    }

    /// Charge a session its whole cap before it starts, so a session that never
    /// reports back (killed, timed out, dropped) still counts.
    pub fn reserve(&self, label: &str, model: Option<String>, cap: Option<f64>) -> Result<usize> {
        let mut file = self.lock();
        file.entries.push(Entry {
            label: label.to_string(),
            model,
            cost_usd: cap.unwrap_or(0.0),
            measured: false,
            cap_usd: cap,
            turns: None,
            outcome: "running".into(),
            at: chrono::Utc::now().to_rfc3339(),
        });
        self.save(&file)?;
        Ok(file.entries.len() - 1)
    }

    /// Replace a reservation with what the session reported; no report keeps the cap.
    pub fn settle(
        &self,
        idx: usize,
        cost: Option<f64>,
        turns: Option<u64>,
        outcome: &str,
    ) -> Result<()> {
        let mut file = self.lock();
        if let Some(e) = file.entries.get_mut(idx) {
            if let Some(c) = cost {
                e.cost_usd = c;
                e.measured = true;
            }
            e.turns = turns;
            e.outcome = outcome.to_string();
        }
        self.save(&file)
    }

    fn save(&self, file: &LedgerFile) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(file)?)?;
        Ok(())
    }

    /// Money for `pass`: what remains, times its share of the passes still to run.
    pub fn allowance(self: &Arc<Self>, pass: &str, enabled: impl Fn(&str) -> bool) -> Allowance {
        let cap = self.remaining().map(|left| {
            let to_run: Vec<_> = PASS_SHARES
                .iter()
                .skip_while(|(p, _)| *p != pass)
                .filter(|(p, _)| *p == pass || enabled(p))
                .collect();
            let total: f64 = to_run.iter().map(|(_, s)| s).sum();
            let own = to_run.first().map_or(0.0, |(_, s)| *s);
            if total > 0.0 {
                left * own / total
            } else {
                left
            }
        });
        Allowance {
            ledger: self.clone(),
            cap,
            base: self.spent(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerFile> {
        self.file.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A pass's slice of the run budget; `None` everywhere means unlimited.
#[derive(Clone)]
pub struct Allowance {
    ledger: Arc<Ledger>,
    cap: Option<f64>,
    base: f64,
}

impl Allowance {
    pub fn left(&self) -> Option<f64> {
        self.cap
            .map(|c| (c - (self.ledger.spent() - self.base)).max(0.0))
            .map(|l| l.min(self.ledger.remaining().unwrap_or(l)))
    }

    /// An equal part of what is left, for one of `parts` sessions yet to start.
    pub fn split(&self, parts: usize) -> Option<f64> {
        self.left().map(|l| l / parts.max(1) as f64)
    }

    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }
}

/// Refuse a session whose cap cannot buy useful work.
pub fn check_cap(cap: Option<f64>) -> Result<()> {
    match cap {
        Some(c) if c < MIN_SESSION_USD => Err(BulwarkError::BudgetExhausted { left: c }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(cost: f64) -> Entry {
        Entry {
            label: "t".into(),
            model: None,
            cost_usd: cost,
            measured: true,
            cap_usd: None,
            turns: None,
            outcome: "success".into(),
            at: String::new(),
        }
    }

    fn ledger(max: Option<f64>) -> (tempfile::TempDir, Arc<Ledger>) {
        let dir = tempfile::tempdir().unwrap();
        let l = Ledger::open(dir.path().join("cost.json"), max, true).unwrap();
        (dir, l)
    }

    #[test]
    fn shares_of_a_full_run_add_up_to_the_budget() {
        let (_d, l) = ledger(Some(20.0));
        let a = l.allowance("recon", |_| true);
        assert!((a.left().unwrap() - 1.0).abs() < 1e-9);
        let a = l.allowance("poc", |_| true);
        // poc's 0.40 over poc..review's 0.60 of the full 20.
        assert!((a.left().unwrap() - 20.0 * 0.40 / 0.60).abs() < 1e-9);
    }

    #[test]
    fn underspend_rolls_forward_and_disabled_passes_release_money() {
        let (_d, l) = ledger(Some(20.0));
        l.record(entry(2.0)).unwrap();
        let review_only = l.allowance("review", |p| p == "review");
        assert!((review_only.left().unwrap() - 18.0).abs() < 1e-9);
        let poc_no_formal = l.allowance("poc", |p| p != "formal");
        assert!((poc_no_formal.left().unwrap() - 18.0 * 0.40 / 0.55).abs() < 1e-9);
    }

    #[test]
    fn allowance_shrinks_as_its_sessions_spend() {
        let (_d, l) = ledger(Some(10.0));
        let a = l.allowance("review", |_| true);
        assert_eq!(a.left(), Some(10.0));
        l.record(entry(3.0)).unwrap();
        assert_eq!(a.left(), Some(7.0));
        assert_eq!(a.split(2), Some(3.5));
        l.record(entry(9.0)).unwrap();
        assert_eq!(a.left(), Some(0.0));
    }

    #[test]
    fn unlimited_means_no_caps() {
        let (_d, l) = ledger(None);
        let a = l.allowance("poc", |_| true);
        assert_eq!(a.left(), None);
        assert!(check_cap(a.split(3)).is_ok());
    }

    #[test]
    fn tiny_cap_is_refused() {
        assert!(check_cap(Some(0.10)).is_err());
        assert!(check_cap(Some(1.0)).is_ok());
    }

    #[test]
    fn reservation_counts_until_settled() {
        let (_d, l) = ledger(Some(10.0));
        let i = l.reserve("agent:red", None, Some(4.0)).unwrap();
        assert_eq!(l.remaining(), Some(6.0));
        l.settle(i, Some(1.5), Some(12), "success").unwrap();
        assert_eq!(l.remaining(), Some(8.5));
        let j = l.reserve("poc:F-001", None, Some(2.0)).unwrap();
        l.settle(j, None, None, "no_result").unwrap();
        assert_eq!(l.remaining(), Some(6.5));
    }

    #[test]
    fn resumed_run_counts_earlier_spend() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cost.json");
        Ledger::open(path.clone(), Some(20.0), true)
            .unwrap()
            .record(entry(5.0))
            .unwrap();
        let resumed = Ledger::open(path.clone(), Some(20.0), false).unwrap();
        assert_eq!(resumed.remaining(), Some(15.0));
        let fresh = Ledger::open(path, Some(20.0), true).unwrap();
        assert_eq!(fresh.remaining(), Some(20.0));
    }
}
