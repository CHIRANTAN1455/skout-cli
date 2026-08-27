/// Published Anthropic first-party rates, USD per 1M tokens.
/// Cache reads bill at 0.1x input; cache writes at 1.25x (5-minute TTL) or
/// 2x (1-hour TTL, which is what Claude Code uses).
#[derive(Debug, Clone, Copy)]
pub struct Price {
    pub input: f64,
    pub output: f64,
}

/// Sonnet 5 launched with promotional pricing that lapses after this date.
/// Stored as a UNIX timestamp for 2026-09-01T00:00:00Z.
const SONNET5_INTRO_ENDS: i64 = 1_756_684_800;
const SONNET5_INTRO: Price = Price { input: 2.0, output: 10.0 };

pub fn lookup(model: &str, at: i64, fast: bool) -> Price {
    let m = model.to_ascii_lowercase();

    // Fast mode reprices Opus 5 / 4.8 at Fable-tier rates.
    if fast && (m.contains("opus-5") || m.contains("opus-4-8")) {
        return Price { input: 10.0, output: 50.0 };
    }

    if m.contains("sonnet-5") {
        return if at > 0 && at < SONNET5_INTRO_ENDS {
            SONNET5_INTRO
        } else {
            Price { input: 3.0, output: 15.0 }
        };
    }
    if m.contains("fable") || m.contains("mythos") {
        return Price { input: 10.0, output: 50.0 };
    }
    if m.contains("haiku") {
        return Price { input: 1.0, output: 5.0 };
    }
    if m.contains("sonnet") {
        return Price { input: 3.0, output: 15.0 };
    }
    // Opus family, and the safe default for an unrecognised id: never
    // under-report what a session cost.
    Price { input: 5.0, output: 25.0 }
}

pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Usage {
    pub fn cost(&self, p: Price, cache_write_mult: f64) -> f64 {
        let per = 1_000_000.0;
        (self.input as f64 * p.input
            + self.output as f64 * p.output
            + self.cache_write as f64 * p.input * cache_write_mult
            + self.cache_read as f64 * p.input * 0.1)
            / per
    }

    /// What the same traffic would have cost with no prompt cache at all:
    /// every cached token billed as fresh input.
    pub fn uncached_cost(&self, p: Price) -> f64 {
        let per = 1_000_000.0;
        ((self.input + self.cache_write + self.cache_read) as f64 * p.input
            + self.output as f64 * p.output)
            / per
    }
}
