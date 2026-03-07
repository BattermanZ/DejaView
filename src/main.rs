use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use colored::*;
use std::io::IsTerminal;
use std::path::PathBuf;

mod config;
mod engine;
mod fsutil;
mod protocol;
mod tui;

use config::{load_config, save_config};
use fsutil::normalize_runtime_dir;
use std::fs;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Scan for duplicates (dry run)
    Scan {
        /// Fail if any scan/hash/link errors occur
        #[arg(long)]
        strict: bool,
        #[command(flatten)]
        output: RunOutputArgs,
    },
    /// Replace duplicates with hardlinks when possible, else symlinks
    Link {
        /// Fail if any scan/hash/link errors occur
        #[arg(long)]
        strict: bool,
        /// Ask for confirmation before applying changes (interactive terminals only)
        #[arg(long)]
        confirm: bool,
        #[command(flatten)]
        output: RunOutputArgs,
    },
    /// Manage the configuration
    Config(ConfigArgs),
    /// Manage the cache
    Cache(CacheArgs),
    /// Open interactive terminal UI
    Tui {
        /// Use strict mode for scan/link actions triggered from the TUI
        #[arg(long)]
        strict: bool,
    },
    /// Preview alternative terminal logo directions
    TuiLogoPreview,
}

#[derive(Parser, Debug)]
struct RunOutputArgs {
    /// Print machine-readable JSON summary to stdout
    #[arg(long)]
    json: bool,
    /// Write run report JSON to this path
    #[arg(long)]
    report: Option<PathBuf>,
    /// Progress display mode
    #[arg(long, value_enum, default_value_t = ProgressMode::Auto)]
    progress: ProgressMode,
    /// Shortcut for --progress off
    #[arg(long, conflicts_with = "progress")]
    no_progress: bool,
    /// Internal NDJSON event stream for the TUI subprocess runner
    #[arg(long, hide = true)]
    tui_events: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum ProgressMode {
    Auto,
    On,
    Off,
}

fn progress_enabled(mode: ProgressMode, no_progress: bool, json: bool, tui_events: bool) -> bool {
    if no_progress || json || tui_events {
        return false;
    }
    match mode {
        ProgressMode::Auto => std::io::stdout().is_terminal(),
        ProgressMode::On => true,
        ProgressMode::Off => false,
    }
}

#[derive(Parser, Debug)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommands,
}

#[derive(Subcommand, Debug)]
enum ConfigCommands {
    /// Set the downloads path
    SetDownloads { path: PathBuf },
    /// Set the media path
    SetMedia { path: PathBuf },
    /// Show the current configuration
    Show,
    /// Set the minimum file age in seconds before considering files
    SetMinAge { seconds: u64 },
    /// Set worker thread count for hashing
    SetThreads { count: usize },
    /// Validate configured paths and permissions
    Validate,
}

#[derive(Parser, Debug)]
struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommands,
}

#[derive(Subcommand, Debug)]
enum CacheCommands {
    /// Clear the cache
    Clear,
    /// Show cache info
    Info,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut config = load_config()?;

    if let Some(threads) = config.threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()?;
    }

    match cli.command {
        Commands::Scan { strict, output } => {
            ensure_required_paths(&config)?;
            engine::run_scan_with_options(
                &config,
                engine::ScanRunOptions {
                    dry_run: true,
                    strict,
                    report_path: output.report.as_deref(),
                    json_output: output.json,
                    show_progress: progress_enabled(
                        output.progress,
                        output.no_progress,
                        output.json,
                        output.tui_events,
                    ),
                    require_confirm: false,
                    tui_events: output.tui_events,
                },
            )?
        }
        Commands::Link {
            strict,
            confirm,
            output,
        } => {
            ensure_required_paths(&config)?;
            engine::run_scan_with_options(
                &config,
                engine::ScanRunOptions {
                    dry_run: false,
                    strict,
                    report_path: output.report.as_deref(),
                    json_output: output.json,
                    show_progress: progress_enabled(
                        output.progress,
                        output.no_progress,
                        output.json,
                        output.tui_events,
                    ),
                    require_confirm: confirm,
                    tui_events: output.tui_events,
                },
            )?
        }
        Commands::Config(args) => match args.command {
            ConfigCommands::SetDownloads { path } => {
                config.downloads = Some(normalize_runtime_dir(&path)?);
                save_config(&config)?;
                println!("{} Downloads path set.", "✔".green());
            }
            ConfigCommands::SetMedia { path } => {
                config.media = Some(normalize_runtime_dir(&path)?);
                save_config(&config)?;
                println!("{} Media path set.", "✔".green());
            }
            ConfigCommands::Show => {
                println!("{}", serde_json::to_string_pretty(&config)?);
            }
            ConfigCommands::SetMinAge { seconds } => {
                config.min_age = Some(seconds);
                save_config(&config)?;
                println!("{} min_age set to {} seconds.", "✔".green(), seconds);
            }
            ConfigCommands::SetThreads { count } => {
                if count == 0 {
                    return Err(anyhow::anyhow!("threads must be greater than 0"));
                }
                config.threads = Some(count);
                save_config(&config)?;
                println!("{} threads set to {}.", "✔".green(), count);
            }
            ConfigCommands::Validate => validate_config(&config)?,
        },
        Commands::Cache(args) => match args.command {
            CacheCommands::Clear => {
                engine::clear_cache(&config)?;
                println!("{} Cache cleared.", "✔".green());
            }
            CacheCommands::Info => {
                let (path, entries) = engine::cache_info(&config)?;
                println!("Cache location: {}", path.display());
                println!("Cached entries: {}", entries);
            }
        },
        Commands::Tui { strict } => {
            tui::run(&config, strict)?;
        }
        Commands::TuiLogoPreview => {
            tui::run_logo_preview()?;
        }
    }

    Ok(())
}

fn ensure_required_paths(config: &config::Config) -> Result<()> {
    config::validate_runtime_paths(config)
}

fn validate_config(config: &config::Config) -> Result<()> {
    let mut failures = Vec::new();

    match &config.downloads {
        Some(path) => {
            if !path.exists() {
                failures.push(format!("downloads path does not exist: {}", path.display()));
            } else if !path.is_dir() {
                failures.push(format!(
                    "downloads path is not a directory: {}",
                    path.display()
                ));
            } else {
                let probe = path.join(format!(".dejaview_validate_{}", std::process::id()));
                match fs::File::create(&probe) {
                    Ok(_) => {
                        let _ = fs::remove_file(&probe);
                    }
                    Err(err) => failures.push(format!(
                        "downloads path is not writable (needed for cache/link ops): {} ({err})",
                        path.display()
                    )),
                }
            }
        }
        None => failures.push(
            "downloads path not set; run: DejaView config set-downloads /path/to/downloads"
                .to_string(),
        ),
    }

    match &config.media {
        Some(path) => {
            if !path.exists() {
                failures.push(format!("media path does not exist: {}", path.display()));
            } else if !path.is_dir() {
                failures.push(format!("media path is not a directory: {}", path.display()));
            } else if let Err(err) = fs::read_dir(path) {
                failures.push(format!(
                    "media path is not readable (needed for scan/hash): {} ({err})",
                    path.display()
                ));
            }
        }
        None => failures
            .push("media path not set; run: DejaView config set-media /path/to/media".to_string()),
    }

    if failures.is_empty() {
        println!("{} Configuration is valid.", "✔".green());
        Ok(())
    } else {
        for failure in &failures {
            eprintln!("{} {}", "✖".red(), failure);
        }
        Err(anyhow::anyhow!("configuration validation failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scan_command() {
        let cli = Cli::try_parse_from(["dejaview", "scan", "--strict"]).expect("parse scan");
        match cli.command {
            Commands::Scan { strict, output } => {
                assert!(strict);
                assert!(!output.json);
                assert_eq!(output.report, None);
                assert_eq!(output.progress, ProgressMode::Auto);
                assert!(!output.no_progress);
                assert!(!output.tui_events);
            }
            _ => panic!("expected scan command"),
        }
    }

    #[test]
    fn parses_link_command() {
        let cli = Cli::try_parse_from([
            "dejaview",
            "link",
            "--json",
            "--report",
            "/tmp/report.json",
            "--no-progress",
        ])
        .expect("parse link");
        match cli.command {
            Commands::Link {
                strict,
                confirm,
                output,
            } => {
                assert!(!strict);
                assert!(!confirm);
                assert!(output.json);
                assert_eq!(output.report, Some(PathBuf::from("/tmp/report.json")));
                assert_eq!(output.progress, ProgressMode::Auto);
                assert!(output.no_progress);
                assert!(!output.tui_events);
            }
            _ => panic!("expected link command"),
        }
    }

    #[test]
    fn parses_scan_progress_mode() {
        let cli = Cli::try_parse_from(["dejaview", "scan", "--progress", "off"])
            .expect("parse scan progress");
        match cli.command {
            Commands::Scan { output, .. } => assert_eq!(output.progress, ProgressMode::Off),
            _ => panic!("expected scan command"),
        }
    }

    #[test]
    fn parses_config_show_command() {
        let cli = Cli::try_parse_from(["dejaview", "config", "show"]).expect("parse config show");
        match cli.command {
            Commands::Config(ConfigArgs {
                command: ConfigCommands::Show,
            }) => {}
            _ => panic!("expected config show command"),
        }
    }

    #[test]
    fn parses_config_set_min_age_command() {
        let cli = Cli::try_parse_from(["dejaview", "config", "set-min-age", "120"])
            .expect("parse config set-min-age");
        match cli.command {
            Commands::Config(ConfigArgs {
                command: ConfigCommands::SetMinAge { seconds },
            }) => assert_eq!(seconds, 120),
            _ => panic!("expected config set-min-age command"),
        }
    }

    #[test]
    fn parses_link_confirm_flag() {
        let cli = Cli::try_parse_from(["dejaview", "link", "--confirm"]).expect("parse link");
        match cli.command {
            Commands::Link { confirm, .. } => assert!(confirm),
            _ => panic!("expected link command"),
        }
    }

    #[test]
    fn parses_cache_info_command() {
        let cli = Cli::try_parse_from(["dejaview", "cache", "info"]).expect("parse cache info");
        match cli.command {
            Commands::Cache(CacheArgs {
                command: CacheCommands::Info,
            }) => {}
            _ => panic!("expected cache info command"),
        }
    }

    #[test]
    fn parses_tui_command() {
        let cli = Cli::try_parse_from(["dejaview", "tui", "--strict"]).expect("parse tui");
        match cli.command {
            Commands::Tui { strict } => assert!(strict),
            _ => panic!("expected tui command"),
        }
    }

    #[test]
    fn parses_tui_logo_preview_command() {
        let cli =
            Cli::try_parse_from(["dejaview", "tui-logo-preview"]).expect("parse logo preview");
        match cli.command {
            Commands::TuiLogoPreview => {}
            _ => panic!("expected logo preview command"),
        }
    }
}
