use crate::config::{Config, Mode};
use crate::hookio::{Decision, HookInput};

/// `Grep` in content mode with no `head_limit` can return every matching line
/// in a repository. The tool itself is cheap; the result is what costs.
pub fn check(cfg: &Config, input: &HookInput) -> Decision {
    if cfg.grep_guard.mode == Mode::Off {
        return Decision::Allow;
    }

    let output_mode = input
        .str_field("output_mode")
        .unwrap_or_else(|| "files_with_matches".into());
    if output_mode != "content" {
        return Decision::Allow;
    }
    if input.num_field("head_limit").is_some() {
        return Decision::Allow;
    }

    let msg = "skout: `Grep` in content mode with no `head_limit` returns every matching line, which \
               on a broad pattern can be thousands. Add `head_limit: 50` — you can always widen it \
               if the matches get truncated."
        .to_string();

    match cfg.grep_guard.mode {
        Mode::Deny => Decision::Deny(msg),
        _ => Decision::Note(msg),
    }
}
