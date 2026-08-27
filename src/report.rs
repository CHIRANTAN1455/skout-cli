use anyhow::Result;
use rusqlite::{params, Connection};

use crate::config::Config;
use crate::transcript;
use crate::util;

struct Style {
    on: bool,
}
impl Style {
    fn new() -> Style {
        Style { on: std::env::var("NO_COLOR").is_err() && atty() }
    }
    fn d(&self, s: &str) -> String { self.wrap(s, "2") }
    fn b(&self, s: &str) -> String { self.wrap(s, "1") }
    fn g(&self, s: &str) -> String { self.wrap(s, "32") }
    fn y(&self, s: &str) -> String { self.wrap(s, "33") }
    fn wrap(&self, s: &str, code: &str) -> String {
        if self.on { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
    }
}

fn atty() -> bool {
    unsafe { libc_isatty() }
}
// Avoid a libc dependency for one call.
unsafe fn libc_isatty() -> bool {
    extern "C" { fn isatty(fd: i32) -> i32; }
    isatty(1) == 1
}

fn bar(frac: f64, width: usize, s: &Style) -> String {
    let filled = ((frac.clamp(0.0, 1.0)) * width as f64).round() as usize;
    let f = "█".repeat(filled);
    let e = "░".repeat(width.saturating_sub(filled));
    format!("{}{}", s.g(&f), s.d(&e))
}

pub struct Opts {
    pub scope_all: bool,
    pub cwd: String,
    pub since: i64,
    pub window_label: String,
    pub json: bool,
}

pub fn run(conn: &Connection, cfg: &Config, o: Opts) -> Result<()> {
    let slug = util::project_slug(&o.cwd);
    let scan = transcript::scan(cfg, if o.scope_all { None } else { Some(&slug) }, o.since)?;
    let saved = skout_savings(conn, cfg, &o)?;

    if o.json {
        println!("{}", serde_json::to_string_pretty(&payload(&scan, &saved, cfg, &o, conn)?)?);
        return Ok(());
    }

    let s = Style::new();
    let t = &scan.total;

    println!();
    println!("  {} {}", s.b("skout"), s.d(&format!("· {} · {}",
        if o.scope_all { "all projects".to_string() } else { short_path(&o.cwd) },
        o.window_label)));
    println!();

    if t.messages == 0 {
        println!("  {}", s.d("No assistant turns found in this window."));
        println!("  {}", s.d("Transcripts live in ~/.claude/projects — try --all or a wider window."));
        println!();
        return Ok(());
    }

    // --- tokens ---------------------------------------------------------
    println!("  {}", s.b("TOKENS"));
    let total_in = t.total_input();
    row(&s, "fresh input", t.input, total_in);
    row(&s, "cache write", t.cache_write, total_in);
    row(&s, "cache read", t.cache_read, total_in);
    println!("  {:>13}  {}", s.d("output"), s.b(&util::human_tokens(t.output)));
    println!("  {:>13}  {}  {}", s.d("total in"), s.b(&util::human_tokens(total_in)),
        s.d(&format!("across {} assistant turns", t.messages)));
    println!();

    // --- cache ----------------------------------------------------------
    let hr = t.cache_hit_rate();
    println!("  {}", s.b("CACHE"));
    println!("  {:>13}  {} {}", s.d("hit rate"), bar(hr, 24, &s), s.b(&format!("{:.0}%", hr * 100.0)));
    let cache_saved = t.uncached_cost - t.cost;
    println!("  {:>13}  {} {}", s.d("saved"), s.g(&format!("${:.2}", cache_saved.max(0.0))),
        s.d(&format!("vs ${:.2} with no prompt cache", t.uncached_cost)));
    println!();

    // --- cost -----------------------------------------------------------
    println!("  {}", s.b("COST"));
    println!("  {:>13}  {}", s.d("actual"), s.b(&format!("${:.2}", t.cost)));
    let mut models: Vec<_> = scan.by_model.iter().collect();
    models.sort_by(|a, b| b.1.cost.partial_cmp(&a.1.cost).unwrap());
    for (m, b) in models.iter().take(4) {
        println!("  {:>13}  {}  {}", s.d(&trim_model(m)), s.d(&format!("${:.2}", b.cost)),
            s.d(&util::human_tokens(b.total_input())));
    }
    println!();

    // --- skout ----------------------------------------------------------
    println!("  {}", s.b("SKOUT"));
    if saved.enforced == 0 && saved.overridden == 0 {
        println!("  {}", s.d("No guard has fired yet in this window."));
    } else {
        println!("  {:>13}  {} {}", s.d("blocked"), s.b(&saved.enforced.to_string()),
            s.d("redundant or oversized calls"));
        println!("  {:>13}  {} {}", s.d("saved"), s.g(&util::human_tokens(saved.tokens as u64)),
            s.g(&format!("(~${:.2})", saved.usd)));
        if saved.overridden > 0 {
            println!("  {:>13}  {} {}", s.d("overridden"), s.y(&saved.overridden.to_string()),
                s.d("let through on retry"));
        }
        for (rule, n, tok) in &saved.by_rule {
            if *n == 0 { continue; }
            println!("  {:>13}  {} {}", s.d(rule), s.d(&format!("{n} blocked")),
                s.d(&format!("~{} tokens", util::human_tokens(*tok as u64))));
        }
    }
    println!();

    // --- top tools ------------------------------------------------------
    let tools = top_tools(conn, &o)?;
    if !tools.is_empty() {
        println!("  {}", s.b("TOP TOOLS BY OUTPUT"));
        let max = tools.first().map(|x| x.2).unwrap_or(1).max(1);
        for (tool, calls, tok) in tools.iter().take(6) {
            println!("  {:>13}  {} {} {}", s.d(tool),
                bar(*tok as f64 / max as f64, 16, &s),
                s.b(&util::human_tokens(*tok as u64)),
                s.d(&format!("{calls} calls")));
        }
        println!();
    }

    // --- sessions -------------------------------------------------------
    if scan.sessions.len() > 1 {
        println!("  {}", s.b("RECENT SESSIONS"));
        for r in scan.sessions.iter().take(5) {
            let where_ = if o.scope_all { format!(" · {}", trim_project(&r.project)) } else { String::new() };
            println!("  {:>13}  {} {}", s.d(&r.session_id[..8.min(r.session_id.len())]),
                s.b(&format!("${:.2}", r.bucket.cost)),
                s.d(&format!("{} · {:.0}% cached{}", util::human_tokens(r.bucket.total_input()),
                    r.bucket.cache_hit_rate() * 100.0, where_)));
        }
        println!();
    }

    Ok(())
}

fn row(s: &Style, label: &str, v: u64, total: u64) {
    let frac = if total == 0 { 0.0 } else { v as f64 / total as f64 };
    println!("  {:>13}  {} {} {}", s.d(label), bar(frac, 24, s),
        s.b(&util::human_tokens(v)), s.d(&format!("{:.0}%", frac * 100.0)));
}

fn trim_model(m: &str) -> String {
    m.strip_prefix("claude-").unwrap_or(m).to_string()
}

/// Transcript directory names are the project path with separators mangled to
/// `-`; the tail is the only readable part.
fn trim_project(slug: &str) -> String {
    slug.rsplit('-').next().unwrap_or(slug).to_string()
}

fn short_path(p: &str) -> String {
    let home = util::home().to_string_lossy().to_string();
    p.strip_prefix(&home).map(|r| format!("~{r}")).unwrap_or_else(|| p.to_string())
}

pub struct Savings {
    pub enforced: i64,
    pub overridden: i64,
    pub tokens: i64,
    pub usd: f64,
    pub by_rule: Vec<(String, i64, i64)>,
}

fn skout_savings(conn: &Connection, cfg: &Config, o: &Opts) -> Result<Savings> {
    let (where_cwd, arg): (&str, String) = if o.scope_all {
        ("1=1", String::new())
    } else {
        ("cwd = ?2", o.cwd.clone())
    };

    let sql = format!(
        "SELECT rule, COUNT(*), COALESCE(SUM(saved_tokens),0), COALESCE(SUM(enforced),0)
           FROM denials WHERE created_at >= ?1 AND {where_cwd} GROUP BY rule"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, i64, i64, i64)> = if o.scope_all {
        stmt.query_map(params![o.since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map(params![o.since, arg], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut enforced = 0;
    let mut total_calls = 0;
    let mut tokens = 0;
    let mut by_rule = Vec::new();
    for (rule, n, tok, enf) in rows {
        enforced += enf;
        total_calls += n;
        tokens += tok;
        // Report the blocks that actually stuck, not every time the rule
        // matched — the overridden ones are counted separately.
        by_rule.push((rule, enf, tok));
    }
    by_rule.sort_by(|a, b| b.2.cmp(&a.2));

    // A blocked read would have entered context as a cache write, then been
    // re-sent as a cache read on every later turn. Valuing it at the write rate
    // alone is the conservative floor.
    let price = crate::pricing::lookup("claude-opus-5", util::now(), false);
    let usd = tokens as f64 * price.input * cfg.cache_write_multiplier() / 1_000_000.0;

    Ok(Savings {
        enforced,
        overridden: total_calls - enforced,
        tokens,
        usd,
        by_rule,
    })
}

fn top_tools(conn: &Connection, o: &Opts) -> Result<Vec<(String, i64, i64)>> {
    let sql = if o.scope_all {
        "SELECT tool, COUNT(*), SUM(est_tokens) FROM tool_events
          WHERE created_at >= ?1 GROUP BY tool ORDER BY 3 DESC"
    } else {
        "SELECT tool, COUNT(*), SUM(est_tokens) FROM tool_events
          WHERE created_at >= ?1 AND cwd = ?2 GROUP BY tool ORDER BY 3 DESC"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = if o.scope_all {
        stmt.query_map(params![o.since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map(params![o.since, o.cwd.clone()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

/// The full shape behind `--json` and the `serve` dashboard. Both read the same
/// numbers, so a screenshot of the UI and a piped report can never disagree.
pub fn payload(
    scan: &transcript::Scan,
    saved: &Savings,
    cfg: &Config,
    o: &Opts,
    conn: &Connection,
) -> Result<serde_json::Value> {
    let t = &scan.total;

    let mut models: Vec<_> = scan.by_model.iter().collect();
    models.sort_by(|a, b| b.1.cost.partial_cmp(&a.1.cost).unwrap());
    let models: Vec<_> = models
        .iter()
        .map(|(m, b)| {
            serde_json::json!({
                "model": trim_model(m),
                "cost_usd": b.cost,
                "total_input": b.total_input(),
                "output": b.output,
                "turns": b.messages,
                "cache_hit_rate": b.cache_hit_rate(),
            })
        })
        .collect();

    let days: Vec<_> = scan
        .by_day
        .iter()
        .map(|(d, b)| {
            serde_json::json!({
                "day": d,
                "cost_usd": b.cost,
                "uncached_cost_usd": b.uncached_cost,
                "total_input": b.total_input(),
                "output": b.output,
                "turns": b.messages,
                "cache_hit_rate": b.cache_hit_rate(),
            })
        })
        .collect();

    let mut projects: Vec<_> = scan.by_project.iter().collect();
    projects.sort_by(|a, b| b.1.cost.partial_cmp(&a.1.cost).unwrap());
    let projects: Vec<_> = projects
        .iter()
        .take(8)
        .map(|(p, b)| {
            serde_json::json!({
                "project": trim_project(p),
                "cost_usd": b.cost,
                "total_input": b.total_input(),
                "turns": b.messages,
            })
        })
        .collect();

    let sessions: Vec<_> = scan
        .sessions
        .iter()
        .take(8)
        .map(|r| {
            serde_json::json!({
                "session_id": r.session_id[..8.min(r.session_id.len())].to_string(),
                "project": trim_project(&r.project),
                "last_ts": r.last_ts,
                "cost_usd": r.bucket.cost,
                "total_input": r.bucket.total_input(),
                "cache_hit_rate": r.bucket.cache_hit_rate(),
            })
        })
        .collect();

    let tools: Vec<_> = top_tools(conn, o)?
        .iter()
        .take(8)
        .map(|(tool, calls, tok)| {
            serde_json::json!({ "tool": tool, "calls": calls, "est_tokens": tok })
        })
        .collect();

    let rules: Vec<_> = saved
        .by_rule
        .iter()
        .map(|(rule, n, tok)| serde_json::json!({ "rule": rule, "blocked": n, "tokens": tok }))
        .collect();

    let guards: Vec<_> = ["dedupe.mode", "big_read.mode", "bash_guard.mode", "grep_guard.mode"]
        .iter()
        .map(|k| {
            serde_json::json!({
                "guard": k.trim_end_matches(".mode"),
                "mode": cfg.get(k).unwrap_or_else(|_| "?".into()),
            })
        })
        .collect();

    Ok(serde_json::json!({
        "scope": if o.scope_all { "all" } else { o.cwd.as_str() },
        "scope_label": if o.scope_all { "all projects".to_string() } else { short_path(&o.cwd) },
        "window": o.window_label,
        "generated_at": util::now(),
        "enabled": cfg.enabled,
        "tokens": {
            "fresh_input": t.input,
            "cache_write": t.cache_write,
            "cache_read": t.cache_read,
            "output": t.output,
            "total_input": t.total_input(),
        },
        "cost_usd": t.cost,
        "cost_without_cache_usd": t.uncached_cost,
        "cache_saved_usd": (t.uncached_cost - t.cost).max(0.0),
        "cache_hit_rate": t.cache_hit_rate(),
        "assistant_turns": t.messages,
        "skout": {
            "blocks_enforced": saved.enforced,
            "blocks_overridden": saved.overridden,
            "tokens_saved": saved.tokens,
            "usd_saved": saved.usd,
            "by_rule": rules,
        },
        "guards": guards,
        "by_model": models,
        "by_day": days,
        "by_project": projects,
        "sessions": sessions,
        "top_tools": tools,
    }))
}

/// Assemble the payload from scratch — the entry point `serve` uses.
pub fn collect(conn: &Connection, cfg: &Config, o: &Opts) -> Result<serde_json::Value> {
    let slug = util::project_slug(&o.cwd);
    let scan = transcript::scan(cfg, if o.scope_all { None } else { Some(&slug) }, o.since)?;
    let saved = skout_savings(conn, cfg, o)?;
    payload(&scan, &saved, cfg, o, conn)
}
