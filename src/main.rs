mod config;
mod db;
mod guards;
mod hookio;
mod hooks;
mod install;
mod memory;
mod pricing;
mod report;
mod serve;
mod transcript;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;

#[derive(Parser)]
#[command(
    name = "skout",
    version,
    about = "Token-aware guard rails and cost analytics for Claude Code",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install skout's hooks into Claude Code settings
    Init {
        /// Write to ./.claude/settings.json instead of the user-level settings
        #[arg(long)]
        project: bool,
        #[arg(long)]
        force: bool,
    },
    /// Remove skout's hooks, keeping config and history
    Uninstall {
        #[arg(long)]
        project: bool,
    },
    /// Show token usage, cost, and what skout saved
    Report {
        /// Every project, not just the current directory
        #[arg(long)]
        all: bool,
        #[arg(long, group = "window")]
        today: bool,
        #[arg(long, group = "window")]
        week: bool,
        #[arg(long, group = "window")]
        month: bool,
        /// No time filter
        #[arg(long, group = "window")]
        ever: bool,
        #[arg(long)]
        json: bool,
    },
    /// Serve the dashboard on localhost
    Serve {
        #[arg(long, default_value_t = 7331)]
        port: u16,
        /// Do not open a browser window
        #[arg(long)]
        no_open: bool,
    },
    /// Read and change settings
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// Check the installation
    Doctor,
    /// Forget which files are already in context
    Reset {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Internal: invoked by Claude Code hooks
    #[command(hide = true)]
    Hook { event: String },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Show every setting and its current value
    List,
    Get { key: String },
    Set { key: String, value: String },
    /// Print the config file location
    Path,
}

fn main() {
    let cli = Cli::parse();

    // The hook path must never surface an error to the user's session.
    if let Cmd::Hook { event } = &cli.cmd {
        hooks::dispatch(event);
        return;
    }

    if let Err(e) = run(cli) {
        eprintln!("skout: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Hook { .. } => unreachable!(),

        Cmd::Init { project, force } => {
            println!();
            install::init(project, force)?;
            println!();
            println!("  skout is live. Start a new Claude Code session to pick up the hooks.");
            println!("  Try `/skout` in the REPL, or `skout report` here.");
            println!();
            Ok(())
        }

        Cmd::Uninstall { project } => {
            println!();
            install::uninstall(project)?;
            println!();
            Ok(())
        }

        Cmd::Doctor => {
            let err = Config::load_strict().err().map(|e| e.to_string());
            install::doctor(err)
        }

        Cmd::Serve { port, no_open } => serve::run(port, !no_open),

        Cmd::Config { action } => config_cmd(action),

        Cmd::Reset { session, all } => {
            let conn = db::open()?;
            if all {
                conn.execute("DELETE FROM reads", [])?;
                conn.execute("DELETE FROM deny_counts", [])?;
                println!("skout: cleared read history for all sessions");
            } else {
                let sid = session
                    .or_else(|| std::env::var("CLAUDE_SESSION_ID").ok())
                    .unwrap_or_default();
                if sid.is_empty() {
                    println!("skout: pass --session <id> or --all");
                    return Ok(());
                }
                let n = db::reset_session(&conn, &sid)?;
                println!("skout: cleared {n} read record(s) for session {sid}");
            }
            Ok(())
        }

        Cmd::Report { all, today, week, month, ever, json } => {
            let cfg = Config::load();
            let conn = db::open()?;
            let (since, label) = window(today, week, month, ever);
            let cwd = std::env::current_dir()?.to_string_lossy().to_string();
            report::run(
                &conn,
                &cfg,
                report::Opts { scope_all: all, cwd, since, window_label: label, json },
            )
        }
    }
}

fn window(today: bool, _week: bool, month: bool, ever: bool) -> (i64, String) {
    let name = if ever {
        "ever"
    } else if today {
        "today"
    } else if month {
        "month"
    } else {
        // Default window.
        "week"
    };
    window_from(name)
}

/// Named time windows, shared by the CLI flags and the dashboard's query string.
pub fn window_from(name: &str) -> (i64, String) {
    use chrono::{Duration, Local, Timelike};
    let now = Local::now();
    match name {
        "ever" => (0, "all time".into()),
        "today" => {
            let start = now
                .with_hour(0).unwrap()
                .with_minute(0).unwrap()
                .with_second(0).unwrap();
            (start.timestamp(), "today".into())
        }
        "month" => ((now - Duration::days(30)).timestamp(), "last 30 days".into()),
        _ => ((now - Duration::days(7)).timestamp(), "last 7 days".into()),
    }
}

fn config_cmd(action: ConfigCmd) -> Result<()> {
    match action {
        ConfigCmd::Path => {
            println!("{}", config::path().display());
            Ok(())
        }
        ConfigCmd::Get { key } => {
            let cfg = Config::load_strict()?;
            println!("{}", cfg.get(&key)?);
            Ok(())
        }
        ConfigCmd::Set { key, value } => {
            let mut cfg = Config::load_strict()?;
            cfg.set(&key, &value)?;
            cfg.save()?;
            println!("skout: {key} = {}", cfg.get(&key)?);
            Ok(())
        }
        ConfigCmd::List => {
            let cfg = Config::load_strict()?;
            println!();
            for key in Config::KEYS {
                println!("  {:<24} {}", key, cfg.get(key)?);
            }
            println!();
            println!("  {}", config::path().display());
            println!();
            Ok(())
        }
    }
}
