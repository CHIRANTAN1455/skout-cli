# skout

Token-aware guard rails and cost analytics for [Claude Code](https://claude.com/claude-code).

A single ~2.7 MB Rust binary. No runtime, no API key, no network calls, no telemetry
leaving your machine.

---

## The problem

Claude Code sessions do not get expensive because the model is verbose. They get
expensive because **tool output accumulates in context and is re-sent on every
subsequent turn**.

Read a 5,000-line file once and it is not a one-time cost — it sits in the context
window for the rest of the session, billed as a cache read on every turn that
follows. Read it *twice* and you paid to put identical bytes in there twice.

A representative 30-day window on a real machine:

```
fresh input     6.1k     0%
cache write    10.6M     2%
cache read    581.9M    98%   <-- context being re-sent, turn after turn
```

98% of all input tokens were re-reads of context already established. The prompt
cache makes that cheap (0.1x input rate), not free. Keeping junk out of the window
in the first place is the only thing that actually reduces it.

skout sits on Claude Code's hook interface and does three things:

1. **Blocks re-reads.** If a file is already in context and has not changed, the
   second `Read` is refused with a pointer to the first one.
2. **Blocks unbounded reads.** A `Read` with no `offset`/`limit` on a very large
   file is refused with a cheaper alternative (grep it, or page it).
3. **Measures everything.** Parses your own transcripts for real token counts and
   real cost, so the savings claim is auditable rather than asserted.

---

## Install

```sh
git clone <repo> && cd skout
cargo build --release
./target/release/skout init
```

`init` writes four hooks into `~/.claude/settings.json` (backing up the existing
file first), creates `~/.skout/config.toml`, and installs a `/skout` slash command.
Start a new Claude Code session to pick them up.

To install for one project only, use `skout init --project`, which writes to
`./.claude/settings.json` instead.

Removal is symmetric and surgical — `skout uninstall` strips only skout's own hook
entries and leaves every other setting and third-party hook untouched.

---

## Commands

| Command | What it does |
|---|---|
| `skout init [--project]` | Install hooks + config + `/skout` slash command |
| `skout uninstall [--project]` | Remove hooks, keep config and history |
| `skout report` | Token usage, cost, cache hit rate, and what skout saved |
| `skout report --all` | Every project instead of the current directory |
| `skout report --today\|--week\|--month\|--ever` | Time window (default: last 7 days) |
| `skout report --json` | Machine-readable output |
| `skout config list\|get\|set\|path` | Read and change settings |
| `skout doctor` | Verify the installation |
| `skout reset [--session ID\|--all]` | Forget which files are already in context |

Inside Claude Code, `/skout`, `/skout today`, `/skout config`, `/skout off`.

---

## Configuration

`skout config set <key> <value>`, stored in `~/.skout/config.toml`.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Master switch; `false` neutralises every hook without editing settings.json |
| `chars_per_token` | `3.6` | Divisor for byte→token estimates (prose ~4, code ~3.2) |
| `cache_ttl` | `1h` | Claude Code's cache TTL. Sets the cache-write multiplier used in cost maths |
| `dedupe.mode` | `deny` | `deny` / `warn` / `off` — re-read guard |
| `dedupe.max_denials` | `1` | Blocks per identical call before the escape hatch opens |
| `big_read.mode` | `deny` | `deny` / `warn` / `off` — unbounded large-file read guard |
| `big_read.max_lines` | `1500` | Line count above which an unbounded `Read` is refused |
| `bash_guard.mode` | `warn` | `cat`/`find`/`git log`/`npm ls` and friends |
| `grep_guard.mode` | `warn` | `Grep` in content mode with no `head_limit` |
| `ignore` | *(empty)* | Comma-separated globs exempt from every guard |

Every guard defaults conservatively. `bash_guard` and `grep_guard` **warn rather
than deny** because a shell command can do anything, and a false positive that
blocks real work costs far more than the tokens it saves.

---

## How it works

| Hook | Matcher | Async | Job |
|---|---|---|---|
| `PreToolUse` | `Read\|Bash\|Grep` | no | Evaluate guards, allow / warn / deny |
| `PostToolUse` | `*` | yes | Record real result size per tool |
| `SessionStart` | `*` | yes | Register the session |
| `SessionEnd` | `*` | yes | Close it; clear read state on `clear`/`compact` |

Only `PreToolUse` is synchronous, because only it can change a decision. It costs
**~8.5 ms** per tool call. Everything else runs `async: true` and never sits in the
critical path.

### Design rules

**A guard must never be the thing that breaks your session.** Three mechanisms
enforce that:

- *Escape hatch.* Every rule counts denials per identical call. Once the count
  exceeds `max_denials` (default 1), the call is allowed through. The refusal text
  tells Claude this explicitly, so a genuinely-needed read costs one extra turn,
  never a deadlock.
- *Fail open.* Unparseable stdin, a missing file, an unreadable config, a locked
  database — every failure path emits nothing at all, which Claude Code reads as
  "no opinion". A broken skout is an absent skout.
- *Never guess about correctness.* Dedupe fires only when size **and** mtime match,
  **and** the content hash agrees when both sides have one. A file edited in place
  with size and mtime preserved is still detected as changed.

Context is tracked per session and cleared on `/clear` and on compaction — after
those, Claude genuinely cannot see the earlier read, so blocking it would be wrong.

### Cost accounting

Rates are Anthropic's published first-party per-token prices. Cache reads bill at
0.1x input; cache writes at 1.25x (5-minute TTL) or **2x (1-hour TTL, which is what
Claude Code uses)**. Sonnet 5's introductory pricing is applied by message
timestamp. Fast-mode turns are repriced. An unrecognised model id falls back to
Opus rates, so cost is never under-reported.

The `saved` figure values a blocked read at the cache-write rate for its estimated
tokens. That is a deliberate **floor**: the real saving is larger, because those
tokens would also have been re-sent as a cache read on every following turn.

---

## Roadmap

**v0.2 — memory.** The largest remaining source of waste is re-exploration after
compaction: Claude rediscovers what it already learned. A `PreCompact` hook will
distill the session into durable facts and `UserPromptSubmit` will inject the
relevant ones back. `src/memory.rs` defines the `Backend` trait and the config
surface; [mem0](https://mem0.ai) is the intended cloud backend, with a local SQLite
FTS5 implementation as the zero-dependency default.

Injection happens at the *tail* of the conversation, which preserves the cached
prefix. Injecting into the system prompt instead would invalidate the prompt cache
on every turn and cost far more than it saves.

---

## License

MIT
