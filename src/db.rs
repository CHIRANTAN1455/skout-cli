use anyhow::Result;
use rusqlite::{params, Connection};

use crate::util;

pub fn open() -> Result<Connection> {
    let dir = util::skout_dir();
    std::fs::create_dir_all(&dir)?;
    let conn = Connection::open(dir.join("state.db"))?;
    // Hooks fire concurrently (PostToolUse runs async), so a writer must wait
    // rather than fail. WAL + a busy timeout keeps them out of each other's way.
    conn.busy_timeout(std::time::Duration::from_millis(3000))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS reads (
            id          INTEGER PRIMARY KEY,
            session_id  TEXT NOT NULL,
            path        TEXT NOT NULL,
            off         INTEGER,
            lim         INTEGER,
            mtime_ns    INTEGER NOT NULL,
            size        INTEGER NOT NULL,
            hash        TEXT,
            est_tokens  INTEGER NOT NULL DEFAULT 0,
            created_at  INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_reads_lookup ON reads(session_id, path);

        CREATE TABLE IF NOT EXISTS tool_events (
            id            INTEGER PRIMARY KEY,
            session_id    TEXT NOT NULL,
            cwd           TEXT NOT NULL,
            tool          TEXT NOT NULL,
            result_bytes  INTEGER NOT NULL,
            est_tokens    INTEGER NOT NULL,
            created_at    INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_events_cwd ON tool_events(cwd, created_at);

        CREATE TABLE IF NOT EXISTS denials (
            id           INTEGER PRIMARY KEY,
            session_id   TEXT NOT NULL,
            cwd          TEXT NOT NULL,
            rule         TEXT NOT NULL,
            tool         TEXT NOT NULL,
            target       TEXT NOT NULL,
            saved_tokens INTEGER NOT NULL,
            enforced     INTEGER NOT NULL DEFAULT 1,
            created_at   INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_denials_cwd ON denials(cwd, created_at);

        CREATE TABLE IF NOT EXISTS deny_counts (
            session_id  TEXT NOT NULL,
            fingerprint TEXT NOT NULL,
            n           INTEGER NOT NULL,
            PRIMARY KEY (session_id, fingerprint)
        );

        CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            cwd        TEXT NOT NULL,
            started_at INTEGER NOT NULL,
            ended_at   INTEGER
        );
        "#,
    )?;
    Ok(())
}

pub struct PriorRead {
    pub off: Option<i64>,
    pub lim: Option<i64>,
    pub mtime_ns: i64,
    pub size: i64,
    pub hash: Option<String>,
    pub est_tokens: i64,
    pub created_at: i64,
}

pub fn record_read(
    conn: &Connection,
    session: &str,
    path: &str,
    off: Option<i64>,
    lim: Option<i64>,
    mtime_ns: i64,
    size: i64,
    hash: Option<&str>,
    est_tokens: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO reads (session_id, path, off, lim, mtime_ns, size, hash, est_tokens, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![session, path, off, lim, mtime_ns, size, hash, est_tokens, util::now()],
    )?;
    Ok(())
}

pub fn prior_reads(conn: &Connection, session: &str, path: &str) -> Result<Vec<PriorRead>> {
    let mut stmt = conn.prepare(
        "SELECT off, lim, mtime_ns, size, hash, est_tokens, created_at
           FROM reads WHERE session_id = ?1 AND path = ?2 ORDER BY created_at DESC",
    )?;
    let rows = stmt
        .query_map(params![session, path], |r| {
            Ok(PriorRead {
                off: r.get(0)?,
                lim: r.get(1)?,
                mtime_ns: r.get(2)?,
                size: r.get(3)?,
                hash: r.get(4)?,
                est_tokens: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn record_event(
    conn: &Connection,
    session: &str,
    cwd: &str,
    tool: &str,
    result_bytes: i64,
    est_tokens: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO tool_events (session_id, cwd, tool, result_bytes, est_tokens, created_at)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![session, cwd, tool, result_bytes, est_tokens, util::now()],
    )?;
    Ok(())
}

pub fn record_denial(
    conn: &Connection,
    session: &str,
    cwd: &str,
    rule: &str,
    tool: &str,
    target: &str,
    saved_tokens: i64,
    enforced: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO denials (session_id, cwd, rule, tool, target, saved_tokens, enforced, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![session, cwd, rule, tool, target, saved_tokens, enforced as i64, util::now()],
    )?;
    Ok(())
}

/// Increment and return how many times this exact call has already been blocked
/// in this session. Drives the anti-deadlock escape hatch.
pub fn bump_deny_count(conn: &Connection, session: &str, fingerprint: &str) -> Result<u32> {
    conn.execute(
        "INSERT INTO deny_counts (session_id, fingerprint, n) VALUES (?1,?2,1)
         ON CONFLICT(session_id, fingerprint) DO UPDATE SET n = n + 1",
        params![session, fingerprint],
    )?;
    let n: u32 = conn.query_row(
        "SELECT n FROM deny_counts WHERE session_id = ?1 AND fingerprint = ?2",
        params![session, fingerprint],
        |r| r.get(0),
    )?;
    Ok(n)
}

pub fn start_session(conn: &Connection, session: &str, cwd: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (session_id, cwd, started_at) VALUES (?1,?2,?3)
         ON CONFLICT(session_id) DO NOTHING",
        params![session, cwd, util::now()],
    )?;
    Ok(())
}

pub fn end_session(conn: &Connection, session: &str) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET ended_at = ?2 WHERE session_id = ?1",
        params![session, util::now()],
    )?;
    Ok(())
}

/// Wipe per-session read state. Used by `skout reset`.
pub fn reset_session(conn: &Connection, session: &str) -> Result<usize> {
    let n = conn.execute("DELETE FROM reads WHERE session_id = ?1", params![session])?;
    conn.execute("DELETE FROM deny_counts WHERE session_id = ?1", params![session])?;
    Ok(n)
}
