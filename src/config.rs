use crate::error::{BulwarkError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub target: TargetConfig,

    #[serde(default)]
    pub workspace: WorkspaceConfig,

    #[serde(default)]
    pub passes: PassesConfig,

    #[serde(default)]
    pub tools: ToolsConfig,

    #[serde(default)]
    pub prompts: PromptsConfig,

    #[serde(default)]
    pub schemas: SchemasConfig,

    /// Legacy single model for every stage not named in `[models]`.
    #[serde(default)]
    pub model: Option<String>,

    /// Claude model per stage, keyed by `STAGES` or an agent name.
    #[serde(default)]
    pub models: BTreeMap<String, String>,

    /// Logbook voice per stage, e.g. `red = "clarkson"`; `"none"` silences one.
    #[serde(default)]
    pub personas: BTreeMap<String, String>,

    #[serde(default)]
    pub budget: BudgetConfig,

    /// The bounty program; without it the scope check is skipped.
    #[serde(default)]
    pub program: Option<crate::program::ProgramConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Hard cap in USD on Claude spend for one run, pass 1 through 6. Unlimited by default.
    #[serde(default = "default_max_usd")]
    pub max_usd: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_usd: default_max_usd(),
        }
    }
}

fn default_max_usd() -> f64 {
    f64::INFINITY
}

pub const OPUS: &str = "claude-opus-5-5";

/// Stages that run a Claude session, besides the Pass 2 agents themselves.
pub const STAGES: &[&str] = &[
    "hunter",
    "scv_scan",
    "variant_analysis",
    "scope_check",
    "known_extract",
    "known_match",
    "fp_check",
    "poc",
    "fuzzing",
    "formal",
    "review",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetConfig {
    pub repo: String,

    #[serde(default = "default_branch")]
    pub branch: String,

    /// Commit to audit instead of the branch head. Bounty scopes are pinned to a commit,
    /// and code that only exists on the branch head may not be deployed.
    #[serde(default)]
    pub commit: Option<String>,

    pub scope: Vec<String>,

    pub core_contracts: Vec<String>,

    #[serde(default)]
    pub math_sensitive: Vec<String>,

    /// Optional shell command to run inside each scope directory before `forge build`.
    /// Useful for Hardhat/npm projects that need `pnpm install` or `npm install` first.
    /// Example: `pre_build_cmd = "pnpm install --frozen-lockfile"`
    #[serde(default)]
    pub pre_build_cmd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    #[serde(default = "default_workspace_path")]
    pub path: String,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            path: default_workspace_path(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PassesConfig {
    #[serde(default)]
    pub recon: ReconPassConfig,

    #[serde(default)]
    pub agents: AgentsPassConfig,

    #[serde(default)]
    pub poc: PocPassConfig,

    #[serde(default)]
    pub fuzzing: FuzzingPassConfig,

    #[serde(default)]
    pub formal: FormalPassConfig,

    #[serde(default)]
    pub review: ReviewPassConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconPassConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Run AI-assisted vulnerability scan (scv-scan) after Slither.
    /// Requires Claude auth and tob-scv-scan skill installed.
    #[serde(default = "default_true")]
    pub scv_scan: bool,

    /// Max turns for the scv-scan Claude session.
    #[serde(default = "default_scv_scan_turns")]
    pub scv_scan_max_turns: u32,
}

impl Default for ReconPassConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scv_scan: true,
            scv_scan_max_turns: default_scv_scan_turns(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsPassConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    #[serde(default = "default_agents")]
    pub agents: Vec<String>,

    #[serde(default = "default_agent_timeout")]
    pub timeout_minutes: u64,

    /// Run variant-analysis on high/critical findings after merge.
    #[serde(default = "default_true")]
    pub variant_analysis: bool,

    /// Max turns for each variant-analysis Claude session.
    #[serde(default = "default_variant_turns")]
    pub variant_max_turns: u32,

    /// Slices of the codebase, each hunted by its own agent alongside `agents`.
    #[serde(default)]
    pub components: Vec<Component>,

    /// Pass 2 sessions running at once.
    #[serde(default = "default_max_parallel")]
    pub max_parallel: usize,
}

/// Unknown keys are an error: in TOML, keys written after the last `[[components]]` belong to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub name: String,
    /// Source paths relative to the target repo root.
    pub files: Vec<String>,
}

impl Default for AgentsPassConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_turns: default_max_turns(),
            agents: default_agents(),
            timeout_minutes: default_agent_timeout(),
            variant_analysis: true,
            variant_max_turns: default_variant_turns(),
            components: Vec::new(),
            max_parallel: default_max_parallel(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PocPassConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_poc_turns")]
    pub max_turns: u32,

    #[serde(default = "default_poc_retries")]
    pub max_retries: u32,

    /// Run fp-check on each finding before PoC generation.
    #[serde(default = "default_true")]
    pub fp_check: bool,

    /// Max turns for each fp-check Claude session.
    #[serde(default = "default_fp_check_turns")]
    pub fp_check_max_turns: u32,

    /// Env var holding the fork RPC URL. Unset means local mode, where no PoC can
    /// demonstrate. The URL itself never goes in config or logs.
    #[serde(default)]
    pub fork_rpc_env: Option<String>,

    /// Block to pin the fork at; required with `fork_rpc_env`.
    #[serde(default)]
    pub fork_block: Option<u64>,
}

impl Default for PocPassConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_turns: default_poc_turns(),
            max_retries: default_poc_retries(),
            fp_check: true,
            fp_check_max_turns: default_fp_check_turns(),
            fork_rpc_env: None,
            fork_block: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuzzingPassConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_fuzz_runs")]
    pub fuzz_runs: u64,

    #[serde(default = "default_invariant_depth")]
    pub invariant_depth: u32,

    #[serde(default = "default_medusa_timeout")]
    pub medusa_timeout: u64,

    #[serde(default = "default_echidna_limit")]
    pub echidna_limit: u64,

    #[serde(default = "default_fuzz_turns")]
    pub max_turns: u32,

    /// Override Claude model for test generation (e.g. "sonnet" for better compilation).
    #[serde(default)]
    pub model: Option<String>,
}

impl Default for FuzzingPassConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fuzz_runs: default_fuzz_runs(),
            invariant_depth: default_invariant_depth(),
            medusa_timeout: default_medusa_timeout(),
            echidna_limit: default_echidna_limit(),
            max_turns: default_fuzz_turns(),
            model: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormalPassConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_solver_timeout")]
    pub solver_timeout: u64,

    #[serde(default = "default_loop_bound")]
    pub loop_bound: u32,

    #[serde(default)]
    pub target_properties: Vec<String>,

    #[serde(default = "default_formal_turns")]
    pub max_turns: u32,

    /// Override Claude model for test generation (e.g. "sonnet" for better compilation).
    #[serde(default)]
    pub model: Option<String>,
}

impl Default for FormalPassConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            solver_timeout: default_solver_timeout(),
            loop_bound: default_loop_bound(),
            target_properties: vec![],
            max_turns: default_formal_turns(),
            model: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPassConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_review_turns")]
    pub max_turns: u32,
}

impl Default for ReviewPassConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_turns: default_review_turns(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsConfig {
    pub forge: Option<String>,
    pub slither: Option<String>,
    pub claude: Option<String>,
    pub halmos: Option<String>,
    pub medusa: Option<String>,
    pub echidna: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptsConfig {
    #[serde(default = "default_prompts_dir")]
    pub dir: String,
}

impl Default for PromptsConfig {
    fn default() -> Self {
        Self {
            dir: default_prompts_dir(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemasConfig {
    #[serde(default = "default_schemas_dir")]
    pub dir: String,
}

impl Default for SchemasConfig {
    fn default() -> Self {
        Self {
            dir: default_schemas_dir(),
        }
    }
}

// Default value functions
fn default_branch() -> String {
    "main".into()
}
fn default_workspace_path() -> String {
    "audit-workspace".into()
}
fn default_true() -> bool {
    true
}
fn default_max_turns() -> u32 {
    400
}
fn default_agents() -> Vec<String> {
    vec!["red".into(), "blue".into(), "gold".into()]
}
fn default_max_parallel() -> usize {
    4
}
fn default_agent_timeout() -> u64 {
    240
}
fn default_poc_turns() -> u32 {
    100
}
fn default_poc_retries() -> u32 {
    2
}
fn default_fuzz_runs() -> u64 {
    10_000
}
fn default_invariant_depth() -> u32 {
    50
}
fn default_medusa_timeout() -> u64 {
    3600
}
fn default_echidna_limit() -> u64 {
    500_000
}
fn default_fuzz_turns() -> u32 {
    100
}
fn default_solver_timeout() -> u64 {
    300
}
fn default_loop_bound() -> u32 {
    5
}
fn default_formal_turns() -> u32 {
    80
}
fn default_review_turns() -> u32 {
    150
}
fn default_prompts_dir() -> String {
    "prompts".into()
}
fn default_schemas_dir() -> String {
    "schemas".into()
}
fn default_scv_scan_turns() -> u32 {
    60
}
fn default_variant_turns() -> u32 {
    50
}
fn default_fp_check_turns() -> u32 {
    40
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            BulwarkError::Config(format!("failed to read config at {}: {e}", path.display()))
        })?;
        let mut config: Config = toml::from_str(&content)?;
        // `bulwark program import` writes program.toml beside the config.
        let imported = path.with_file_name("program.toml");
        if config.program.is_none() && imported.exists() {
            #[derive(Deserialize)]
            struct Wrap {
                program: crate::program::ProgramConfig,
            }
            let w: Wrap = toml::from_str(&std::fs::read_to_string(&imported)?)?;
            config.program = Some(w.program);
        }
        config.check_models()?;
        Ok(config)
    }

    fn check_models(&self) -> Result<()> {
        let known =
            |k: &str| STAGES.contains(&k) || self.passes.agents.agents.iter().any(|a| a == k);
        let unknown: Vec<&str> = self
            .models
            .keys()
            .map(String::as_str)
            .filter(|k| !known(k))
            .collect();
        if unknown.is_empty() {
            return Ok(());
        }
        Err(BulwarkError::Config(format!(
            "unknown [models] stage(s): {}; known: {}, plus agents {}",
            unknown.join(", "),
            STAGES.join(", "),
            self.passes.agents.agents.join(", ")
        )))
    }

    /// Display name for a persona key.
    pub fn persona_name(key: &str) -> &str {
        match key {
            "clarkson" => "Jeremy Clarkson",
            "may" => "James May",
            "foghorn" => "Foghorn Leghorn",
            "juliette" => "Juliette Nichols",
            "wilman" => "Andy Wilman",
            other => other,
        }
    }

    /// Persona key for a stage. Stages that read a verdict from the session's final text
    /// (fp_check, known_*, scope_check) have no default, so narration cannot flip a result.
    pub fn persona_for(&self, stage: &str) -> Option<String> {
        let key = match self.personas.get(stage) {
            Some(k) => k.as_str(),
            None => match stage {
                "red" | "scv_scan" => "clarkson",
                "blue" | "fuzzing" | "formal" | "variant_analysis" => "may",
                "gold" => "foghorn",
                "hunter" | "poc" => "juliette",
                "review" => "wilman",
                _ => return None,
            },
        };
        (key != "none").then(|| key.to_string())
    }

    /// Model for a stage: `[models]`, then the pass's own override, then the legacy
    /// top-level `model`, then the built-in default.
    pub fn model_for(&self, stage: &str) -> String {
        let pass_override = match stage {
            "fuzzing" => self.passes.fuzzing.model.as_ref(),
            "formal" => self.passes.formal.model.as_ref(),
            _ => None,
        };
        self.models
            .get(stage)
            .or(pass_override)
            .or(self.model.as_ref())
            .cloned()
            .unwrap_or_else(|| OPUS.to_string())
    }

    /// Resolve tool path: use configured override, or fall back to `which`.
    pub fn resolve_tool(&self, name: &str) -> Result<PathBuf> {
        let override_path = match name {
            "forge" => self.tools.forge.as_deref(),
            "slither" => self.tools.slither.as_deref(),
            "claude" => self.tools.claude.as_deref(),
            "halmos" => self.tools.halmos.as_deref(),
            "medusa" => self.tools.medusa.as_deref(),
            "echidna" => self.tools.echidna.as_deref(),
            _ => None,
        };

        if let Some(p) = override_path {
            let path = PathBuf::from(p);
            if path.exists() {
                return Ok(path);
            }
            return Err(BulwarkError::ToolNotFound {
                tool: name.into(),
                hint: format!("configured path does not exist: {p}"),
            });
        }

        which::which(name).map_err(|_| BulwarkError::ToolNotFound {
            tool: name.into(),
            hint: install_hint(name),
        })
    }

    /// Check if a tool is available (configured or on PATH) without erroring.
    pub fn has_tool(&self, name: &str) -> bool {
        self.resolve_tool(name).is_ok()
    }
}

fn install_hint(tool: &str) -> String {
    match tool {
        "forge" => "curl -L https://foundry.paradigm.xyz | bash && foundryup".into(),
        "slither" => "pip install slither-analyzer".into(),
        "claude" => "curl -fsSL https://claude.ai/install.sh | bash".into(),
        "halmos" => "pip install halmos".into(),
        "medusa" => "see https://github.com/crytic/medusa".into(),
        "echidna" => "see https://github.com/crytic/echidna".into(),
        _ => format!("install {tool} and ensure it is on PATH"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TOML: &str = r#"
[target]
repo = "https://example.com/repo.git"
scope = ["src"]
core_contracts = ["Test"]
"#;

    #[test]
    fn parse_minimal_toml_config() {
        let config: Config = toml::from_str(MINIMAL_TOML).expect("parse minimal config");

        assert_eq!(config.target.repo, "https://example.com/repo.git");
        assert_eq!(config.target.scope, vec!["src"]);
        assert_eq!(config.target.core_contracts, vec!["Test"]);
    }

    #[test]
    fn defaults_applied_when_sections_omitted() {
        let config: Config = toml::from_str(MINIMAL_TOML).expect("parse");

        // Target defaults
        assert_eq!(config.target.branch, "main");
        assert!(config.target.math_sensitive.is_empty());

        // Workspace default
        assert_eq!(config.workspace.path, "audit-workspace");

        // Tools all default to None
        assert!(config.tools.forge.is_none());
        assert!(config.tools.slither.is_none());
        assert!(config.tools.claude.is_none());
        assert!(config.tools.halmos.is_none());
        assert!(config.tools.medusa.is_none());
        assert!(config.tools.echidna.is_none());

        // Prompts and schemas defaults
        assert_eq!(config.prompts.dir, "prompts");
        assert_eq!(config.schemas.dir, "schemas");
    }

    #[test]
    fn components_parse() {
        let c: Config = toml::from_str(&format!(
            "{MINIMAL_TOML}\n[[passes.agents.components]]\nname = \"queues\"\nfiles = [\"src/Q.sol\"]\n"
        ))
        .unwrap();
        assert_eq!(
            c.passes.agents.components,
            vec![Component {
                name: "queues".into(),
                files: vec!["src/Q.sol".into()]
            }]
        );
        assert_eq!(c.passes.agents.max_parallel, 4);
    }

    #[test]
    fn keys_after_a_component_are_rejected() {
        let r: std::result::Result<Config, _> = toml::from_str(&format!(
            "{MINIMAL_TOML}\n[[passes.agents.components]]\nname = \"q\"\nfiles = []\ntimeout_minutes = 5\n"
        ));
        assert!(r.is_err());
    }

    #[test]
    fn default_agents_list_is_red_blue_gold() {
        let config: Config = toml::from_str(MINIMAL_TOML).expect("parse");

        assert_eq!(config.passes.agents.agents, vec!["red", "blue", "gold"]);
    }

    #[test]
    fn all_pass_configs_default_to_enabled() {
        let config: Config = toml::from_str(MINIMAL_TOML).expect("parse");

        assert!(config.passes.recon.enabled);
        assert!(config.passes.agents.enabled);
        assert!(config.passes.poc.enabled);
        assert!(config.passes.fuzzing.enabled);
        assert!(config.passes.formal.enabled);
        assert!(config.passes.review.enabled);
    }

    #[test]
    fn full_config_round_trip() {
        let config: Config = toml::from_str(MINIMAL_TOML).expect("parse");

        let serialized = toml::to_string(&config).expect("serialize");
        let roundtripped: Config = toml::from_str(&serialized).expect("re-parse");

        assert_eq!(roundtripped.target.repo, config.target.repo);
        assert_eq!(roundtripped.target.scope, config.target.scope);
        assert_eq!(
            roundtripped.target.core_contracts,
            config.target.core_contracts
        );
        assert_eq!(roundtripped.target.branch, config.target.branch);
        assert_eq!(roundtripped.workspace.path, config.workspace.path);
        assert_eq!(
            roundtripped.passes.agents.agents,
            config.passes.agents.agents
        );
        assert_eq!(
            roundtripped.passes.agents.max_turns,
            config.passes.agents.max_turns
        );
        assert_eq!(
            roundtripped.passes.fuzzing.fuzz_runs,
            config.passes.fuzzing.fuzz_runs
        );
        assert_eq!(
            roundtripped.passes.formal.solver_timeout,
            config.passes.formal.solver_timeout
        );
    }

    #[test]
    fn pass_config_default_values_are_correct() {
        let config: Config = toml::from_str(MINIMAL_TOML).expect("parse");

        assert!(config.budget.max_usd.is_infinite());
        assert_eq!(config.passes.agents.max_turns, 400);
        assert_eq!(config.passes.agents.timeout_minutes, 240);
        assert_eq!(config.passes.poc.max_turns, 100);
        assert_eq!(config.passes.poc.max_retries, 2);
        assert_eq!(config.passes.fuzzing.fuzz_runs, 10_000);
        assert_eq!(config.passes.fuzzing.invariant_depth, 50);
        assert_eq!(config.passes.formal.solver_timeout, 300);
        assert_eq!(config.passes.formal.loop_bound, 5);
        assert_eq!(config.passes.fuzzing.max_turns, 100);
        assert_eq!(config.passes.formal.max_turns, 80);
        assert_eq!(config.passes.review.max_turns, 150);

        // Phase 1 skill integration defaults
        assert!(config.passes.recon.scv_scan);
        assert_eq!(config.passes.recon.scv_scan_max_turns, 60);
        assert!(config.passes.agents.variant_analysis);
        assert_eq!(config.passes.agents.variant_max_turns, 50);
        assert!(config.passes.poc.fp_check);
        assert_eq!(config.passes.poc.fp_check_max_turns, 40);
    }

    fn with(extra: &str) -> Result<Config> {
        let config: Config = toml::from_str(&format!("{extra}\n{MINIMAL_TOML}")).expect("parse");
        config.check_models().map(|_| config)
    }

    #[test]
    fn personas_default_override_and_silence() {
        let c = with("").unwrap();
        assert_eq!(c.persona_for("red").as_deref(), Some("clarkson"));
        assert_eq!(c.persona_for("hunter").as_deref(), Some("juliette"));
        assert_eq!(c.persona_for("fp_check"), None);
        let c: Config = toml::from_str(&format!(
            "{MINIMAL_TOML}\n[personas]\nred = \"may\"\nreview = \"none\"\n"
        ))
        .unwrap();
        assert_eq!(c.persona_for("red").as_deref(), Some("may"));
        assert_eq!(c.persona_for("review"), None);
    }

    #[test]
    fn every_default_persona_has_a_voice_file() {
        let c = with("").unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompts/personas");
        assert!(dir.join("_logbook.md").exists());
        for s in ["red", "blue", "gold"].iter().chain(STAGES) {
            if let Some(k) = c.persona_for(s) {
                assert!(dir.join(format!("{k}.md")).exists(), "{s} -> {k}");
            }
        }
    }

    #[test]
    fn every_stage_defaults_to_opus() {
        let c = with("").unwrap();
        for s in ["red", "gold", "blue"].iter().chain(STAGES) {
            assert_eq!(c.model_for(s), OPUS, "{s}");
        }
    }

    #[test]
    fn models_table_beats_pass_override_beats_top_level() {
        let c: Config = toml::from_str(&format!(
            "model = \"haiku\"\n{MINIMAL_TOML}\n[models]\nred = \"x\"\nfuzzing = \"y\"\n\
             [passes.fuzzing]\nmodel = \"z\"\n[passes.formal]\nmodel = \"w\"\n"
        ))
        .unwrap();
        assert_eq!(c.model_for("red"), "x");
        assert_eq!(c.model_for("fuzzing"), "y");
        assert_eq!(c.model_for("formal"), "w");
        assert_eq!(c.model_for("review"), "haiku");
    }

    #[test]
    fn unknown_models_key_is_refused() {
        let err = toml::from_str::<Config>(&format!("{MINIMAL_TOML}\n[models]\nrde = \"x\"\n"))
            .unwrap()
            .check_models()
            .unwrap_err();
        assert!(err.to_string().contains("rde"));
    }

    #[test]
    fn custom_agent_names_are_valid_keys() {
        let c: Config = toml::from_str(&format!(
            "{MINIMAL_TOML}\n[passes.agents]\nagents = [\"purple\"]\n[models]\npurple = \"x\"\n"
        ))
        .unwrap();
        assert!(c.check_models().is_ok());
        assert_eq!(c.model_for("purple"), "x");
    }

    #[test]
    fn shipped_config_loads() {
        let c: Config =
            toml::from_str(include_str!("../bulwark.toml")).expect("bulwark.toml parses");
        c.check_models().unwrap();
        assert_eq!(c.model_for("red"), OPUS);
    }
}
