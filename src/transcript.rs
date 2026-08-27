use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::Config;
use crate::pricing::{self, Usage};
use crate::util;

#[derive(Default, Clone)]
pub struct Bucket {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub messages: u64,
    pub cost: f64,
    pub uncached_cost: f64,
}

impl Bucket {
    pub fn total_input(&self) -> u64 {
        self.input + self.cache_write + self.cache_read
    }
    pub fn cache_hit_rate(&self) -> f64 {
        let denom = self.total_input();
        if denom == 0 { 0.0 } else { self.cache_read as f64 / denom as f64 }
    }
    fn merge(&mut self, o: &Bucket) {
        self.input += o.input;
        self.output += o.output;
        self.cache_write += o.cache_write;
        self.cache_read += o.cache_read;
        self.messages += o.messages;
        self.cost += o.cost;
        self.uncached_cost += o.uncached_cost;
    }
}

pub struct SessionRow {
    pub session_id: String,
    pub project: String,
    pub last_ts: i64,
    pub bucket: Bucket,
}

pub struct Scan {
    pub total: Bucket,
    pub by_model: HashMap<String, Bucket>,
    pub sessions: Vec<SessionRow>,
}

fn projects_dir() -> PathBuf {
    util::claude_dir().join("projects")
}

/// Collect transcript files. `slug` limits the scan to one project directory.
fn transcript_files(slug: Option<&str>) -> Vec<PathBuf> {
    let root = projects_dir();
    let dirs: Vec<PathBuf> = match slug {
        Some(s) => vec![root.join(s)],
        None => std::fs::read_dir(&root)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default(),
    };
    let mut out = Vec::new();
    for d in dirs {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                    out.push(p);
                }
            }
        }
    }
    out
}

fn parse_ts(v: Option<&Value>) -> i64 {
    v.and_then(|x| x.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

pub fn scan(cfg: &Config, slug: Option<&str>, since: i64) -> Result<Scan> {
    let mult = cfg.cache_write_multiplier();
    let mut total = Bucket::default();
    let mut by_model: HashMap<String, Bucket> = HashMap::new();
    let mut sessions: HashMap<String, SessionRow> = HashMap::new();

    for file in transcript_files(slug) {
        let project = file
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        let Ok(content) = std::fs::read_to_string(&file) else { continue };

        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            // Cheap pre-filter: only assistant turns carry a usage block, and
            // these files run to megabytes.
            if !line.contains("\"usage\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
                continue;
            }
            let Some(msg) = v.get("message") else { continue };
            let Some(usage) = msg.get("usage") else { continue };

            let ts = parse_ts(v.get("timestamp"));
            if since > 0 && ts > 0 && ts < since {
                continue;
            }

            let g = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
            let u = Usage {
                input: g("input_tokens"),
                output: g("output_tokens"),
                cache_write: g("cache_creation_input_tokens"),
                cache_read: g("cache_read_input_tokens"),
            };
            if u.input == 0 && u.output == 0 && u.cache_write == 0 && u.cache_read == 0 {
                continue;
            }

            let model = msg
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown")
                .to_string();
            let fast = usage.get("speed").and_then(|s| s.as_str()) == Some("fast");
            let price = pricing::lookup(&model, ts, fast);

            let b = Bucket {
                input: u.input,
                output: u.output,
                cache_write: u.cache_write,
                cache_read: u.cache_read,
                messages: 1,
                cost: u.cost(price, mult),
                uncached_cost: u.uncached_cost(price),
            };

            total.merge(&b);
            by_model.entry(model).or_default().merge(&b);

            let sid = v
                .get("sessionId")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            let row = sessions.entry(sid.clone()).or_insert_with(|| SessionRow {
                session_id: sid,
                project: project.clone(),
                last_ts: 0,
                bucket: Bucket::default(),
            });
            row.bucket.merge(&b);
            row.last_ts = row.last_ts.max(ts);
        }
    }

    let mut sessions: Vec<SessionRow> = sessions.into_values().collect();
    sessions.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));

    Ok(Scan { total, by_model, sessions })
}
