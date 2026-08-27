use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Wall-clock seconds since the UNIX epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Rough token count for a blob of text.
///
/// Real tokenization needs the model's BPE table, which we deliberately do not
/// ship: every guard decision here is a threshold comparison, so being within a
/// few percent is plenty and being fast matters more. Prose runs ~4 chars/token,
/// code closer to ~3.2; `chars_per_token` is configurable for people who want to
/// calibrate against `count_tokens`.
pub fn est_tokens(bytes: usize, chars_per_token: f64) -> u64 {
    if chars_per_token <= 0.0 {
        return bytes as u64;
    }
    (bytes as f64 / chars_per_token).ceil() as u64
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// `~/.skout` — config, state DB, logs.
pub fn skout_dir() -> PathBuf {
    std::env::var("SKOUT_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".skout"))
}

pub fn claude_dir() -> PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".claude"))
}

/// Resolve a possibly-relative path against `cwd`, then canonicalize.
/// Falls back to the lexical join when the file does not exist.
pub fn resolve(path: &str, cwd: &str) -> PathBuf {
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(cwd).join(p)
    };
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

/// Claude Code mangles a project path into a transcript directory name by
/// replacing every non-alphanumeric byte with `-`.
/// `/Users/x/Desktop/api.studyportal` -> `-Users-x-Desktop-api-studyportal`
pub fn project_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub struct FileStat {
    pub size: u64,
    pub mtime_ns: i64,
    pub lines: u64,
    /// Content hash, so dedupe can catch an edit that preserved size and mtime.
    /// `None` for files too large to be worth hashing on the hook path.
    pub hash: Option<String>,
}

pub fn stat_file(path: &Path) -> Result<FileStat> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(path)?;
    let size = md.size();
    let mtime_ns = md.mtime() * 1_000_000_000 + md.mtime_nsec();
    // One read serves both the line count and the hash — the file is already
    // in the page cache, and Claude Code is about to read it anyway.
    let (lines, hash) = match std::fs::read(path) {
        Ok(b) => {
            let lines = b.iter().filter(|&&c| c == b'\n').count() as u64 + 1;
            let hash = if size < 2_000_000 {
                let mut h = Sha256::new();
                h.update(&b);
                Some(format!("{:x}", h.finalize())[..16].to_string())
            } else {
                None
            };
            (lines, hash)
        }
        Err(_) => (0, None),
    };
    Ok(FileStat { size, mtime_ns, lines, hash })
}

pub fn short_hash(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
}

/// Files where a byte-for-byte re-read is normal and cheap, or where our
/// line-based reasoning is meaningless (images, PDFs, notebooks Claude renders
/// specially). Never guard these.
pub fn is_opaque(path: &Path) -> bool {
    const OPAQUE: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg", "pdf", "zip",
        "gz", "tar", "wasm", "so", "dylib", "a", "o", "bin", "exe", "mp4", "mov",
        "mp3", "wav", "woff", "woff2", "ttf", "otf",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| OPAQUE.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn human_tokens(t: u64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.1}k", t as f64 / 1_000.0)
    } else {
        t.to_string()
    }
}

pub fn human_bytes(b: u64) -> String {
    if b >= 1_048_576 {
        format!("{:.1}MB", b as f64 / 1_048_576.0)
    } else if b >= 1024 {
        format!("{:.1}KB", b as f64 / 1024.0)
    } else {
        format!("{}B", b)
    }
}
