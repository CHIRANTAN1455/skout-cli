pub mod bash;
pub mod grep;
pub mod read;

use rusqlite::Connection;

use crate::config::Config;
use crate::hookio::{Decision, HookInput};

pub fn evaluate(conn: &Connection, cfg: &Config, input: &HookInput) -> Decision {
    match input.tool_name.as_str() {
        "Read" => read::check(conn, cfg, input),
        "Bash" => bash::check(conn, cfg, input),
        "Grep" => grep::check(cfg, input),
        _ => Decision::Allow,
    }
}
