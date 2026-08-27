use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Block the call. Claude sees the reason and is expected to retry cheaply.
    Deny,
    /// Let it through but attach a note nudging the cheaper form.
    Warn,
    /// Rule disabled.
    Off,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Mode> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "deny" => Mode::Deny,
            "warn" => Mode::Warn,
            "off" => Mode::Off,
            other => bail!("invalid mode '{other}' (expected deny|warn|off)"),
        })
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Deny => "deny",
            Mode::Warn => "warn",
            Mode::Off => "off",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Master switch. `skout config set enabled false` neutralises every hook
    /// without touching settings.json.
    pub enabled: bool,

    /// Divisor turning bytes into an estimated token count.
    pub chars_per_token: f64,

    /// Claude Code caches with a 1-hour TTL, which costs 2x base input to write
    /// (vs 1.25x for the 5-minute default). Cost maths depends on this.
    pub cache_ttl: String,

    pub dedupe: Dedupe,
    pub big_read: BigRead,
    pub bash_guard: BashGuard,
    pub grep_guard: GrepGuard,

    /// Glob patterns exempt from every guard.
    pub ignore: Vec<String>,

    /// Memory layer. Inert in 0.1 — the backend trait and mem0 client are
    /// wired but no hook consumes them yet.
    pub memory: Memory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Dedupe {
    pub mode: Mode,
    /// After this many identical denials in one session, stop blocking and let
    /// the call through. Without this a determined model can deadlock.
    pub max_denials: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BigRead {
    pub mode: Mode,
    /// Unbounded Read of a file longer than this is blocked with a suggestion
    /// to page it or grep it.
    pub max_lines: u64,
    pub max_denials: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BashGuard {
    /// Defaults to Warn, not Deny: a shell command can do anything, and a false
    /// positive that blocks real work is worse than the tokens it saves.
    pub mode: Mode,
    pub max_lines: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GrepGuard {
    pub mode: Mode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    pub enabled: bool,
    /// "local" | "mem0"
    pub backend: String,
    /// Read from MEM0_API_KEY when blank.
    pub mem0_api_key: String,
    pub mem0_base_url: String,
    pub top_k: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            chars_per_token: 3.6,
            cache_ttl: "1h".into(),
            dedupe: Dedupe::default(),
            big_read: BigRead::default(),
            bash_guard: BashGuard::default(),
            grep_guard: GrepGuard::default(),
            ignore: vec![],
            memory: Memory::default(),
        }
    }
}

impl Default for Dedupe {
    fn default() -> Self {
        Dedupe { mode: Mode::Deny, max_denials: 1 }
    }
}
impl Default for BigRead {
    fn default() -> Self {
        BigRead { mode: Mode::Deny, max_lines: 1500, max_denials: 1 }
    }
}
impl Default for BashGuard {
    fn default() -> Self {
        BashGuard { mode: Mode::Warn, max_lines: 1500 }
    }
}
impl Default for GrepGuard {
    fn default() -> Self {
        GrepGuard { mode: Mode::Warn }
    }
}
impl Default for Memory {
    fn default() -> Self {
        Memory {
            enabled: false,
            backend: "local".into(),
            mem0_api_key: String::new(),
            mem0_base_url: "https://api.mem0.ai".into(),
            top_k: 6,
        }
    }
}

pub fn path() -> PathBuf {
    util::skout_dir().join("config.toml")
}

impl Config {
    pub fn load() -> Config {
        // A broken config must never brick the hooks — fall back to defaults
        // and let `skout doctor` surface the problem.
        std::fs::read_to_string(path())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn load_strict() -> Result<Config> {
        let p = path();
        if !p.exists() {
            return Ok(Config::default());
        }
        let s = std::fs::read_to_string(&p)?;
        toml::from_str(&s).with_context(|| format!("parsing {}", p.display()))
    }

    pub fn save(&self) -> Result<()> {
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn cache_write_multiplier(&self) -> f64 {
        if self.cache_ttl == "5m" {
            1.25
        } else {
            2.0
        }
    }

    pub fn is_ignored(&self, path: &str) -> bool {
        self.ignore.iter().any(|pat| {
            glob::Pattern::new(pat).map(|p| p.matches(path)).unwrap_or(false)
        })
    }

    pub fn get(&self, key: &str) -> Result<String> {
        Ok(match key {
            "enabled" => self.enabled.to_string(),
            "chars_per_token" => self.chars_per_token.to_string(),
            "cache_ttl" => self.cache_ttl.clone(),
            "dedupe.mode" => self.dedupe.mode.as_str().into(),
            "dedupe.max_denials" => self.dedupe.max_denials.to_string(),
            "big_read.mode" => self.big_read.mode.as_str().into(),
            "big_read.max_lines" => self.big_read.max_lines.to_string(),
            "big_read.max_denials" => self.big_read.max_denials.to_string(),
            "bash_guard.mode" => self.bash_guard.mode.as_str().into(),
            "bash_guard.max_lines" => self.bash_guard.max_lines.to_string(),
            "grep_guard.mode" => self.grep_guard.mode.as_str().into(),
            "ignore" => self.ignore.join(","),
            "memory.enabled" => self.memory.enabled.to_string(),
            "memory.backend" => self.memory.backend.clone(),
            "memory.mem0_api_key" => {
                if self.memory.mem0_api_key.is_empty() { "(unset)".into() }
                else { "(set)".into() }
            }
            "memory.top_k" => self.memory.top_k.to_string(),
            other => bail!("unknown key '{other}' (try `skout config list`)"),
        })
    }

    pub fn set(&mut self, key: &str, val: &str) -> Result<()> {
        match key {
            "enabled" => self.enabled = val.parse()?,
            "chars_per_token" => self.chars_per_token = val.parse()?,
            "cache_ttl" => {
                if val != "1h" && val != "5m" {
                    bail!("cache_ttl must be '1h' or '5m'");
                }
                self.cache_ttl = val.into();
            }
            "dedupe.mode" => self.dedupe.mode = Mode::parse(val)?,
            "dedupe.max_denials" => self.dedupe.max_denials = val.parse()?,
            "big_read.mode" => self.big_read.mode = Mode::parse(val)?,
            "big_read.max_lines" => self.big_read.max_lines = val.parse()?,
            "big_read.max_denials" => self.big_read.max_denials = val.parse()?,
            "bash_guard.mode" => self.bash_guard.mode = Mode::parse(val)?,
            "bash_guard.max_lines" => self.bash_guard.max_lines = val.parse()?,
            "grep_guard.mode" => self.grep_guard.mode = Mode::parse(val)?,
            "ignore" => {
                self.ignore = val
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            "memory.enabled" => self.memory.enabled = val.parse()?,
            "memory.backend" => {
                if val != "local" && val != "mem0" {
                    bail!("memory.backend must be 'local' or 'mem0'");
                }
                self.memory.backend = val.into();
            }
            "memory.mem0_api_key" => self.memory.mem0_api_key = val.into(),
            "memory.top_k" => self.memory.top_k = val.parse()?,
            other => bail!("unknown key '{other}' (try `skout config list`)"),
        }
        Ok(())
    }

    pub const KEYS: &'static [&'static str] = &[
        "enabled",
        "chars_per_token",
        "cache_ttl",
        "dedupe.mode",
        "dedupe.max_denials",
        "big_read.mode",
        "big_read.max_lines",
        "big_read.max_denials",
        "bash_guard.mode",
        "bash_guard.max_lines",
        "grep_guard.mode",
        "ignore",
        "memory.enabled",
        "memory.backend",
        "memory.mem0_api_key",
        "memory.top_k",
    ];
}
