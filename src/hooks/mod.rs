use crate::config::Config;
use crate::db;
use crate::guards;
use crate::hookio::{emit_pre_tool_use, Decision, HookInput};
use crate::util;

/// Hooks run inside someone else's editing session. The contract here is that a
/// skout failure is always silent and always non-blocking: on any error we emit
/// nothing, which Claude Code reads as "no opinion".
pub fn dispatch(event: &str) {
    let input = HookInput::read();
    let cfg = Config::load();

    if !cfg.enabled {
        return;
    }

    match event {
        "pre-tool-use" => pre_tool_use(&cfg, &input),
        "post-tool-use" => post_tool_use(&cfg, &input),
        "session-start" => session_start(&input),
        "session-end" => session_end(&input),
        _ => {}
    }
}

fn pre_tool_use(cfg: &Config, input: &HookInput) {
    let Ok(conn) = db::open() else { return };
    let decision = guards::evaluate(&conn, cfg, input);
    if let Decision::Allow = decision {
        return;
    }
    emit_pre_tool_use(decision);
}

fn post_tool_use(cfg: &Config, input: &HookInput) {
    let len = input.result_len();
    if len == 0 {
        return;
    }
    let Ok(conn) = db::open() else { return };
    let est = util::est_tokens(len, cfg.chars_per_token);
    let _ = db::record_event(
        &conn,
        &input.session_id,
        &input.cwd,
        &input.tool_name,
        len as i64,
        est as i64,
    );
}

fn session_start(input: &HookInput) {
    let Ok(conn) = db::open() else { return };
    let _ = db::start_session(&conn, &input.session_id, &input.cwd);
}

fn session_end(input: &HookInput) {
    let Ok(conn) = db::open() else { return };
    let _ = db::end_session(&conn, &input.session_id);
    // A cleared or compacted session no longer holds the earlier reads in
    // context, so the dedupe record must go with it — otherwise skout would
    // block a read of something Claude can no longer see.
    if matches!(input.reason.as_str(), "clear" | "compact") {
        let _ = db::reset_session(&conn, &input.session_id);
    }
}
