use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "familiar-daemon", about = "Familiar background daemon")]
pub struct Cli {
    /// Path to configuration file
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Override log level (e.g. debug, info, warn, error)
    #[arg(long)]
    pub log_level: Option<String>,

    /// Run in foreground (default)
    #[arg(long, default_value_t = true)]
    pub foreground: bool,
}
