use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::anyhow;
use chrono::TimeZone as _;
use clap::CommandFactory;
use clap::Parser as _;
use clap::{self};
use clap_complete::CompleteEnv;
use jj_cli::cli_util::find_workspace_dir;
use jj_cli::config::ConfigEnv;
use jj_cli::config::config_from_environment;
use jj_cli::config::default_config_layers;
use jj_cli::config::default_config_migrations;
use jj_cli::revset_util;
use jj_cli::ui::Ui;
use jj_lib::ref_name::WorkspaceName;
use jj_lib::repo_path::RepoPathUiConverter;
use jj_lib::revset::RevsetAliasesMap;
use jj_lib::revset::RevsetExtensions;
use jj_lib::revset::RevsetParseContext;
use jj_lib::revset::RevsetWorkspaceContext;
use jj_lib::settings::UserSettings;
use jj_lib::workspace::DefaultWorkspaceLoaderFactory;
use jj_lib::workspace::WorkspaceLoaderFactory as _;

use crate::parse::ReferenceMap;
use crate::print::pretty_print;
use crate::tree::AnalyzeContext;

mod expr;
mod parse;
mod print;
mod tree;

#[derive(Debug, Copy, Clone, PartialEq, Eq, clap::ValueEnum)]
enum ColorMode {
    Auto,
    Never,
    Always,
}

/// Analyze a revset and display a tree showing how it will be evaluated
///
/// Potentially expensive operations are indicated with an `(EXPENSIVE)` label.
/// When color is enabled, operations are also colored based on how they are
/// evaluated. Eager evaluation is indicated by blue, lazy evaluation is
/// indicated by cyan, and predicates are indicated by magenta.
///
/// This tool attempts to match the default index implementation's revset engine
/// as well as possible. If you use a custom build of `jj` which uses a
/// different index implementation, analysis results may not be accurate.
///
/// To make the output easier to read, nested union, intersection, and coalesce
/// operations are flattened, and some operations have been renamed for clarity.
#[derive(clap::Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Collapses the provided revset alias, hiding it from the output
    #[arg(long, value_name = "ALIAS")]
    collapse: Vec<String>,

    /// When to colorize output
    #[arg(long, value_name = "MODE")]
    color: Option<ColorMode>,

    /// Base context for evaluation of revset
    ///
    /// For instance, if the entire revset will be iterated over, using
    /// `--context eager` may give more accurate analysis results. By default,
    /// lazy evaluation of the base revset is assumed.
    #[arg(short, long, default_value_t = AnalyzeContext::Lazy)]
    context: AnalyzeContext,

    /// Define a custom revset alias
    ///
    /// For example, `--define 'immutable_heads()=none()' will override
    /// `immutable_heads()` to be `none()`.
    #[arg(short, long)]
    define: Vec<String>,

    /// Disable analysis of evaluation and cost
    ///
    /// If you are using a different revset backend, the analysis features may
    /// not be useful, so this flag disables them. When using this option, all
    /// unresolved nodes are printed in blue.
    #[arg(short = 'A', long)]
    no_analyze: bool,

    /// Disable collapsing of builtin revset aliases
    ///
    /// By default, `trunk()` and `builtin_immutable_heads()` are collapsed to
    /// make the output easier to read.
    #[arg(short = 'B', long)]
    no_collapse_builtin: bool,

    /// Disable loading user-defined revset aliases
    #[arg(short = 'C', long)]
    no_config: bool,

    /// Disable revset optimizations
    #[arg(short = 'O', long)]
    no_optimize: bool,

    /// Path to repository to load revset aliases from
    #[arg(short = 'R', long, value_name = "PATH")]
    repository: Option<PathBuf>,

    /// A revset to analyze
    #[arg(value_name = "REVSET")]
    input: String,
}

fn main() -> anyhow::Result<()> {
    CompleteEnv::with_factory(Args::command).complete();

    let args = Args::parse();

    let cwd = env::current_dir()
        .and_then(dunce::canonicalize)
        .context("Failed to find current directory")?;
    let workspace_dir = args
        .repository
        .as_deref()
        .unwrap_or_else(|| find_workspace_dir(&cwd));
    let settings =
        load_settings(workspace_dir, !args.no_config).context("Failed to load settings")?;
    let ui = Ui::with_config(settings.config()).map_err(|err| err.error)?;
    if let Some(color) = args.color {
        // If color argument is provided directly, use it
        match color {
            ColorMode::Always => colored::control::set_override(true),
            ColorMode::Never => colored::control::set_override(false),
            _ => {}
        }
    } else {
        // Fall back to `jj` config (we don't support "debug" though)
        match settings.get("ui.color")? {
            jj_cli::ui::ColorChoice::Always => colored::control::set_override(true),
            jj_cli::ui::ColorChoice::Never => colored::control::set_override(false),
            _ => {}
        }
    };

    let path_converter = RepoPathUiConverter::Fs {
        cwd: cwd.clone(),
        base: workspace_dir.to_owned(),
    };
    let workspace_context = RevsetWorkspaceContext {
        path_converter: &path_converter,
        workspace_name: WorkspaceName::DEFAULT,
    };
    let now = if let Some(timestamp) = settings.commit_timestamp() {
        chrono::Local
            .timestamp_millis_opt(timestamp.timestamp.0)
            .unwrap()
    } else {
        chrono::Local::now()
    };
    let mut revset_aliases_map =
        revset_util::load_revset_aliases(&ui, settings.config()).map_err(|err| err.error)?;
    let collapse = |map: &mut RevsetAliasesMap, function: &str| -> anyhow::Result<()> {
        if args.input != function {
            map.insert(function, format!("{function:?}"))
                .context("Failed to parse alias name for `--collapse`")?;
        }
        Ok(())
    };
    if !args.no_collapse_builtin {
        collapse(&mut revset_aliases_map, "trunk()")?;
        collapse(&mut revset_aliases_map, "builtin_immutable_heads()")?;
    }
    for definition in args.define {
        let (name, value) = definition
            .split_once('=')
            .ok_or_else(|| anyhow!("Expected a '=' in revset definition"))?;
        revset_aliases_map
            .insert(name.trim(), value.trim())
            .context("Failed to insert revset definition")?;
    }
    for function in &args.collapse {
        collapse(&mut revset_aliases_map, function.as_str())?;
    }
    let parse_context = RevsetParseContext {
        aliases_map: &revset_aliases_map,
        local_variables: HashMap::new(),
        user_email: "<user-email>",
        date_pattern_context: now.into(),
        default_ignored_remote: None,
        use_glob_by_default: true,
        extensions: &RevsetExtensions::new(),
        workspace: Some(workspace_context),
    };
    let mut reference_map = ReferenceMap::new();
    let expr = parse::parse(
        &args.input,
        &parse_context,
        &mut reference_map,
        !args.no_optimize,
    )?;
    pretty_print(&expr, args.context, !args.no_analyze);
    Ok(())
}

fn load_settings(workspace_dir: &Path, load_user_config: bool) -> anyhow::Result<UserSettings> {
    let mut raw_config = config_from_environment(default_config_layers());
    let mut config_env = ConfigEnv::from_environment();
    if load_user_config {
        config_env
            .reload_user_config(&mut raw_config)
            .context("Failed to load user config")?;
        if let Ok(loader) = DefaultWorkspaceLoaderFactory.create(workspace_dir) {
            config_env.reset_repo_path(loader.repo_path());
            config_env
                .reload_repo_config(&mut raw_config)
                .context("Failed to load repo config")?;
            config_env.reset_workspace_path(loader.workspace_root());
            config_env
                .reload_workspace_config(&mut raw_config)
                .context("Failed to load workspace config")?;
        }
    }

    let mut config = config_env.resolve_config(&raw_config)?;
    jj_lib::config::migrate(&mut config, &default_config_migrations())
        .context("Failed to apply config migrations")?;

    let settings = UserSettings::from_config(config)?;
    Ok(settings)
}
