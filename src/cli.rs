use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "bulwark",
    about = "Multi-pass smart contract audit pipeline",
    version
)]
pub struct Cli {
    /// Path to configuration file
    #[arg(short, long, default_value = "bulwark.toml")]
    pub config: PathBuf,

    /// Audit target directory (overrides config)
    #[arg(short = 'd', long)]
    pub audit_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run the audit pipeline
    Run(RunArgs),

    /// Show pipeline status
    Status(StatusArgs),

    /// List and filter findings
    Findings(FindingsArgs),

    /// Validate workspace JSON against schemas
    Validate(ValidateArgs),

    /// Generate final report from existing workspace
    Report(ReportArgs),

    /// Initialise a new project directory scaffold
    Init(InitArgs),

    /// Show tool availability and configuration
    Doctor,

    /// Authenticate Claude Code (runs `claude login`)
    Login,
}

#[derive(clap::Args)]
pub struct RunArgs {
    /// Run only this pass (1-6)
    #[arg(long, value_parser = parse_pass_range)]
    pub pass: Option<PassRange>,

    /// Resume from this pass number
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=6))]
    pub resume: Option<u8>,

    /// Run only this agent in pass 2 (red, blue, gold)
    #[arg(long)]
    pub agent: Option<String>,
}

#[derive(clap::Args)]
pub struct StatusArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct FindingsArgs {
    /// Filter by severity (critical, high, medium, low)
    #[arg(long, short)]
    pub severity: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct ValidateArgs {
    /// Validate a specific file instead of entire workspace
    #[arg(long)]
    pub file: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct ReportArgs {
    /// Output format
    #[arg(long, default_value = "markdown")]
    pub format: String,
}

#[derive(clap::Args)]
pub struct InitArgs {
    /// Directory to create the project in (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Git repository URL to audit
    #[arg(long)]
    pub repo: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PassRange {
    pub start: u8,
    pub end: u8,
}

pub(crate) fn parse_pass_range(s: &str) -> Result<PassRange, String> {
    if let Some((start, end)) = s.split_once('-') {
        let start: u8 = start.parse().map_err(|_| format!("invalid pass number: {start}"))?;
        let end: u8 = end.parse().map_err(|_| format!("invalid pass number: {end}"))?;
        if !(1..=6).contains(&start) || !(1..=6).contains(&end) || start > end {
            return Err(format!("pass range must be 1-6, got {start}-{end}"));
        }
        Ok(PassRange { start, end })
    } else {
        let n: u8 = s.parse().map_err(|_| format!("invalid pass number: {s}"))?;
        if !(1..=6).contains(&n) {
            return Err(format!("pass number must be 1-6, got {n}"));
        }
        Ok(PassRange { start: n, end: n })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_pass_number() {
        let r = parse_pass_range("1").unwrap();
        assert_eq!(r.start, 1);
        assert_eq!(r.end, 1);
    }

    #[test]
    fn parse_single_pass_all_valid() {
        for n in 1..=6u8 {
            let r = parse_pass_range(&n.to_string()).unwrap();
            assert_eq!(r.start, n);
            assert_eq!(r.end, n);
        }
    }

    #[test]
    fn parse_pass_range_1_to_3() {
        let r = parse_pass_range("1-3").unwrap();
        assert_eq!(r.start, 1);
        assert_eq!(r.end, 3);
    }

    #[test]
    fn parse_pass_range_full() {
        let r = parse_pass_range("1-6").unwrap();
        assert_eq!(r.start, 1);
        assert_eq!(r.end, 6);
    }

    #[test]
    fn parse_pass_7_is_error() {
        assert!(parse_pass_range("7").is_err());
    }

    #[test]
    fn parse_pass_0_is_error() {
        assert!(parse_pass_range("0").is_err());
    }

    #[test]
    fn parse_pass_range_reversed_is_error() {
        assert!(parse_pass_range("3-1").is_err());
    }

    #[test]
    fn parse_pass_abc_is_error() {
        assert!(parse_pass_range("abc").is_err());
    }

    #[test]
    fn parse_pass_range_out_of_bounds_is_error() {
        assert!(parse_pass_range("0-7").is_err());
        assert!(parse_pass_range("1-7").is_err());
        assert!(parse_pass_range("0-3").is_err());
    }
}
