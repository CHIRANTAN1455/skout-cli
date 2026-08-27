use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::util;

/// Every hook we own. `async` where the result cannot change a decision, so the
/// hook never sits in the critical path of a tool call.
const EVENTS: &[(&str, &str, &str, bool)] = &[
    // (event, matcher, subcommand, async)
    ("PreToolUse", "Read|Bash|Grep", "pre-tool-use", false),
    ("PostToolUse", "*", "post-tool-use", true),
    ("SessionStart", "*", "session-start", true),
    ("SessionEnd", "*", "session-end", true),
];

fn settings_path(project: bool) -> PathBuf {
    if project {
        PathBuf::from(".claude").join("settings.json")
    } else {
        util::claude_dir().join("settings.json")
    }
}

fn bin_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "skout".into())
}

/// True if this hook entry is one of ours, by binary basename.
fn is_ours(h: &Value) -> bool {
    h.get("command")
        .and_then(|c| c.as_str())
        .map(|c| {
            let base = c.rsplit('/').next().unwrap_or(c);
            base == "skout" || base == "skout.exe"
        })
        .unwrap_or(false)
}

fn load(path: &PathBuf) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    if s.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&s)
        .with_context(|| format!("{} is not valid JSON", path.display()))
}

fn backup(path: &PathBuf) -> Result<()> {
    if path.exists() {
        let bak = path.with_extension(format!("json.skout-bak.{}", util::now()));
        std::fs::copy(path, &bak)?;
        println!("  backed up {} -> {}", path.display(), bak.display());
    }
    Ok(())
}

/// Drop every skout-owned hook entry, leaving other tools' hooks untouched.
fn strip(settings: &mut Value) -> usize {
    let mut removed = 0;
    let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return 0;
    };
    for (_event, groups) in hooks.iter_mut() {
        let Some(arr) = groups.as_array_mut() else { continue };
        for group in arr.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                let before = list.len();
                list.retain(|h| !is_ours(h));
                removed += before - list.len();
            }
        }
        // Drop matcher groups that are now empty because they only held ours.
        arr.retain(|g| {
            g.get("hooks")
                .and_then(|h| h.as_array())
                .map(|l| !l.is_empty())
                .unwrap_or(true)
        });
    }
    let empty: Vec<String> = hooks
        .iter()
        .filter(|(_, v)| v.as_array().map(|a| a.is_empty()).unwrap_or(false))
        .map(|(k, _)| k.clone())
        .collect();
    for k in empty {
        hooks.remove(&k);
    }
    removed
}

pub fn init(project: bool, force: bool) -> Result<()> {
    let path = settings_path(project);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut settings = load(&path)?;

    let existing = strip(&mut settings);
    if existing > 0 && !force {
        println!("  replacing {existing} existing skout hook(s)");
    }

    backup(&path)?;

    let bin = bin_path();
    if !settings.get("hooks").map(|h| h.is_object()).unwrap_or(false) {
        settings["hooks"] = json!({});
    }

    for (event, matcher, sub, is_async) in EVENTS {
        let mut entry = json!({
            "type": "command",
            "command": bin,
            "args": ["hook", sub],
            "timeout": 10
        });
        if *is_async {
            entry["async"] = json!(true);
        }

        let hooks = settings["hooks"].as_object_mut().unwrap();
        let groups = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
        let arr = groups.as_array_mut().unwrap();

        // Reuse a matcher group if one already exists for this matcher.
        let slot = arr.iter_mut().find(|g| {
            g.get("matcher").and_then(|m| m.as_str()) == Some(*matcher)
        });
        match slot {
            Some(g) => {
                g["hooks"].as_array_mut().unwrap().push(entry);
            }
            None => arr.push(json!({ "matcher": matcher, "hooks": [entry] })),
        }
    }

    std::fs::write(&path, format!("{:#}\n", settings))?;
    println!("  wrote hooks to {}", path.display());

    // Config file
    let cfg_path = crate::config::path();
    if !cfg_path.exists() {
        crate::config::Config::default().save()?;
        println!("  wrote config to {}", cfg_path.display());
    } else {
        println!("  kept existing config at {}", cfg_path.display());
    }

    install_slash_command()?;
    Ok(())
}

pub fn uninstall(project: bool) -> Result<()> {
    let path = settings_path(project);
    if !path.exists() {
        println!("  nothing to do — {} does not exist", path.display());
        return Ok(());
    }
    let mut settings = load(&path)?;
    backup(&path)?;
    let n = strip(&mut settings);
    std::fs::write(&path, format!("{:#}\n", settings))?;
    println!("  removed {n} skout hook(s) from {}", path.display());
    println!("  config and history kept at {}", util::skout_dir().display());
    Ok(())
}

/// A `/skout` slash command so the report is reachable without leaving the REPL.
fn install_slash_command() -> Result<()> {
    let dir = util::claude_dir().join("commands");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("skout.md");
    let body = r#"---
description: Token usage report and guard settings for this project
allowed-tools: Bash(skout:*)
---

Run the skout command the user asked for and present the result.

- No arguments, or "report": run `skout report`
- "all": run `skout report --all`
- "today" / "week" / "month": run `skout report --today` etc.
- "config": run `skout config list`
- "set <key> <value>": run `skout config set <key> <value>`, then `skout config list`
- "off" / "on": run `skout config set enabled false` (or true)
- "doctor": run `skout doctor`
- "reset": run `skout reset --session $CLAUDE_SESSION_ID`

Arguments: $ARGUMENTS

Show the command's output as-is — it is already formatted for the terminal. Add a
one-line interpretation only if something stands out (a low cache hit rate, one
tool dominating output tokens, a guard firing repeatedly).
"#;
    std::fs::write(&path, body)?;
    println!("  wrote slash command to {}", path.display());
    Ok(())
}

pub fn doctor(cfg_err: Option<String>) -> Result<()> {
    let mut ok = true;
    println!();
    println!("  skout doctor");
    println!();

    let bin = bin_path();
    println!("  binary        {bin}");
    if !PathBuf::from(&bin).exists() {
        println!("                ! binary path does not resolve");
        ok = false;
    }

    let cfg_path = crate::config::path();
    match cfg_err {
        None => println!("  config        {} (valid)", cfg_path.display()),
        Some(e) => {
            println!("  config        {} (INVALID: {e})", cfg_path.display());
            println!("                hooks are running on built-in defaults");
            ok = false;
        }
    }

    let db = util::skout_dir().join("state.db");
    println!("  state         {} ({})", db.display(),
        if db.exists() { util::human_bytes(std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0)) } else { "not created yet".into() });

    let projects = util::claude_dir().join("projects");
    let n = std::fs::read_dir(&projects).map(|r| r.count()).unwrap_or(0);
    println!("  transcripts   {} ({n} projects)", projects.display());
    if n == 0 {
        println!("                ! no transcripts found — reports will be empty");
        ok = false;
    }

    for project in [false, true] {
        let path = settings_path(project);
        let label = if project { "project" } else { "user   " };
        if !path.exists() {
            println!("  hooks {label}  {} (absent)", path.display());
            continue;
        }
        let settings = load(&path)?;
        let mut found = Vec::new();
        if let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) {
            for (event, groups) in hooks {
                let installed = groups
                    .as_array()
                    .map(|a| {
                        a.iter().any(|g| {
                            g.get("hooks")
                                .and_then(|h| h.as_array())
                                .map(|l| l.iter().any(is_ours))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);
                if installed {
                    found.push(event.clone());
                }
            }
        }
        found.sort();
        if found.is_empty() {
            println!("  hooks {label}  none installed");
        } else {
            println!("  hooks {label}  {}", found.join(", "));
        }
    }

    println!();
    if ok {
        println!("  all good");
    } else {
        println!("  see the ! lines above");
    }
    println!();
    Ok(())
}
