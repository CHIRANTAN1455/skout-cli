use serde::Deserialize;
use serde_json::{json, Value};

/// The union of every field we consume across hook events. Claude Code sends a
/// superset per event; unknown fields are ignored and absent ones default.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct HookInput {
    pub session_id: String,
    pub cwd: String,
    pub hook_event_name: String,
    pub transcript_path: String,
    pub tool_name: String,
    pub tool_input: Value,
    pub tool_result: Value,
    pub reason: String,
}

impl HookInput {
    pub fn read() -> HookInput {
        use std::io::Read;
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        serde_json::from_str(&buf).unwrap_or_default()
    }

    pub fn str_field(&self, key: &str) -> Option<String> {
        self.tool_input.get(key)?.as_str().map(|s| s.to_string())
    }

    pub fn num_field(&self, key: &str) -> Option<i64> {
        self.tool_input.get(key)?.as_i64()
    }

    /// Tool results arrive as a bare string for most tools and as a structured
    /// object for others. Measure the serialized size either way.
    pub fn result_len(&self) -> usize {
        match &self.tool_result {
            Value::Null => 0,
            Value::String(s) => s.len(),
            other => other.to_string().len(),
        }
    }
}

/// A PreToolUse verdict. `Allow` with no note emits nothing at all, which is the
/// cheapest possible outcome — zero added tokens on the overwhelmingly common
/// path where no rule fires.
pub enum Decision {
    Allow,
    Note(String),
    Deny(String),
}

pub fn emit_pre_tool_use(d: Decision) {
    let out = match d {
        Decision::Allow => return,
        Decision::Note(reason) => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "additionalContext": reason
            }
        }),
        Decision::Deny(reason) => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason
            }
        }),
    };
    println!("{out}");
}
