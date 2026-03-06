use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::*;
use std::path::PathBuf;

mod config;
mod engine;

use config::{load_config, save_config};

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
    },
    /// Replace duplicates with symlinks
    Link {
        /// Fail if any scan/hash/link errors occur
        #[arg(long)]
        strict: bool,
        /// Write link report manifest to this path
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Manage the configuration
    Config(ConfigArgs),
    /// Manage the cache
    Cache(CacheArgs),
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
        Commands::Scan { strict } => engine::run_scan(&config, true, strict, None)?,
        Commands::Link { strict, report } => {
            engine::run_scan(&config, false, strict, report.as_deref())?
        }
        Commands::Config(args) => match args.command {
            ConfigCommands::SetDownloads { path } => {
                config.downloads = Some(path);
                save_config(&config)?;
                println!("{} Downloads path set.", "✔".green());
            }
            ConfigCommands::SetMedia { path } => {
                config.media = Some(path);
                save_config(&config)?;
                println!("{} Media path set.", "✔".green());
            }
            ConfigCommands::Show => {
                println!("{}", serde_json::to_string_pretty(&config)?);
            }
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
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scan_command() {
        let cli = Cli::try_parse_from(["dejaview", "scan", "--strict"]).expect("parse scan");
        match cli.command {
            Commands::Scan { strict } => assert!(strict),
            _ => panic!("expected scan command"),
        }
    }

    #[test]
    fn parses_link_command() {
        let cli = Cli::try_parse_from(["dejaview", "link", "--report", "/tmp/report.json"])
            .expect("parse link");
        match cli.command {
            Commands::Link { strict, report } => {
                assert!(!strict);
                assert_eq!(report, Some(PathBuf::from("/tmp/report.json")));
            }
            _ => panic!("expected link command"),
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
    fn parses_cache_info_command() {
        let cli = Cli::try_parse_from(["dejaview", "cache", "info"]).expect("parse cache info");
        match cli.command {
            Commands::Cache(CacheArgs {
                command: CacheCommands::Info,
            }) => {}
            _ => panic!("expected cache info command"),
        }
    }
}
