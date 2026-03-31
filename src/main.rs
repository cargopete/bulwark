#![allow(dead_code)]
// Suppress collapsible_if: clippy suggests `let` chains which are unstable (rust#53667)
#![allow(clippy::collapsible_if)]

mod cli;
mod config;
mod docker;
mod error;
mod findings;
mod passes;
mod pipeline;
mod tools;
mod workspace;

use clap::Parser;
use cli::{Cli, Commands};
use config::Config;
use console::style;
use pipeline::checkpoint::PipelineStatus;
use pipeline::orchestrator::Orchestrator;
use std::path::PathBuf;
use workspace::Workspace;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialise tracing: BULWARK_LOG=debug for verbose, default is warn
    // User-facing output uses styled eprintln; tracing is for diagnostics.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BULWARK_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("bulwark=warn")),
        )
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    // Resolve config: CWD first, then $BULWARK_ROOT fallback
    let config_path = if cli.config.exists() {
        cli.config.clone()
    } else if let Ok(root) = std::env::var("BULWARK_ROOT") {
        let root_config = PathBuf::from(&root).join("bulwark.toml");
        if root_config.exists() { root_config } else { cli.config.clone() }
    } else {
        cli.config.clone()
    };

    let config = Config::load(&config_path)?;

    // Resolve audit directory: CLI flag > env var > config-relative
    let audit_dir = cli
        .audit_dir
        .or_else(|| std::env::var("AUDIT_DIR").ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            let bulwark_root = std::env::var("BULWARK_ROOT")
                .unwrap_or_else(|_| "/home/auditor".into());
            PathBuf::from(bulwark_root).join("audits/graph-contracts")
        });

    let bulwark_root = std::env::var("BULWARK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            cli.config
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf()
        });

    match cli.command {
        Commands::Run(args) => cmd_run(config, audit_dir, bulwark_root, args).await?,
        Commands::Status(args) => cmd_status(config, audit_dir, args)?,
        Commands::Findings(args) => cmd_findings(config, audit_dir, args)?,
        Commands::Validate(args) => cmd_validate(config, audit_dir, bulwark_root, args)?,
        Commands::Report(args) => cmd_report(config, audit_dir, args)?,
        Commands::Doctor => cmd_doctor(config)?,
        Commands::Login => cmd_login()?,
    }

    Ok(())
}

async fn cmd_run(
    config: Config,
    audit_dir: PathBuf,
    bulwark_root: PathBuf,
    args: cli::RunArgs,
) -> anyhow::Result<()> {
    let (start, end) = if let Some(range) = args.pass {
        (range.start, range.end)
    } else if let Some(resume) = args.resume {
        (resume, 6)
    } else {
        (1, 6)
    };

    let orchestrator = Orchestrator::new(config, audit_dir, bulwark_root, start, end, args.agent)?;
    let results = orchestrator.run().await?;

    // Exit with error code if any pass failed
    if results
        .iter()
        .any(|r| r.status == pipeline::pass::PassStatus::Failed)
    {
        std::process::exit(1);
    }

    Ok(())
}

fn cmd_status(config: Config, audit_dir: PathBuf, args: cli::StatusArgs) -> anyhow::Result<()> {
    let ws = Workspace::new(&audit_dir, &config.workspace.path);
    let status = PipelineStatus::load(&ws.pipeline_status_file())?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }

    if status.passes.is_empty() {
        eprintln!("No pipeline runs recorded yet.");
        eprintln!(
            "Run: bulwark run --pass 1    (to start with reconnaissance)"
        );
        return Ok(());
    }

    eprintln!("{}", style("Pipeline Status").bold());
    eprintln!();

    let pass_order = ["recon", "agents", "poc", "fuzzing", "formal", "review"];
    for (i, name) in pass_order.iter().enumerate() {
        if let Some(record) = status.passes.get(*name) {
            let status_str = match record.status {
                pipeline::pass::PassStatus::Completed => {
                    style("completed").green().to_string()
                }
                pipeline::pass::PassStatus::Failed => style("FAILED").red().to_string(),
                pipeline::pass::PassStatus::Skipped => {
                    style("skipped").yellow().to_string()
                }
                pipeline::pass::PassStatus::NotImplemented => {
                    style("not implemented").dim().to_string()
                }
            };
            eprintln!(
                "  Pass {}: {:<10} {} ({}s)",
                i + 1,
                name,
                status_str,
                record.duration
            );
        } else {
            eprintln!(
                "  Pass {}: {:<10} {}",
                i + 1,
                name,
                style("not run").dim()
            );
        }
    }

    // Show finding counts if available
    let merged = ws.merged_findings();
    if merged.exists() {
        if let Ok(content) = std::fs::read_to_string(&merged) {
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&content) {
                eprintln!();
                eprintln!("  Findings: {}", arr.len());
            }
        }
    }

    eprintln!();
    Ok(())
}

fn cmd_findings(
    config: Config,
    audit_dir: PathBuf,
    args: cli::FindingsArgs,
) -> anyhow::Result<()> {
    let ws = Workspace::new(&audit_dir, &config.workspace.path);
    let merged = ws.merged_findings();

    if !merged.exists() {
        eprintln!("No findings yet. Run Pass 2 first.");
        return Ok(());
    }

    let content = std::fs::read_to_string(&merged)?;
    let findings: Vec<findings::Finding> = serde_json::from_str(&content)?;

    // Filter by severity if requested
    let filtered: Vec<&findings::Finding> = if let Some(ref sev) = args.severity {
        let sev_lower = sev.to_lowercase();
        findings
            .iter()
            .filter(|f| f.severity.to_string().to_lowercase() == sev_lower)
            .collect()
    } else {
        findings.iter().collect()
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&filtered)?);
        return Ok(());
    }

    if filtered.is_empty() {
        eprintln!("No findings match the filter.");
        return Ok(());
    }

    eprintln!(
        "{} ({} findings)\n",
        style("Findings").bold(),
        filtered.len()
    );

    for f in &filtered {
        let sev_str = f.severity.to_string();
        let sev_style = match f.severity {
            findings::Severity::Critical => style(&sev_str).red().bold(),
            findings::Severity::High => style(&sev_str).red(),
            findings::Severity::Medium => style(&sev_str).yellow(),
            findings::Severity::Low => style(&sev_str).cyan(),
            findings::Severity::Informational => style(&sev_str).dim(),
        };

        eprintln!(
            "  {} [{}] {} — {}",
            style(&f.id).bold(),
            sev_style,
            f.contract,
            f.title
        );

        if let Some(ref prop) = f.property_violated {
            eprintln!("       Property: {prop}");
        }

        if !f.found_by.is_empty() {
            eprintln!("       Found by: {}", f.found_by.join(", "));
        }

        eprintln!();
    }

    Ok(())
}

fn cmd_validate(
    config: Config,
    audit_dir: PathBuf,
    bulwark_root: PathBuf,
    args: cli::ValidateArgs,
) -> anyhow::Result<()> {
    let ws = Workspace::new(&audit_dir, &config.workspace.path);
    let schemas_dir = bulwark_root.join(&config.schemas.dir);
    let finding_schema = schemas_dir.join("finding.schema.json");

    if !finding_schema.exists() {
        anyhow::bail!("Schema not found: {}", finding_schema.display());
    }

    if let Some(file) = args.file {
        eprintln!("Validating {}...", file.display());
        let content = std::fs::read_to_string(&file)?;
        let value: serde_json::Value = serde_json::from_str(&content)?;
        findings::schema::validate_findings_array(&value, &finding_schema)?;
        eprintln!("  {} Valid", style("✓").green());
        return Ok(());
    }

    // Validate all known output files
    let files_to_check = [
        ws.merged_findings(),
        ws.agent_raw_output("red"),
        ws.agent_raw_output("blue"),
        ws.agent_raw_output("gold"),
    ];

    let mut errors = 0;
    for file in &files_to_check {
        if !file.exists() {
            continue;
        }
        eprint!("  {}... ", file.display());
        let content = std::fs::read_to_string(file)?;
        let value: serde_json::Value = serde_json::from_str(&content)?;
        match findings::schema::validate_findings_array(&value, &finding_schema) {
            Ok(()) => eprintln!("{}", style("✓").green()),
            Err(e) => {
                eprintln!("{} {e}", style("✗").red());
                errors += 1;
            }
        }
    }

    if errors > 0 {
        anyhow::bail!("{errors} file(s) failed validation");
    }

    eprintln!("\n  {} All files valid", style("✓").green());
    Ok(())
}

fn cmd_report(
    config: Config,
    audit_dir: PathBuf,
    args: cli::ReportArgs,
) -> anyhow::Result<()> {
    let ws = Workspace::new(&audit_dir, &config.workspace.path);
    let bulwark_root = std::env::var("BULWARK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));

    let ctx = pipeline::pass::PipelineContext {
        config,
        workspace: ws,
        audit_dir,
        bulwark_root,
    };

    eprintln!(
        "{}",
        style("Assembling report from workspace data...").bold()
    );

    // Use the review module's report generation (skips adversarial review)
    let result = passes::review::generate_report(&ctx)?;

    match args.format.as_str() {
        "json" => {
            let json_path = ctx.workspace.final_report_json();
            if json_path.exists() {
                let content = std::fs::read_to_string(&json_path)?;
                println!("{content}");
            }
        }
        _ => {
            eprintln!(
                "  {} {}",
                style("✓").green(),
                result
            );
        }
    }

    Ok(())
}

fn cmd_doctor(config: Config) -> anyhow::Result<()> {
    let in_docker = docker::is_in_docker();
    let label = if in_docker { "container" } else { "host" };
    eprintln!("{}\n", style(format!("Bulwark Doctor ({label})")).bold());

    if !in_docker {
        // Host-side: check Docker
        let docker_ok = std::process::Command::new("docker")
            .arg("info")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        eprintln!(
            "  {} {:<12} {}",
            if docker_ok { style("✓").green() } else { style("✗").red() },
            "Docker",
            if docker_ok { "running" } else { "not available — install Docker Desktop" }
        );
        eprintln!();
        eprintln!(
            "  Audit tools live inside the Docker container.\n  \
             Run: docker compose run --rm audit-env bulwark doctor"
        );
        eprintln!();
        return Ok(());
    }

    // Inside container: show all tools
    let tools = ["forge", "slither", "claude", "halmos", "medusa", "echidna"];

    for tool in &tools {
        match config.resolve_tool(tool) {
            Ok(path) => eprintln!(
                "  {} {:<10} {}",
                style("✓").green(),
                tool,
                path.display()
            ),
            Err(_) => eprintln!(
                "  {} {:<10} not found",
                style("✗").red(),
                tool,
            ),
        }
    }

    eprintln!();

    // Check Claude auth
    let auth = tools::claude::check_auth();
    eprintln!(
        "  {} Claude auth: {}",
        if auth.is_authenticated() {
            style("✓").green()
        } else {
            style("⚠").yellow()
        },
        auth
    );

    if !auth.is_authenticated() {
        eprintln!(
            "\n  To authenticate: {}",
            style("bulwark login").bold()
        );
    }

    eprintln!();
    Ok(())
}

fn cmd_login() -> anyhow::Result<()> {
    // Find claude binary
    let claude = which::which("claude").map_err(|_| {
        anyhow::anyhow!("Claude Code not found. Are you inside the Docker container?")
    })?;

    eprintln!(
        "  {} Launching Claude login...\n",
        style("→").cyan()
    );

    let status = std::process::Command::new(claude)
        .arg("login")
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()?;

    if status.success() {
        eprintln!("\n  {} Claude authenticated successfully", style("✓").green());
    } else {
        eprintln!("\n  {} Login failed", style("✗").red());
    }

    Ok(())
}
