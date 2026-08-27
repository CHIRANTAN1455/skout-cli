//! Memory layer seam.
//!
//! Not wired into any hook in 0.1 — the shipped guards are stateless across
//! sessions by design. This exists so the mem0 backend chosen for the roadmap
//! plugs in without reshaping the crate: a `PreCompact` hook writes distilled
//! facts through `Backend::add`, and `UserPromptSubmit` reads them back through
//! `Backend::search` into `additionalContext`.
#![allow(dead_code)]

use anyhow::{bail, Result};

pub struct Fact {
    pub text: String,
    pub scope: String,
    pub score: f64,
}

pub trait Backend {
    fn add(&self, scope: &str, text: &str) -> Result<()>;
    fn search(&self, scope: &str, query: &str, top_k: u32) -> Result<Vec<Fact>>;
}

/// SQLite FTS5 over the local state DB. Zero network, zero key.
pub struct Local;

impl Backend for Local {
    fn add(&self, _scope: &str, _text: &str) -> Result<()> {
        bail!("skout memory: local backend not implemented in 0.1")
    }
    fn search(&self, _scope: &str, _query: &str, _k: u32) -> Result<Vec<Fact>> {
        Ok(vec![])
    }
}

/// mem0 cloud (`https://api.mem0.ai`) — `POST /v1/memories/` to add,
/// `POST /v1/memories/search/` to retrieve.
///
/// Deliberately unimplemented rather than half-implemented: turning this on
/// means an API key, a signup, and a network round trip on the prompt path,
/// which is a product decision that belongs with the memory feature itself.
pub struct Mem0 {
    pub api_key: String,
    pub base_url: String,
}

impl Backend for Mem0 {
    fn add(&self, _scope: &str, _text: &str) -> Result<()> {
        bail!("skout memory: mem0 backend arrives with the memory feature (roadmap)")
    }
    fn search(&self, _scope: &str, _query: &str, _k: u32) -> Result<Vec<Fact>> {
        bail!("skout memory: mem0 backend arrives with the memory feature (roadmap)")
    }
}

pub fn backend(cfg: &crate::config::Config) -> Box<dyn Backend> {
    match cfg.memory.backend.as_str() {
        "mem0" => Box::new(Mem0 {
            api_key: if cfg.memory.mem0_api_key.is_empty() {
                std::env::var("MEM0_API_KEY").unwrap_or_default()
            } else {
                cfg.memory.mem0_api_key.clone()
            },
            base_url: cfg.memory.mem0_base_url.clone(),
        }),
        _ => Box::new(Local),
    }
}
