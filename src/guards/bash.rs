use rusqlite::Connection;

use crate::config::{Config, Mode};
use crate::db;
use crate::hookio::{Decision, HookInput};
use crate::util;

/// Split a command line into independently-executed segments. This is a
/// heuristic, not a shell parser — it only needs to be good enough to find the
/// leading utility of each segment.
fn segments(cmd: &str) -> Vec<&str> {
    cmd.split(|c| c == ';' || c == '\n')
        .flat_map(|s| s.split("&&"))
        .flat_map(|s| s.split("||"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Does this segment already bound its own output? Anything piped into a
/// limiter or a filter is assumed to be under control.
fn is_bounded(seg: &str) -> bool {
    let Some((_, downstream)) = seg.split_once('|') else {
        return false;
    };
    const LIMITERS: &[&str] = &[
        "head", "tail", "wc", "grep", "rg", "sed", "awk", "jq", "cut", "uniq",
        "select", "first",
    ];
    LIMITERS.iter().any(|l| {
        downstream
            .split_whitespace()
            .any(|w| w.trim_start_matches("./") == *l)
    })
}

fn words(seg: &str) -> Vec<String> {
    seg.split('|')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string())
        .collect()
}

fn has_flag(w: &[String], flags: &[&str]) -> bool {
    w.iter().any(|x| flags.iter().any(|f| x == f || x.starts_with(&format!("{f}="))))
}

pub fn check(conn: &Connection, cfg: &Config, input: &HookInput) -> Decision {
    if cfg.bash_guard.mode == Mode::Off {
        return Decision::Allow;
    }
    let Some(cmd) = input.str_field("command") else {
        return Decision::Allow;
    };

    let mut notes: Vec<String> = Vec::new();

    for seg in segments(&cmd) {
        if is_bounded(seg) {
            continue;
        }
        let w = words(seg);
        let Some(head) = w.first().map(|s| s.trim_start_matches("./").to_string()) else {
            continue;
        };
        let args: Vec<&String> = w.iter().skip(1).filter(|a| !a.starts_with('-')).collect();

        match head.as_str() {
            // `cat file` — the single most common way a big blob lands in context.
            "cat" | "bat" => {
                for a in &args {
                    let abs = util::resolve(a, &input.cwd);
                    let abs_str = abs.to_string_lossy().to_string();
                    if util::is_opaque(&abs) || cfg.is_ignored(&abs_str) {
                        continue;
                    }
                    let Ok(st) = util::stat_file(&abs) else { continue };

                    // Already pulled into context by a Read this session?
                    if let Ok(priors) = db::prior_reads(conn, &input.session_id, &abs_str) {
                        if priors.iter().any(|p| {
                            p.size == st.size as i64 && p.mtime_ns == st.mtime_ns
                                && p.off.is_none() && p.lim.is_none()
                        }) {
                            notes.push(format!(
                                "`{a}` was already read in full earlier this session and has not changed — \
                                 the contents are above, no need to cat it again."
                            ));
                            continue;
                        }
                    }

                    if st.lines > cfg.bash_guard.max_lines {
                        let est = util::est_tokens(st.size as usize, cfg.chars_per_token);
                        notes.push(format!(
                            "`cat {a}` will dump {} lines (~{} tokens) into context. Prefer \
                             `sed -n '1,200p' {a}` to page it, or `rg <pattern> {a}` if you are \
                             looking for something specific.",
                            st.lines,
                            util::human_tokens(est)
                        ));
                    }
                }
            }
            "find" => {
                if !has_flag(&w, &["-maxdepth"]) && !seg.contains("-prune") {
                    notes.push(
                        "`find` without `-maxdepth` walks the entire tree and can return thousands \
                         of paths. Add `-maxdepth 3`, or use `rg --files -g <glob>`."
                            .into(),
                    );
                }
            }
            "tree" => {
                if !has_flag(&w, &["-L"]) {
                    notes.push("`tree` without `-L` prints the whole tree. Add `-L 2`.".into());
                }
            }
            "ls" => {
                if w.iter().any(|x| x.starts_with('-') && x.contains('R')) {
                    notes.push(
                        "`ls -R` recurses without bound. Use `find . -maxdepth 2` or `rg --files | head -50`."
                            .into(),
                    );
                }
            }
            "git" => {
                let sub = w.get(1).map(|s| s.as_str()).unwrap_or("");
                match sub {
                    "log" => {
                        let limited = w.iter().any(|x| {
                            x == "-n" || x.starts_with("-n")
                                || (x.starts_with('-') && x[1..].chars().all(|c| c.is_ascii_digit()) && x.len() > 1)
                                || x == "--oneline"
                                || x.starts_with("--max-count")
                        });
                        if !limited {
                            notes.push(
                                "`git log` prints the full history. Add `-n 20 --oneline` unless you \
                                 need the whole log."
                                    .into(),
                            );
                        }
                    }
                    "diff" | "show" => {
                        if !has_flag(&w, &["--stat", "--name-only", "--name-status", "--numstat"])
                            && args.len() <= 1
                        {
                            notes.push(format!(
                                "`git {sub}` with no pathspec can be enormous. Run `git {sub} --stat` \
                                 first, then diff only the files you care about."
                            ));
                        }
                    }
                    _ => {}
                }
            }
            "npm" | "pnpm" | "yarn" => {
                let sub = w.get(1).map(|s| s.as_str()).unwrap_or("");
                if (sub == "ls" || sub == "list") && !has_flag(&w, &["--depth"]) {
                    notes.push(format!(
                        "`{head} {sub}` prints the whole dependency tree. Add `--depth=0`."
                    ));
                }
            }
            _ => {}
        }
    }

    if notes.is_empty() {
        return Decision::Allow;
    }

    let est_saved: i64 = 0;
    let _ = db::record_denial(
        conn,
        &input.session_id,
        &input.cwd,
        "bash_guard",
        "Bash",
        &cmd.chars().take(120).collect::<String>(),
        est_saved,
        cfg.bash_guard.mode == Mode::Deny,
    );

    let body = format!("skout: {}", notes.join("\n\nskout: "));
    match cfg.bash_guard.mode {
        Mode::Deny => Decision::Deny(format!(
            "{body}\n\nRe-run with the cheaper form above, or repeat this exact command to override."
        )),
        _ => Decision::Note(body),
    }
}
