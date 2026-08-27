use rusqlite::Connection;

use crate::config::{Config, Mode};
use crate::db;
use crate::hookio::{Decision, HookInput};
use crate::util;

/// Claude Code's Read takes a 1-based line `offset` and a line `limit`.
/// A missing offset means "from line 1"; a missing limit means "to EOF".
struct Range {
    start: u64,
    end: u64,
}

const EOF: u64 = u64::MAX;

fn range(off: Option<i64>, lim: Option<i64>) -> Range {
    let start = off.filter(|v| *v > 0).map(|v| v as u64).unwrap_or(1);
    let end = match lim {
        Some(l) if l > 0 => start.saturating_add(l as u64).saturating_sub(1),
        _ => EOF,
    };
    Range { start, end }
}

fn covers(prior: &Range, want: &Range) -> bool {
    prior.start <= want.start && prior.end >= want.end
}

fn rel(path: &std::path::Path, cwd: &str) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

pub fn check(conn: &Connection, cfg: &Config, input: &HookInput) -> Decision {
    let Some(raw) = input.str_field("file_path") else {
        return Decision::Allow;
    };
    let abs = util::resolve(&raw, &input.cwd);
    let abs_str = abs.to_string_lossy().to_string();

    if util::is_opaque(&abs) || cfg.is_ignored(&abs_str) {
        return Decision::Allow;
    }

    // If we cannot stat it, let Read run and produce the real error. A guard
    // must never be the thing that reports "file not found".
    let Ok(st) = util::stat_file(&abs) else {
        return Decision::Allow;
    };
    if st.lines == 0 {
        return Decision::Allow;
    }

    let off = input.num_field("offset");
    let lim = input.num_field("limit");
    let want = range(off, lim);

    let bytes_per_line = (st.size as f64 / st.lines as f64).max(1.0);
    let want_lines = if want.end == EOF {
        st.lines.saturating_sub(want.start - 1)
    } else {
        (want.end - want.start + 1).min(st.lines)
    };
    let est = util::est_tokens(
        (want_lines as f64 * bytes_per_line) as usize,
        cfg.chars_per_token,
    );

    // --- Rule 1: already in context, unchanged ------------------------------
    if cfg.dedupe.mode != Mode::Off {
        if let Ok(priors) = db::prior_reads(conn, &input.session_id, &abs_str) {
            for p in &priors {
                // Size and mtime agreeing is the fast path; when both sides
                // carry a hash, it is the one that actually decides.
                let stamps_match =
                    p.size == st.size as i64 && p.mtime_ns == st.mtime_ns;
                let unchanged = stamps_match
                    && match (&p.hash, &st.hash) {
                        (Some(a), Some(b)) => a == b,
                        _ => true,
                    };
                if unchanged && covers(&range(p.off, p.lim), &want) {
                    let fp = util::short_hash(&format!(
                        "dedupe|{abs_str}|{:?}|{:?}",
                        off, lim
                    ));
                    let n = db::bump_deny_count(conn, &input.session_id, &fp)
                        .unwrap_or(1);

                    // Escape hatch: the model asked again after being told no.
                    // Assume it has a reason we cannot see and get out of the way.
                    if n > cfg.dedupe.max_denials {
                        let _ = db::record_denial(
                            conn, &input.session_id, &input.cwd, "dedupe",
                            "Read", &abs_str, 0, false,
                        );
                        return Decision::Allow;
                    }

                    let _ = db::record_denial(
                        conn, &input.session_id, &input.cwd, "dedupe",
                        "Read", &abs_str, p.est_tokens, true,
                    );

                    if cfg.dedupe.mode == Mode::Warn {
                        return Decision::Note(format!(
                            "skout: {} is already in this conversation (read {} ago, unchanged since).",
                            rel(&abs, &input.cwd),
                            ago(util::now() - p.created_at)
                        ));
                    }
                    return Decision::Deny(format!(
                        "skout: `{}` is already in your context. You read it {} ago and the file has not \
                         changed since (identical size and mtime). Re-reading adds ~{} tokens and returns \
                         byte-for-byte what is already above — scroll back to that earlier Read result \
                         instead.\n\nIf you need a different part of the file, pass `offset`/`limit` for a \
                         range you have not read yet. If you genuinely need a fresh copy, issue the exact \
                         same Read again and skout will allow it through.",
                        rel(&abs, &input.cwd),
                        ago(util::now() - p.created_at),
                        util::human_tokens(p.est_tokens.max(0) as u64)
                    ));
                }
            }
        }
    }

    // --- Rule 2: unbounded read of a large file -----------------------------
    if cfg.big_read.mode != Mode::Off
        && off.is_none()
        && lim.is_none()
        && st.lines > cfg.big_read.max_lines
    {
        let fp = util::short_hash(&format!("bigread|{abs_str}"));
        let n = db::bump_deny_count(conn, &input.session_id, &fp).unwrap_or(1);

        if n > cfg.big_read.max_denials {
            let _ = db::record_denial(
                conn, &input.session_id, &input.cwd, "big_read", "Read",
                &abs_str, 0, false,
            );
            record(conn, cfg, input, &abs_str, off, lim, &st, est);
            return Decision::Allow;
        }

        let _ = db::record_denial(
            conn, &input.session_id, &input.cwd, "big_read", "Read", &abs_str,
            est as i64, true,
        );

        let msg = format!(
            "skout: `{}` is {} lines (~{} tokens, {}) and you asked for all of it. That much raw file \
             stays in context for the rest of the session and is re-sent on every following turn.\n\n\
             Cheaper options, in order:\n\
             1. `Grep` the file for the symbol or string you actually need — it returns matching lines \
             with line numbers.\n\
             2. `Read` a page at a time: `offset: 1, limit: {}` and continue from where it lands.\n\n\
             If you really do need the whole file, repeat this exact call and skout will allow it.",
            rel(&abs, &input.cwd),
            st.lines,
            util::human_tokens(est),
            util::human_bytes(st.size),
            cfg.big_read.max_lines
        );

        if cfg.big_read.mode == Mode::Warn {
            return Decision::Note(msg);
        }
        return Decision::Deny(msg);
    }

    record(conn, cfg, input, &abs_str, off, lim, &st, est);
    Decision::Allow
}

/// Book-keep the read now, synchronously, rather than in PostToolUse. The
/// dedupe rule is only correct if the record lands before the next PreToolUse
/// fires, and PostToolUse runs async.
fn record(
    conn: &Connection,
    _cfg: &Config,
    input: &HookInput,
    abs: &str,
    off: Option<i64>,
    lim: Option<i64>,
    st: &util::FileStat,
    est: u64,
) {
    let _ = db::record_read(
        conn,
        &input.session_id,
        abs,
        off,
        lim,
        st.mtime_ns,
        st.size as i64,
        st.hash.as_deref(),
        est as i64,
    );
}

fn ago(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}
