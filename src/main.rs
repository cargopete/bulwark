#![allow(dead_code)]
// Suppress collapsible_if: clippy suggests `let` chains which are unstable (rust#53667)
#![allow(clippy::collapsible_if)]

mod bench;
mod budget;
mod cli;
mod config;
mod coverage;
mod dashboard;
mod docker;
mod error;
mod findings;
mod known;
mod passes;
mod pin;
mod pipeline;
mod program;
mod program_import;
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

    // init creates bulwark.toml, so it cannot wait for one to load.
    if let Commands::Init(args) = cli.command {
        return cmd_init(args);
    }
    if let Commands::Bench(cmd) = cli.command {
        return cmd_bench(cmd).await;
    }
    if let Commands::Dashboard(args) = &cli.command {
        if let Some(root) = &args.workspace {
            // A config, when one is at hand, tells the dashboard which files each agent owes.
            let scope = [
                Some(cli.config.clone()),
                std::env::var("BULWARK_ROOT")
                    .ok()
                    .map(|r| PathBuf::from(r).join("bulwark.toml")),
            ]
            .into_iter()
            .flatten()
            .find(|p| p.exists())
            .and_then(|p| Config::load(&p).ok())
            .map(|c| dashboard::status::Scope::from_config(&c))
            .unwrap_or_default();
            return dashboard::serve(Workspace::new(root, ""), scope, &args.bind, args.port).await;
        }
    }

    // Resolve config: CWD first, then $BULWARK_ROOT fallback
    let config_path = if cli.config.exists() {
        cli.config.clone()
    } else if let Ok(root) = std::env::var("BULWARK_ROOT") {
        let root_config = PathBuf::from(&root).join("bulwark.toml");
        if root_config.exists() {
            root_config
        } else {
            cli.config.clone()
        }
    } else {
        cli.config.clone()
    };

    let config = Config::load(&config_path)?;

    // Resolve audit directory: CLI flag > env var > derive from repo URL in config
    let audit_dir = cli
        .audit_dir
        .or_else(|| std::env::var("AUDIT_DIR").ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            // Audits live under BULWARK_INSTALL (the tool dir), not BULWARK_ROOT (the project dir).
            // BULWARK_ROOT is the per-project config dir and should not accumulate audit outputs.
            let base = std::env::var("BULWARK_INSTALL")
                .or_else(|_| std::env::var("BULWARK_ROOT"))
                .unwrap_or_else(|_| "/home/auditor".into());
            // Derive a name from the repo URL (last path component, strip .git)
            let repo_name = config
                .target
                .repo
                .trim_end_matches(".git")
                .rsplit('/')
                .next()
                .unwrap_or("audit")
                .to_string();
            PathBuf::from(base).join("audits").join(repo_name)
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
        Commands::Init(_) | Commands::Bench(_) => unreachable!("handled before the config loads"),
        Commands::Dashboard(args) => {
            let ws = Workspace::new(&audit_dir, &config.workspace.path);
            let mut scope = dashboard::status::Scope::from_config(&config);
            scope.owed.drop_unmatchable(&audit_dir);
            dashboard::serve(ws, scope, &args.bind, args.port).await?
        }
        Commands::Doctor => cmd_doctor(config)?,
        Commands::Login => cmd_login()?,
        Commands::Program(cli::ProgramCmd::Import { url }) => {
            let dir = config_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| std::path::Path::new("."));
            program_import::run(&url, dir, &config.target).await?
        }
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
    let ledger = orchestrator.ledger();
    let results = tokio::select! {
        r = orchestrator.run() => r?,
        sig = shutdown_signal() => {
            // `docker stop` would otherwise leave every live session "running" on the dashboard.
            ledger.stop_running()?;
            eprintln!("\n  Stopped by {sig}; unfinished sessions marked stopped.");
            std::process::exit(143);
        }
    };

    // Exit with error code if any pass failed
    if results
        .iter()
        .any(|r| r.status == pipeline::pass::PassStatus::Failed)
    {
        std::process::exit(1);
    }

    Ok(())
}

async fn shutdown_signal() -> &'static str {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = tokio::signal::ctrl_c() => "SIGINT",
    }
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
        eprintln!("Run: bulwark run --pass 1    (to start with reconnaissance)");
        return Ok(());
    }

    eprintln!("{}", style("Pipeline Status").bold());
    eprintln!();

    let pass_order = ["recon", "agents", "poc", "fuzzing", "formal", "review"];
    for (i, name) in pass_order.iter().enumerate() {
        if let Some(record) = status.passes.get(*name) {
            let status_str = match record.status {
                pipeline::pass::PassStatus::Completed => style("completed").green().to_string(),
                pipeline::pass::PassStatus::Failed => style("FAILED").red().to_string(),
                pipeline::pass::PassStatus::Skipped => style("skipped").yellow().to_string(),
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
            eprintln!("  Pass {}: {:<10} {}", i + 1, name, style("not run").dim());
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

fn cmd_findings(config: Config, audit_dir: PathBuf, args: cli::FindingsArgs) -> anyhow::Result<()> {
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

fn cmd_report(config: Config, audit_dir: PathBuf, args: cli::ReportArgs) -> anyhow::Result<()> {
    let ws = Workspace::new(&audit_dir, &config.workspace.path);
    let ws_cost = ws.cost_ledger();
    let bulwark_root = std::env::var("BULWARK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let bulwark_install = std::env::var("BULWARK_INSTALL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| bulwark_root.clone());

    let ctx = pipeline::pass::PipelineContext {
        config,
        workspace: ws,
        audit_dir,
        bulwark_root,
        bulwark_install,
        ledger: budget::Ledger::open(ws_cost, None, false)?,
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
            eprintln!("  {} {}", style("✓").green(), result);
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
            if docker_ok {
                style("✓").green()
            } else {
                style("✗").red()
            },
            "Docker",
            if docker_ok {
                "running"
            } else {
                "not available — install Docker Desktop"
            }
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
            Ok(path) => eprintln!("  {} {:<10} {}", style("✓").green(), tool, path.display()),
            Err(_) => eprintln!("  {} {:<10} not found", style("✗").red(), tool,),
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
        eprintln!("\n  To authenticate: {}", style("bulwark login").bold());
    }

    eprintln!();
    Ok(())
}

fn cmd_init(args: cli::InitArgs) -> anyhow::Result<()> {
    let path = args.path.canonicalize().unwrap_or(args.path.clone());
    let repo = args
        .repo
        .as_deref()
        .unwrap_or("https://github.com/YOUR-ORG/YOUR-REPO.git");

    // Derive a project name from repo URL
    let project_name = repo
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .unwrap_or("audit");

    let project_dir = if path.as_path() == std::path::Path::new(".") {
        std::env::current_dir()?
    } else {
        path.clone()
    };

    std::fs::create_dir_all(project_dir.join("context"))?;

    // bulwark.toml
    let toml = format!(
        r#"[target]
repo = "{repo}"
branch = "main"
scope = ["contracts"]
core_contracts = [
    # List the main contracts you want AI agents to focus on
    # e.g. "Vault", "Router", "Pool"
]
math_sensitive = [
    # List contracts with complex arithmetic for extra scrutiny
]

# Every stage runs on Opus by default; override any stage by name. Pass 2 agents are keyed by name.
# A table, so it must sit after every top-level key.
# [models]
# blue     = "claude-sonnet-5-5"
# fuzzing  = "claude-sonnet-5-5"

[budget]
# No cap by default, which suits a subscription. With an API key, set a hard cap: each pass
# gets its share of what is left, PoCs go highest severity first, spend lands in cost.json.
# max_usd = 50.0

[workspace]
path = "audit-workspace"

[passes.recon]
enabled = true
scv_scan = true
scv_scan_max_turns = 20

[passes.agents]
enabled = true
max_turns = 80
agents = ["red", "blue", "gold"]
timeout_minutes = 60
variant_analysis = true
variant_max_turns = 15

[passes.poc]
enabled = true
max_turns = 30
max_retries = 2
fp_check = true
fp_check_max_turns = 15

[passes.fuzzing]
enabled = true
fuzz_runs = 10_000
invariant_depth = 50
max_turns = 40

[passes.formal]
enabled = true
solver_timeout = 300
loop_bound = 5
target_properties = []
max_turns = 30

[passes.review]
enabled = true
max_turns = 60

[prompts]
dir = "prompts"

[schemas]
dir = "schemas"
"#,
    );
    std::fs::write(project_dir.join("bulwark.toml"), &toml)?;

    // context/AUDIT_CONTEXT.md
    let audit_context = format!(
        r#"# {project_name} — Audit Context

## Protocol Overview

TODO: Describe what this protocol does, its purpose, and its main components.

## Architecture

TODO: Describe the contract architecture. Key contracts, how they interact,
upgrade patterns, and any proxy relationships.

## Trust Model

TODO: Who are the privileged actors? What can they do?
- **Owner/Admin**: ...
- **Operators**: ...
- **Users**: ...

## Attack Surface

TODO: Where is value held? What operations move funds?
What external dependencies exist (oracles, other protocols)?

## Economic Parameters

TODO: Fee rates, token amounts, pool sizes, minimum/maximum values
that are relevant to economic attack analysis.

## Known Issues / Accepted Risks

See KNOWN_ISSUES.md.
"#
    );
    std::fs::write(
        project_dir.join("context").join("AUDIT_CONTEXT.md"),
        audit_context,
    )?;

    // context/PROPERTIES.md
    let properties = format!(
        r#"# {project_name} — Security Properties

These are the invariants the audit pipeline will attempt to verify and violate.

## P-1: [Property Name]

**Informal**: [One sentence describing the property]

**Formal**: [Mathematical or pseudo-code formulation if applicable]

**Enforcement**: [Which function(s) enforce this?]

---

## P-2: [Property Name]

TODO: Add one section per security property.

Properties should be:
- Specific and verifiable (not vague like "funds are safe")
- Tied to concrete code paths
- Testable with Foundry invariant tests or Halmos symbolic tests

---

<!-- Add more properties as P-3, P-4, etc. -->
"#
    );
    std::fs::write(
        project_dir.join("context").join("PROPERTIES.md"),
        properties,
    )?;

    // context/KNOWN_ISSUES.md
    let known_issues = format!(
        r#"# {project_name} — Known Issues and Accepted Risks

AI agents must NOT flag the following as findings. These are acknowledged risks
or design decisions accepted by the protocol team.

## KI-1: [Issue Name]

**Description**: TODO

**Accepted because**: TODO

---

<!-- Add more known issues as KI-2, KI-3, etc. -->
"#
    );
    std::fs::write(
        project_dir.join("context").join("KNOWN_ISSUES.md"),
        known_issues,
    )?;

    eprintln!("{}", style("✓ Project scaffold created").green().bold());
    eprintln!();
    eprintln!("  {}", project_dir.display());
    eprintln!("  ├── bulwark.toml");
    eprintln!("  └── context/");
    eprintln!("      ├── AUDIT_CONTEXT.md");
    eprintln!("      ├── PROPERTIES.md");
    eprintln!("      └── KNOWN_ISSUES.md");
    eprintln!();
    eprintln!("Next steps:");
    eprintln!("  1. Edit context/AUDIT_CONTEXT.md  — describe the protocol");
    eprintln!("  2. Edit context/PROPERTIES.md      — define the invariants to verify");
    eprintln!("  3. Edit context/KNOWN_ISSUES.md    — list accepted risks");
    eprintln!("  4. Edit bulwark.toml               — set scope and core_contracts");
    eprintln!(
        "  5. Run:  BULWARK_PROJECT={} docker compose up",
        project_dir.display()
    );

    Ok(())
}

fn cmd_login() -> anyhow::Result<()> {
    // Find claude binary
    let claude = which::which("claude").map_err(|_| {
        anyhow::anyhow!("Claude Code not found. Are you inside the Docker container?")
    })?;

    eprintln!("  {} Launching Claude login...\n", style("→").cyan());

    let status = std::process::Command::new(claude)
        .arg("login")
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()?;

    if status.success() {
        eprintln!(
            "\n  {} Claude authenticated successfully",
            style("✓").green()
        );
    } else {
        eprintln!("\n  {} Login failed", style("✗").red());
    }

    Ok(())
}

async fn cmd_bench(cmd: cli::BenchCmd) -> anyhow::Result<()> {
    match cmd {
        cli::BenchCmd::Score {
            expected,
            workspace,
            out,
            label,
            model,
        } => {
            let claude = which_claude();
            let r = bench::score(bench::ScoreArgs {
                expected: &expected,
                workspace: &workspace,
                out_dir: &out,
                label: &label,
                claude: &claude,
                model: &model,
            })
            .await?;
            println!("{}\n{}", bench::TABLE_HEADER, bench::table_row(&r));
        }
        cli::BenchCmd::Table { results } => {
            let mut rows: Vec<serde_json::Value> = std::fs::read_dir(&results)?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .filter_map(|p| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok())
                .collect();
            rows.sort_by(|a, b| b["scored_at"].as_str().cmp(&a["scored_at"].as_str()));
            println!("{}", bench::TABLE_HEADER);
            for r in &rows {
                println!("{}", bench::table_row(r));
            }
        }
    }
    Ok(())
}

fn which_claude() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    [
        format!("{home}/.local/bin/claude"),
        "/usr/local/bin/claude".into(),
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
    .unwrap_or_else(|| PathBuf::from("claude"))
}
