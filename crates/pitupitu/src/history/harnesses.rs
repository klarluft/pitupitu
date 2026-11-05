//! The logs of the harnesses besides Claude Code and Codex: Kiro CLI, pi
//! and Qwen Code (JSONL, read on line by line as Claude Code's and Codex's
//! are), Cline (one JSON document it rewrites, read whole), OpenCode and
//! Kilo Code (a SQLite database, a session read whole), Antigravity and
//! Maki (JSONL, read whole each time). Each gives the
//! same turns: what the person typed, the agent's text, and a tool call's
//! name and one target. Never thinking, tool results or file contents.

use super::*;
use crate::logs;

/// A whole log larger than this (Cline's) is not read.
const WHOLE_MAX: u64 = 64 * 1024 * 1024;

/// A time in ms since the epoch as RFC 3339 with milliseconds, UTC.
pub(super) fn rfc3339_ms(ms: u64) -> String {
    let s = inventory::rfc3339(SystemTime::UNIX_EPOCH + Duration::from_millis(ms));
    format!("{}.{:03}Z", s.trim_end_matches('Z'), ms % 1000)
}

/// The text of a message's content: a string, or its `text` blocks.
fn texts(content: &Value) -> Vec<&str> {
    match content {
        Value::String(s) => vec![s.as_str()],
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect(),
        _ => Vec::new(),
    }
}

/// What the person typed, as Cline logs it: wrapped in
/// `<user_input mode="…">`. Anything else Cline put in the person's place
/// (context, a tool's feedback) is not what was typed.
fn cline_you(text: &str) -> Option<String> {
    let t = text.trim();
    for tag in ["user_input", "task", "feedback"] {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        if let Some(rest) = t.strip_prefix(&open) {
            if !rest.starts_with([' ', '>']) {
                continue;
            }
            let start = rest.find('>')? + 1;
            let inner = &rest[start..];
            let inner = inner.find(&close).map_or(inner, |end| &inner[..end]);
            return Some(inner.trim().to_string());
        }
    }
    None
}

/// A Cline tool call's target: the first of `run_commands`' commands,
/// else as for every tool.
fn cline_target(name: &str, input: &Value) -> Option<String> {
    if name == "run_commands" {
        let first = input["commands"]
            .as_array()
            .and_then(|c| c.first())
            .and_then(command_text);
        return first.and_then(|c| one_line(&c, TARGET_MAX));
    }
    target(name, input)
}

/// What the person typed, as Antigravity logs it: inside `<USER_REQUEST>`,
/// with the metadata it adds after that.
fn agy_you(text: &str) -> Option<String> {
    const OPEN: &str = "<USER_REQUEST>";
    let inner = match text.find(OPEN) {
        Some(i) => {
            let rest = &text[i + OPEN.len()..];
            rest.find("</USER_REQUEST>")
                .map_or(rest, |end| &rest[..end])
        }
        None => {
            let added = ["<ADDITIONAL_METADATA>", "<USER_SETTINGS_CHANGE>"]
                .iter()
                .filter_map(|t| text.find(t))
                .min();
            added.map_or(text, |end| &text[..end])
        }
    };
    Some(inner.trim().to_string())
}

/// An Antigravity tool call's target, from the arguments that name a file,
/// a command or a search; any other argument can hold file contents
/// (`Content`, `Replacement`, `Target`) and is left alone.
fn agy_target(args: &Value) -> Option<String> {
    ["Path", "CommandLine", "query"]
        .iter()
        .find_map(|k| args[*k].as_str())
        .and_then(|s| one_line(s, TARGET_MAX))
}

/// Maki's title for a session it has not titled yet.
const MAKI_NO_TITLE: &str = "New session";
/// What Maki writes in place of a reply that came back empty.
const MAKI_EMPTY: &str = "(empty)";

/// Whether a line of a Maki log is one the reader looks at: not tool
/// output (`out`), a subagent's message (`sub_msg`) or the system prompt
/// (`frame`).
pub(super) fn maki_reads(line: &str) -> bool {
    ["header", "msg", "meta"]
        .iter()
        .any(|t| line.starts_with(&format!(r#"{{"t":"{t}""#)))
}

/// OpenCode's title for a session it has not titled yet.
fn placeholder_title(t: &str) -> bool {
    t.starts_with("New session - ") || t.starts_with("Child session - ")
}

impl Parsed {
    /// A line of Kiro's `messages.jsonl`: `user`, `assistant` (`Say`, not
    /// `Reasoning`) and `tool_call` records; tool results, approvals and
    /// Kiro's own records are skipped.
    pub(super) fn kiro(&mut self, v: &Value) {
        let p = &v["payload"];
        let at = v["timestamp"].as_str().unwrap_or_default().to_string();
        match p["type"].as_str().unwrap_or_default() {
            "user" => {
                for t in texts(&p["content"]) {
                    self.you(&at, Some(t.to_string()));
                }
            }
            "assistant" if p["operationType"] == "Say" => {
                for t in texts(&p["content"]) {
                    self.said(&at, t);
                }
            }
            "tool_call" => {
                let name = p["toolName"].as_str().unwrap_or("tool");
                let args = &p["args"];
                let t = match p["kind"].as_str() {
                    Some("execute") => args
                        .get("command")
                        .and_then(command_text)
                        .and_then(|c| one_line(&c, TARGET_MAX)),
                    Some("edit" | "read" | "delete" | "move") => p["filePath"]
                        .as_str()
                        .or_else(|| args["path"].as_str())
                        .and_then(|f| one_line(f, TARGET_MAX)),
                    _ => target(name, args),
                };
                self.tool(&at, name, t);
            }
            _ => return,
        }
        self.meta(None, None, Some(&at));
    }

    /// Kiro's `session.json`: the session's id, folder and title.
    pub(super) fn kiro_session(&mut self, messages: &Path) {
        if let Some(m) = logs::kiro_meta(messages) {
            self.meta(m.id.as_deref(), m.cwd.as_deref(), None);
            if let Some(t) = m.title.as_deref().and_then(title_of) {
                self.title = Some(t);
            }
        }
    }

    /// A line of a pi session: the `session` header (id, folder), then
    /// `message` entries: the person's (`user`, and `bashExecution` as
    /// `! command` without its output), the agent's (`assistant`: `text`
    /// and `toolCall` blocks, not `thinking`). Tool results and system
    /// messages are skipped.
    pub(super) fn pi(&mut self, v: &Value) {
        let at = v["timestamp"].as_str().unwrap_or_default().to_string();
        match v["type"].as_str().unwrap_or_default() {
            "session" => {
                self.meta(v["id"].as_str(), v["cwd"].as_str(), Some(&at));
            }
            "message" => {
                let m = &v["message"];
                match m["role"].as_str().unwrap_or_default() {
                    "user" => {
                        for t in texts(&m["content"]) {
                            self.you(&at, Some(t.to_string()));
                        }
                    }
                    "bashExecution" => {
                        let cmd = m["command"].as_str().map(|c| format!("! {c}"));
                        self.you(&at, cmd);
                    }
                    "assistant" => {
                        for b in m["content"].as_array().into_iter().flatten() {
                            match b["type"].as_str() {
                                Some("text") => {
                                    if let Some(s) = b["text"].as_str() {
                                        self.said(&at, s);
                                    }
                                }
                                Some("toolCall") => {
                                    let name = b["name"].as_str().unwrap_or("tool");
                                    self.tool(&at, name, target(name, &b["arguments"]));
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => return,
                }
                self.meta(None, None, Some(&at));
            }
            _ => {}
        }
    }

    /// A line of a Qwen Code chat: `user` records the person typed
    /// (`real_user`) and `assistant` records (`text` parts that are not
    /// thoughts, `functionCall` parts). Tool results, telemetry and other
    /// system records, and the background memory extractor's records are
    /// skipped.
    pub(super) fn qwen(&mut self, v: &Value) {
        let ty = v["type"].as_str().unwrap_or_default();
        if ty != "user" && ty != "assistant" {
            return;
        }
        let background = v["promptId"]
            .as_str()
            .is_some_and(|p| p.contains("#managed"));
        if background || v["isSidechain"] == true {
            return;
        }
        if ty == "user" && v["provenance"].as_str().is_some_and(|p| p != "real_user") {
            return;
        }
        self.meta(
            v["sessionId"].as_str(),
            v["cwd"].as_str(),
            v["timestamp"].as_str(),
        );
        let at = v["timestamp"].as_str().unwrap_or_default().to_string();
        for part in v["message"]["parts"].as_array().into_iter().flatten() {
            if part["thought"] == true {
                continue;
            }
            if let Some(text) = part["text"].as_str() {
                if ty == "user" {
                    self.you(&at, Some(text.to_string()));
                } else {
                    self.said(&at, text);
                }
            } else if let (Some(call), "assistant") = (part.get("functionCall"), ty) {
                let name = call["name"].as_str().unwrap_or("tool");
                self.tool(&at, name, target(name, &call["args"]));
            }
        }
    }

    /// A line of Antigravity's `transcript.jsonl`: what the person typed
    /// (`USER_INPUT` from `USER_EXPLICIT`), the agent's `content` and
    /// `tool_calls` (`PLANNER_RESPONSE`). `thinking`, tool results
    /// (`GENERIC`) and Antigravity's own messages are skipped.
    pub(super) fn agy(&mut self, v: &Value) {
        let at = v["created_at"].as_str().unwrap_or_default().to_string();
        match v["type"].as_str().unwrap_or_default() {
            "USER_INPUT" if v["source"] == "USER_EXPLICIT" => {
                self.you(&at, v["content"].as_str().and_then(agy_you));
            }
            "PLANNER_RESPONSE" => {
                if let Some(text) = v["content"].as_str() {
                    self.said(&at, text);
                }
                for call in v["tool_calls"].as_array().into_iter().flatten() {
                    let name = call["name"].as_str().unwrap_or("tool");
                    self.tool(&at, name, agy_target(&call["args"]));
                }
            }
            _ => return,
        }
        self.meta(None, None, Some(&at));
    }

    /// The conversation's id, from the transcript's folder, and its folder
    /// and title from Antigravity's summaries.
    pub(super) fn agy_session(&mut self, transcript: &Path) {
        self.meta(logs::agy_id(transcript).as_deref(), None, None);
        if let Some(m) = logs::agy_meta(transcript) {
            self.meta(None, m.cwd.as_deref(), None);
            if let Some(t) = m.title.as_deref().and_then(title_of) {
                self.title = Some(t);
            }
        }
    }

    /// A line of a Maki session: the `header` (folder, start), `meta`
    /// records (title, last written; the last one counts) and `msg`
    /// records. Of those only `turn`s: the host's observations and context
    /// updates are skipped, as are prompts it made up (an empty
    /// `display_text`). The person's words are `display_text`, else the
    /// `text` blocks; the agent's are `text` and `tool_use` blocks, not
    /// `thinking`, and tool results are not read. Maki's messages carry no
    /// time, so a turn has the last time the log gave before it. The id is
    /// the file's name, the spelling `maki --resume` takes.
    pub(super) fn maki(&mut self, v: &Value) {
        let at_of = |key: &str| {
            v[key]
                .as_u64()
                .filter(|s| *s > 0)
                .map(|s| rfc3339_ms(s * 1000))
        };
        match v["t"].as_str().unwrap_or_default() {
            "header" => self.meta(None, v["cwd"].as_str(), at_of("created_at").as_deref()),
            "meta" => {
                self.meta(None, None, at_of("updated_at").as_deref());
                let title = v["title"].as_str().filter(|t| *t != MAKI_NO_TITLE);
                if let Some(t) = title.and_then(title_of) {
                    self.title = Some(t);
                }
            }
            "msg" => {
                let d = &v["d"];
                if !(d["kind"].is_null() || d["kind"] == "turn") {
                    return;
                }
                let at = self.last_at.clone().unwrap_or_default();
                let blocks = d["content"].as_array().into_iter().flatten();
                match d["role"].as_str().unwrap_or_default() {
                    "user" => match d["display_text"].as_str() {
                        Some(shown) => self.you(&at, Some(shown.to_string())),
                        None => {
                            for t in texts(&d["content"]) {
                                self.you(&at, Some(t.to_string()));
                            }
                        }
                    },
                    "assistant" => {
                        for b in blocks {
                            match b["type"].as_str() {
                                Some("text") => {
                                    if let Some(s) = b["text"].as_str().filter(|s| *s != MAKI_EMPTY)
                                    {
                                        self.said(&at, s);
                                    }
                                }
                                Some("tool_use") => {
                                    let name = b["name"].as_str().unwrap_or("tool");
                                    self.tool(&at, name, target(name, &b["input"]));
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Cline's `<id>.messages.json`, whole, with the id, folder and title
    /// from its `<id>.json`: the person's `<user_input>` texts, the
    /// agent's `text` and `tool_use` blocks; not `thinking`, not
    /// `tool_result`.
    pub(super) fn read_cline(&mut self, file: &Path) -> std::io::Result<()> {
        if std::fs::metadata(file)?.len() > WHOLE_MAX {
            return Err(std::io::Error::other("log too large"));
        }
        let v: Value = serde_json::from_slice(&std::fs::read(file)?)?;
        let meta = logs::cline_meta(file).unwrap_or_default();
        self.meta(
            v["sessionId"].as_str().or(meta.id.as_deref()),
            meta.cwd.as_deref(),
            None,
        );
        if let Some(t) = meta.title.as_deref().and_then(title_of) {
            self.title = Some(t);
        }
        for m in v["messages"].as_array().into_iter().flatten() {
            let at = m["ts"].as_u64().map(rfc3339_ms).unwrap_or_default();
            let blocks: Vec<Value> = match &m["content"] {
                Value::String(s) => vec![json!({"type": "text", "text": s})],
                Value::Array(a) => a.clone(),
                _ => continue,
            };
            match m["role"].as_str().unwrap_or_default() {
                "user" => {
                    for b in &blocks {
                        if b["type"] == "text" {
                            let typed = b["text"].as_str().and_then(cline_you);
                            self.you(&at, typed);
                        }
                    }
                }
                "assistant" => {
                    for b in &blocks {
                        match b["type"].as_str() {
                            Some("text") => {
                                if let Some(s) = b["text"].as_str() {
                                    self.said(&at, s);
                                }
                            }
                            Some("tool_use") => {
                                let name = b["name"].as_str().unwrap_or("tool");
                                self.tool(&at, name, cline_target(name, &b["input"]));
                            }
                            _ => {}
                        }
                    }
                }
                _ => continue,
            }
            if !at.is_empty() {
                self.meta(None, None, Some(&at));
            }
        }
        Ok(())
    }

    /// One session of OpenCode's or Kilo Code's database, named
    /// `<database>#<id>`: the person's `text` parts and the agent's `text`
    /// and `tool` parts (the tool and its input; its output is not even
    /// taken out of the database). Parts the harness added (`synthetic`,
    /// `ignored`), reasoning and step records are skipped.
    pub(super) fn read_db(&mut self, name: &Path) -> std::io::Result<()> {
        let not_found = || std::io::Error::from(std::io::ErrorKind::NotFound);
        let (db, id) = logs::split_db_path(name).ok_or_else(not_found)?;
        let s = logs::db_session(&db, &id)
            .map_err(std::io::Error::other)?
            .ok_or_else(not_found)?;
        let created = (s.created > 0).then(|| rfc3339_ms(s.created));
        self.meta(Some(&id), s.cwd.as_deref(), created.as_deref());
        if let Some(t) = s
            .title
            .as_deref()
            .filter(|t| !placeholder_title(t))
            .and_then(title_of)
        {
            self.title = Some(t);
        }
        for part in &s.parts {
            let at = rfc3339_ms(part.at);
            match (part.role.as_str(), part.kind.as_str()) {
                ("user", "text") if !part.added => self.you(&at, part.text.clone()),
                ("assistant", "text") if !part.added => {
                    if let Some(t) = &part.text {
                        self.said(&at, t);
                    }
                }
                ("assistant", "tool") => {
                    let name = part.tool.as_deref().unwrap_or("tool");
                    self.tool(&at, name, target(name, &part.input));
                }
                _ => continue,
            }
            self.meta(None, None, Some(&at));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[Value]) -> String {
        v.iter().map(|v| v.to_string() + "\n").collect()
    }

    fn put(home: &Path, rel: &str, text: &str) -> PathBuf {
        let p = home.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn turns(p: &Parsed) -> Vec<String> {
        p.turns
            .iter()
            .map(|t| match &t.kind {
                Kind::You { text } => format!("you: {text}"),
                Kind::Agent { text } => format!("agent: {text}"),
                Kind::Tool { name, target } => {
                    format!("tool: {name} {}", target.as_deref().unwrap_or("-"))
                }
            })
            .collect()
    }

    /// What must never be in what is sent: each fixture carries these
    /// markers in its thinking, tool results, file contents, the
    /// harness's own additions and keys.
    const NEVER: &[&str] = &[
        "MUSING",
        "TOOL OUTPUT",
        "FILE BODY",
        "ADDED CONTEXT",
        "MEMORY",
        "SYSTEM PROMPT",
        "abcdefghijklmnop1234",
    ];

    fn never_sent(p: &Parsed) {
        let sent =
            serde_json::to_string(&p.turns).unwrap() + p.title.as_deref().unwrap_or_default();
        for marker in NEVER {
            assert!(!sent.contains(marker), "{marker} in {sent}");
        }
    }

    fn read(agent: &'static str, path: &Path) -> Parsed {
        let mut p = Parsed::new(agent);
        p.load(path).unwrap();
        p
    }

    /// An Antigravity conversation of two prompts. Invented throughout, in
    /// the shape of real transcripts.
    fn agy_log() -> String {
        let step = |i: u32, ty: &str, source: &str, rest: Value| {
            let mut v = json!({"step_index": i, "type": ty, "source": source, "status": "DONE",
                "created_at": format!("2026-10-12T10:00:{i:02}Z")});
            if let (Value::Object(v), Value::Object(r)) = (&mut v, rest) {
                v.extend(r);
            }
            v
        };
        let meta = "<ADDITIONAL_METADATA>\nADDED CONTEXT\n</ADDITIONAL_METADATA>";
        lines(&[
            step(
                0,
                "USER_INPUT",
                "USER_EXPLICIT",
                json!({"content":
                format!("<USER_REQUEST>\nWater the ferns, key=sk-abcdefghijklmnop1234\n</USER_REQUEST>\n{meta}\n<USER_SETTINGS_CHANGE>ADDED CONTEXT</USER_SETTINGS_CHANGE>")}),
            ),
            step(
                1,
                "SYSTEM_MESSAGE",
                "SYSTEM",
                json!({"content": "SYSTEM PROMPT"}),
            ),
            step(
                2,
                "PLANNER_RESPONSE",
                "MODEL",
                json!({"thinking": "MUSING", "tool_calls": [
                    {"name": "run_command", "args": {"CommandLine": "ls ferns", "WaitMsBeforeAsync": 500, "toolSummary": "List"}},
                    {"name": "read_file", "args": {"Path": "/garden/ferns.md"}},
                    {"name": "write_file", "args": {"Content": "FILE BODY", "Path": "/garden/log.md"}},
                    {"name": "edit_file", "args": {"Path": "/garden/a.md", "Replacement": "FILE BODY", "Target": "FILE BODY"}},
                    {"name": "search_web", "args": {"domain": "", "query": "fern care"}},
                    {"name": "ask_question", "args": {"questions": [{"text": "ADDED CONTEXT"}]}}
                ]}),
            ),
            step(
                3,
                "GENERIC",
                "MODEL",
                json!({"content": "TOOL OUTPUT", "truncated_fields": ["content"]}),
            ),
            step(
                4,
                "PLANNER_RESPONSE",
                "MODEL",
                json!({"thinking": "MUSING", "content": "The ferns are watered."}),
            ),
            step(
                5,
                "ERROR_MESSAGE",
                "SYSTEM",
                json!({"content": "TOOL OUTPUT", "error": "TOOL OUTPUT"}),
            ),
            step(
                6,
                "USER_INPUT",
                "USER_EXPLICIT",
                json!({"content": format!("<USER_REQUEST>Thanks</USER_REQUEST>{meta}")}),
            ),
            step(
                7,
                "PLANNER_RESPONSE",
                "MODEL",
                json!({"content": "Glad to help."}),
            ),
        ])
    }

    #[test]
    fn an_antigravity_conversation_becomes_turns() {
        let home = tempfile::tempdir().unwrap();
        let id = "0a1b2c3d-1111-4222-8333-444455556666";
        logs::tests::make_agy_summaries(
            &home.path().join(".gemini/antigravity-cli"),
            &[(id, "Water the ferns", r#"["file:///garden"]"#, "", 0)],
        );
        let log = put(
            home.path(),
            &format!(".gemini/antigravity-cli/brain/{id}/.system_generated/logs/transcript.jsonl"),
            &agy_log(),
        );
        let p = read("agy", &log);
        assert_eq!(p.id.as_deref(), Some(id));
        assert_eq!(p.cwd.as_deref(), Some("/garden"));
        assert_eq!(p.title.as_deref(), Some("Water the ferns"));
        assert_eq!(p.started_at.as_deref(), Some("2026-10-12T10:00:00Z"));
        assert_eq!(p.last_at.as_deref(), Some("2026-10-12T10:00:07Z"));
        assert_eq!(
            turns(&p),
            [
                "you: Water the ferns, key=[redacted]",
                "tool: run_command ls ferns",
                "tool: read_file /garden/ferns.md",
                "tool: write_file /garden/log.md",
                "tool: edit_file /garden/a.md",
                "tool: search_web fern care",
                "tool: ask_question -",
                "agent: The ferns are watered.",
                "you: Thanks",
                "agent: Glad to help.",
            ]
        );
        never_sent(&p);
        let r = last_reply(&log, "agy").unwrap();
        assert_eq!((r.text.as_str(), r.turn), ("Glad to help.", 2));
        let session = herdr::AgentSession {
            agent: "agy".into(),
            kind: "id".into(),
            value: id.into(),
        };
        assert_eq!(
            find_log(home.path(), "agy", Some(&session), Path::new("/garden")),
            Ok((log, "agy"))
        );
    }

    #[test]
    fn what_antigravity_adds_to_a_prompt_is_not_what_was_typed() {
        assert_eq!(agy_you("plain words").as_deref(), Some("plain words"));
        assert_eq!(
            agy_you("no tag <ADDITIONAL_METADATA>x</ADDITIONAL_METADATA>").as_deref(),
            Some("no tag")
        );
        assert_eq!(
            agy_you("<USER_REQUEST>not closed").as_deref(),
            Some("not closed")
        );
    }

    /// A Maki session, invented, in the record shapes of Maki's source and
    /// its own test fixture; never checked against a file Maki wrote.
    fn maki_log() -> String {
        let msg = |d: Value| json!({"t": "msg", "d": d});
        lines(&[
            json!({"t": "header", "v": 3, "id": "3vQB7B6MrGQZaxCuFg4oh", "model": "x/y",
                   "cwd": "/meadow", "created_at": 1_791_800_000u64}),
            json!({"t": "frame", "d": {"system": "SYSTEM PROMPT"}}),
            msg(
                json!({"role": "user", "kind": "context_update", "facts": {"date": "2026-10-12"},
                       "content": [{"type": "text", "text": "ADDED CONTEXT"}]}),
            ),
            msg(
                json!({"role": "user", "kind": {"context_update": {"date": "2026-10-12"}},
                       "content": [{"type": "text", "text": "ADDED CONTEXT"}]}),
            ),
            msg(json!({"role": "user", "kind": "observation",
                       "content": [{"type": "text", "text": "ADDED CONTEXT"}]})),
            msg(json!({"role": "user", "display_text": "",
                       "content": [{"type": "text", "text": "ADDED CONTEXT"}]})),
            msg(json!({"role": "user", "content": [{"type": "text", "text": "Mow the meadow"}]})),
            json!({"t": "meta", "title": "New session", "token_usage": null, "updated_at": 1_791_800_010u64}),
            msg(json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "MUSING"},
                {"type": "redacted_thinking", "data": "MUSING"},
                {"type": "text", "text": "Mowing."},
                {"type": "tool_use", "id": "k1", "name": "bash", "input": {"command": "mow --all"}},
                {"type": "tool_use", "id": "k2", "name": "write", "input": {"path": "/meadow/log.md", "content": "FILE BODY"}},
                {"type": "server_tool_use", "id": "k3", "name": "web_search", "input": {"query": "MEMORY"}}
            ]})),
            msg(json!({"role": "user", "display_text": "", "content": [
                {"type": "tool_result", "tool_use_id": "k1", "content": "TOOL OUTPUT"}]})),
            json!({"t": "out", "id": "k1", "d": {"text": "TOOL OUTPUT"}}),
            json!({"t": "sub_msg", "sub": "s1", "d": {"role": "user", "content": [{"type": "text", "text": "MEMORY"}]}}),
            msg(json!({"role": "assistant", "content": [{"type": "text", "text": "(empty)"}]})),
            msg(
                json!({"role": "user", "display_text": "Now the verge", "content": [
                {"type": "text", "text": "Now the verge ADDED CONTEXT"}]}),
            ),
            msg(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]})),
            json!({"t": "meta", "title": "Mowing the meadow", "token_usage": null, "updated_at": 1_791_800_020u64}),
        ])
    }

    #[test]
    fn a_maki_session_becomes_turns() {
        let home = tempfile::tempdir().unwrap();
        let log = put(
            home.path(),
            ".local/state/maki/sessions/3vQB7B6MrGQZaxCuFg4oh.jsonl",
            &maki_log(),
        );
        let p = read("maki", &log);
        // The file's name gives the id.
        assert_eq!(p.id, None);
        assert_eq!(p.cwd.as_deref(), Some("/meadow"));
        assert_eq!(p.title.as_deref(), Some("Mowing the meadow"));
        assert_eq!(p.started_at, Some(rfc3339_ms(1_791_800_000_000)));
        assert_eq!(p.last_at, Some(rfc3339_ms(1_791_800_020_000)));
        assert_eq!(
            turns(&p),
            [
                "you: Mow the meadow",
                "agent: Mowing.",
                "tool: bash mow --all",
                "tool: write /meadow/log.md",
                "you: Now the verge",
                "agent: Done.",
            ]
        );
        assert_eq!(p.turns[1].at, rfc3339_ms(1_791_800_010_000));
        never_sent(&p);
        let r = last_reply(&log, "maki").unwrap();
        assert_eq!((r.text.as_str(), r.turn), ("Done.", 2));
        let found = logs::list(home.path()).remove(0);
        assert_eq!(logs::cwd_of(&found).as_deref(), Some(Path::new("/meadow")));
        let session = herdr::AgentSession {
            agent: "maki".into(),
            kind: "id".into(),
            value: "3vQB7B6MrGQZaxCuFg4oh".into(),
        };
        assert_eq!(
            find_log(home.path(), "maki", Some(&session), Path::new("/meadow")),
            Ok((log, "maki"))
        );
    }

    fn kiro_log() -> String {
        let rec = |id: &str, at: &str, payload: Value| json!({"id": id, "timestamp": at, "payload": payload});
        lines(&[
            rec(
                "u1",
                "2026-10-10T08:00:00.000Z",
                json!({"type": "user", "content": "Plant a file named seed.txt, key=sk-abcdefghijklmnop1234", "documents": []}),
            ),
            rec(
                "t1-turn-start",
                "2026-10-10T08:00:00.100Z",
                json!({"type": "turn_start", "turnId": "t1"}),
            ),
            rec(
                "r1",
                "2026-10-10T08:00:01.000Z",
                json!({"type": "assistant", "operationType": "Reasoning", "content": "MUSING", "turnId": "t1"}),
            ),
            rec(
                "c1-pending",
                "2026-10-10T08:00:02.000Z",
                json!({"type": "pending_interaction", "interactionType": "tool_approval", "question": "Write File"}),
            ),
            rec(
                "c1-call",
                "2026-10-10T08:00:03.000Z",
                json!({"type": "tool_call", "toolName": "fs_write", "kind": "edit", "filePath": "/garden/seed.txt", "args": {"path": "/garden/seed.txt", "text": "FILE BODY"}}),
            ),
            rec(
                "c1-result",
                "2026-10-10T08:00:04.000Z",
                json!({"type": "tool_result", "content": "TOOL OUTPUT", "success": true}),
            ),
            rec(
                "c2-call",
                "2026-10-10T08:00:05.000Z",
                json!({"type": "tool_call", "toolName": "execute_bash", "kind": "execute", "args": {"command": "ls -la /garden", "description": "List"}}),
            ),
            rec(
                "c3-call",
                "2026-10-10T08:00:06.000Z",
                json!({"type": "tool_call", "toolName": "read_file", "kind": "read", "args": {"path": "/garden/notes.md"}}),
            ),
            rec(
                "s1",
                "2026-10-10T08:00:07.000Z",
                json!({"type": "assistant", "operationType": "Say", "content": "Planted seed.txt.", "turnId": "t1"}),
            ),
            rec(
                "t1-turn-end",
                "2026-10-10T08:00:08.000Z",
                json!({"type": "turn_end", "stopReason": "end_turn"}),
            ),
            rec(
                "start",
                "2026-10-10T08:00:09.000Z",
                json!({"type": "session_start", "content": "SYSTEM PROMPT"}),
            ),
        ])
    }

    #[test]
    fn a_kiro_session_becomes_turns() {
        let home = tempfile::tempdir().unwrap();
        let dir = ".kiro/sessions/0123456789abcdef/sess_k-1";
        let log = put(home.path(), &format!("{dir}/messages.jsonl"), &kiro_log());
        put(
            home.path(),
            &format!("{dir}/session.json"),
            &json!({"id": "sess_k-1", "title": "Plant a seed", "workspacePaths": ["/garden"],
                    "rootPaths": ["/garden"], "status": "idle"})
            .to_string(),
        );
        let p = read("kiro", &log);
        assert_eq!(p.id.as_deref(), Some("sess_k-1"));
        assert_eq!(p.cwd.as_deref(), Some("/garden"));
        assert_eq!(p.title.as_deref(), Some("Plant a seed"));
        assert_eq!(p.started_at.as_deref(), Some("2026-10-10T08:00:00.000Z"));
        assert_eq!(p.last_at.as_deref(), Some("2026-10-10T08:00:07.000Z"));
        assert_eq!(
            turns(&p),
            [
                "you: Plant a file named seed.txt, key=[redacted]",
                "tool: fs_write /garden/seed.txt",
                "tool: execute_bash ls -la /garden",
                "tool: read_file /garden/notes.md",
                "agent: Planted seed.txt.",
            ]
        );
        never_sent(&p);
        let r = last_reply(&log, "kiro").unwrap();
        assert_eq!((r.text.as_str(), r.turn), ("Planted seed.txt.", 1));
        assert_eq!(r.at, "2026-10-10T08:00:07.000Z");
    }

    fn pi_log() -> String {
        let msg = |id: &str, at: &str, m: Value| json!({"type": "message", "id": id, "timestamp": at, "message": m});
        lines(&[
            json!({"type": "session", "version": 3, "id": "p-1", "timestamp": "2026-10-11T09:00:00.000Z", "cwd": "/orchard"}),
            json!({"type": "model_change", "id": "m0", "provider": "x", "modelId": "y"}),
            msg(
                "a",
                "2026-10-11T09:00:01.000Z",
                json!({"role": "system", "content": "SYSTEM PROMPT"}),
            ),
            msg(
                "b",
                "2026-10-11T09:00:02.000Z",
                json!({"role": "user", "content": [{"type": "text", "text": "Count the apple trees"}]}),
            ),
            msg(
                "c",
                "2026-10-11T09:00:03.000Z",
                json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "MUSING"},
                {"type": "toolCall", "id": "k1", "name": "bash", "arguments": {"command": "wc -l trees.txt"}},
                {"type": "toolCall", "id": "k2", "name": "write", "arguments": {"path": "/orchard/count.md", "content": "FILE BODY"}},
                {"type": "toolCall", "id": "k3", "name": "edit", "arguments": {"path": "/orchard/notes.md", "edits": [{"oldText": "FILE BODY", "newText": "FILE BODY"}]}}
            ], "stopReason": "toolUse"}),
            ),
            msg(
                "d",
                "2026-10-11T09:00:04.000Z",
                json!({"role": "toolResult", "toolCallId": "k1", "toolName": "bash", "content": [{"type": "text", "text": "TOOL OUTPUT"}]}),
            ),
            msg(
                "e",
                "2026-10-11T09:00:05.000Z",
                json!({"role": "assistant", "content": [{"type": "text", "text": "There are 12 trees."}], "stopReason": "stop"}),
            ),
            msg(
                "f",
                "2026-10-11T09:00:06.000Z",
                json!({"role": "bashExecution", "command": "git status", "output": "TOOL OUTPUT"}),
            ),
        ])
    }

    #[test]
    fn a_pi_session_becomes_turns() {
        let home = tempfile::tempdir().unwrap();
        let log = put(
            home.path(),
            ".pi/agent/sessions/--orchard--/2026-10-11T09-00-00-000Z_p-1.jsonl",
            &pi_log(),
        );
        let p = read("pi", &log);
        assert_eq!(p.id.as_deref(), Some("p-1"));
        assert_eq!(p.cwd.as_deref(), Some("/orchard"));
        assert_eq!(p.started_at.as_deref(), Some("2026-10-11T09:00:00.000Z"));
        assert_eq!(
            turns(&p),
            [
                "you: Count the apple trees",
                "tool: bash wc -l trees.txt",
                "tool: write /orchard/count.md",
                "tool: edit /orchard/notes.md",
                "agent: There are 12 trees.",
                "you: ! git status",
            ]
        );
        never_sent(&p);
        // The `!` command is a prompt with no reply yet: the one before.
        let r = last_reply(&log, "pi").unwrap();
        assert_eq!((r.text.as_str(), r.turn), ("There are 12 trees.", 1));
    }

    fn qwen_log() -> String {
        let rec = |ty: &str, prov: &str, prompt: &str, at: &str, message: Value| {
            json!({"uuid": at, "sessionId": "q-1", "timestamp": at, "type": ty, "provenance": prov,
                   "promptId": prompt, "cwd": "/meadow", "message": message})
        };
        let main = "q-1########1";
        lines(&[
            rec(
                "system",
                "system",
                main,
                "2026-10-12T10:00:00.000Z",
                json!({"role": "user", "parts": [{"text": "MEMORY"}]}),
            ),
            rec(
                "user",
                "real_user",
                main,
                "2026-10-12T10:00:01.000Z",
                json!({"role": "user", "parts": [{"text": "Mow the meadow"}]}),
            ),
            rec(
                "assistant",
                "assistant_output",
                main,
                "2026-10-12T10:00:02.000Z",
                json!({"role": "model", "parts": [
                    {"text": "MUSING", "thought": true},
                    {"functionCall": {"id": "f1", "name": "run_shell_command", "args": {"command": "mow --all", "description": "Mow"}}},
                    {"functionCall": {"id": "f2", "name": "write_file", "args": {"file_path": "/meadow/log.txt", "content": "FILE BODY"}}}
                ]}),
            ),
            rec(
                "tool_result",
                "tool_result",
                main,
                "2026-10-12T10:00:03.000Z",
                json!({"role": "user", "parts": [{"functionResponse": {"id": "f1", "response": {"output": "TOOL OUTPUT"}}}]}),
            ),
            rec(
                "user",
                "tool_result",
                main,
                "2026-10-12T10:00:03.500Z",
                json!({"role": "user", "parts": [{"text": "ADDED CONTEXT"}]}),
            ),
            rec(
                "assistant",
                "assistant_output",
                main,
                "2026-10-12T10:00:04.000Z",
                json!({"role": "model", "parts": [{"text": "MUSING", "thought": true}, {"text": "Mowed."}]}),
            ),
            rec(
                "assistant",
                "assistant_output",
                "q-1#managed-auto-memory-extractor-1#",
                "2026-10-12T10:00:05.000Z",
                json!({"role": "model", "parts": [{"text": "MEMORY"}]}),
            ),
            json!({"type": "system", "subtype": "ui_telemetry", "timestamp": "2026-10-12T10:00:06.000Z", "systemPayload": {"uiEvent": {"phase": "ended"}}}),
        ])
    }

    #[test]
    fn a_qwen_chat_becomes_turns() {
        let home = tempfile::tempdir().unwrap();
        let log = put(
            home.path(),
            ".qwen/projects/-meadow/chats/q-1.jsonl",
            &qwen_log(),
        );
        let p = read("qwen", &log);
        assert_eq!(p.id.as_deref(), Some("q-1"));
        assert_eq!(p.cwd.as_deref(), Some("/meadow"));
        assert_eq!(p.last_at.as_deref(), Some("2026-10-12T10:00:04.000Z"));
        assert_eq!(
            turns(&p),
            [
                "you: Mow the meadow",
                "tool: run_shell_command mow --all",
                "tool: write_file /meadow/log.txt",
                "agent: Mowed.",
            ]
        );
        never_sent(&p);
        let r = last_reply(&log, "qwen").unwrap();
        assert_eq!((r.text.as_str(), r.turn), ("Mowed.", 1));
    }

    fn cline_session(home: &Path, id: &str, cwd: &str) -> PathBuf {
        let dir = format!(".cline/data/sessions/{id}");
        put(
            home,
            &format!("{dir}/{id}.json"),
            &json!({"session_id": id, "source": "cli", "pid": 1, "status": "running", "cwd": cwd,
                    "prompt": "SYSTEM PROMPT", "metadata": {"title": "Water the roses", "systemPrompt": "SYSTEM PROMPT"}})
            .to_string(),
        );
        let doc = json!({"version": 1, "agent": "lead", "sessionId": id, "system_prompt": "SYSTEM PROMPT", "messages": [
            {"role": "user", "ts": 1_791_000_000_000u64, "content": [{"type": "text", "text": "<user_input mode=\"act\">Water the roses, token=hunter2</user_input>"}]},
            {"role": "assistant", "ts": 1_791_000_001_000u64, "content": [
                {"type": "thinking", "thinking": "MUSING"},
                {"type": "text", "text": "Watering now."},
                {"type": "tool_use", "id": "u1", "name": "run_commands", "input": {"commands": ["water --roses", "water --tulips"]}},
                {"type": "tool_use", "id": "u2", "name": "editor", "input": {"path": "/garden/roses.md", "new_text": "FILE BODY"}}
            ]},
            {"role": "user", "ts": 1_791_000_002_000u64, "content": [
                {"type": "tool_result", "tool_use_id": "u1", "content": "TOOL OUTPUT"},
                {"type": "text", "text": "<environment_details>ADDED CONTEXT</environment_details>"}
            ]},
            {"role": "assistant", "ts": 1_791_000_003_500u64, "content": [{"type": "text", "text": "The roses are watered."}]}
        ]});
        put(home, &format!("{dir}/{id}.messages.json"), &doc.to_string())
    }

    #[test]
    fn a_cline_session_becomes_turns() {
        let home = tempfile::tempdir().unwrap();
        let log = cline_session(home.path(), "1791000000000_abcde", "/garden");
        let p = read("cline", &log);
        assert_eq!(p.id.as_deref(), Some("1791000000000_abcde"));
        assert_eq!(p.cwd.as_deref(), Some("/garden"));
        assert_eq!(p.title.as_deref(), Some("Water the roses"));
        assert_eq!(p.started_at.as_deref(), Some("2026-10-03T04:00:00.000Z"));
        assert_eq!(p.last_at.as_deref(), Some("2026-10-03T04:00:03.500Z"));
        assert_eq!(
            turns(&p),
            [
                "you: Water the roses, token=[redacted]",
                "agent: Watering now.",
                "tool: run_commands water --roses",
                "tool: editor /garden/roses.md",
                "agent: The roses are watered.",
            ]
        );
        never_sent(&p);
        let r = last_reply(&log, "cline").unwrap();
        assert_eq!(r.text, "Watering now.\n\nThe roses are watered.");
        assert_eq!((r.turn, r.at.as_str()), (1, "2026-10-03T04:00:03.500Z"));
    }

    #[test]
    fn cline_prompts_are_unwrapped() {
        assert_eq!(
            cline_you("<user_input mode=\"plan\">\n Plan it \n</user_input>").as_deref(),
            Some("Plan it")
        );
        assert_eq!(cline_you("<task>Do it</task>").as_deref(), Some("Do it"));
        assert_eq!(
            cline_you("<environment_details>x</environment_details>"),
            None
        );
        assert_eq!(cline_you("<user_inputs>x</user_inputs>"), None);
        assert_eq!(cline_you("plain text"), None);
    }

    /// A made-up OpenCode session: a prompt, reasoning, a tool with its
    /// output, a part OpenCode added, and the reply; a subagent's session
    /// beside it.
    pub(crate) fn opencode_db(home: &Path, db_rel: &str, cwd: &str) -> PathBuf {
        let db = home.join(db_rel);
        let c = logs::tests::make_db(&db);
        let ms = 1_791_100_000_000i64;
        c.execute(
            "INSERT INTO session VALUES ('ses_o1', 'p', NULL, ?1, 'Sweep the porch', ?2, ?3, NULL)",
            rusqlite::params![cwd, ms, ms + 9000],
        )
        .unwrap();
        c.execute(
            "INSERT INTO session VALUES ('ses_sub', 'p', 'ses_o1', ?1, 'SUBAGENT', ?2, ?3, NULL)",
            rusqlite::params![cwd, ms, ms + 9000],
        )
        .unwrap();
        let msg = |id: &str, session: &str, at: i64, data: Value| {
            c.execute(
                "INSERT INTO message VALUES (?1, ?2, ?3, ?3, ?4)",
                rusqlite::params![id, session, at, data.to_string()],
            )
            .unwrap();
        };
        let part = |id: &str, m: &str, session: &str, at: i64, data: Value| {
            c.execute(
                "INSERT INTO part VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                rusqlite::params![id, m, session, at, data.to_string()],
            )
            .unwrap();
        };
        msg("msg_1", "ses_o1", ms, json!({"role": "user"}));
        part(
            "prt_1",
            "msg_1",
            "ses_o1",
            ms,
            json!({"type": "text", "text": "Sweep the porch, Bearer abcdefghijklmnop1234"}),
        );
        part(
            "prt_2",
            "msg_1",
            "ses_o1",
            ms + 1,
            json!({"type": "text", "text": "ADDED CONTEXT", "synthetic": true}),
        );
        msg(
            "msg_2",
            "ses_o1",
            ms + 1000,
            json!({"role": "assistant", "finish": "tool-calls"}),
        );
        part(
            "prt_3",
            "msg_2",
            "ses_o1",
            ms + 1000,
            json!({"type": "step-start"}),
        );
        part(
            "prt_4",
            "msg_2",
            "ses_o1",
            ms + 1100,
            json!({"type": "reasoning", "text": "MUSING"}),
        );
        part(
            "prt_5",
            "msg_2",
            "ses_o1",
            ms + 1200,
            json!({"type": "tool", "tool": "bash", "callID": "c1",
            "state": {"status": "completed", "input": {"command": "sweep --porch", "description": "Sweep"}, "output": "TOOL OUTPUT"}}),
        );
        part(
            "prt_6",
            "msg_2",
            "ses_o1",
            ms + 1300,
            json!({"type": "tool", "tool": "write", "callID": "c2",
            "state": {"status": "completed", "input": {"filePath": "/porch/done.md", "content": "FILE BODY"}, "output": "TOOL OUTPUT"}}),
        );
        msg(
            "msg_3",
            "ses_o1",
            ms + 2000,
            json!({"role": "assistant", "finish": "stop"}),
        );
        part(
            "prt_7",
            "msg_3",
            "ses_o1",
            ms + 2000,
            json!({"type": "text", "text": "The porch is swept."}),
        );
        part(
            "prt_8",
            "msg_3",
            "ses_o1",
            ms + 2100,
            json!({"type": "step-finish", "reason": "stop"}),
        );
        msg("msg_9", "ses_sub", ms, json!({"role": "user"}));
        part(
            "prt_9",
            "msg_9",
            "ses_sub",
            ms,
            json!({"type": "text", "text": "SUBAGENT"}),
        );
        db
    }

    #[test]
    fn an_opencode_session_becomes_turns() {
        let home = tempfile::tempdir().unwrap();
        let db = opencode_db(home.path(), ".local/share/opencode/opencode.db", "/porch");
        let name = logs::db_path(&db, "ses_o1");
        for agent in ["opencode", "kilo"] {
            let p = read(agent, &name);
            assert_eq!(p.id.as_deref(), Some("ses_o1"));
            assert_eq!(p.cwd.as_deref(), Some("/porch"));
            assert_eq!(p.title.as_deref(), Some("Sweep the porch"));
            assert_eq!(p.started_at.as_deref(), Some("2026-10-04T07:46:40.000Z"));
            assert_eq!(p.last_at.as_deref(), Some("2026-10-04T07:46:42.000Z"));
            assert_eq!(
                turns(&p),
                [
                    "you: Sweep the porch, Bearer [redacted]",
                    "tool: bash sweep --porch",
                    "tool: write /porch/done.md",
                    "agent: The porch is swept.",
                ]
            );
            never_sent(&p);
            assert!(!serde_json::to_string(&p.turns)
                .unwrap()
                .contains("SUBAGENT"));
        }
        let r = last_reply(&name, "opencode").unwrap();
        assert_eq!((r.text.as_str(), r.turn), ("The porch is swept.", 1));
        // A subagent's session is no session of its own.
        let sub = logs::db_path(&db, "ses_sub");
        assert!(matches!(
            last_reply(&sub, "opencode"),
            Err(NoReply::NoLog(_))
        ));
        assert!(!logs::list(home.path()).iter().any(|l| l.path == sub));
    }

    /// The reader sends every harness's sessions, a database's again when
    /// it changed.
    #[test]
    fn the_reader_reads_every_harness() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let h = home.path();
        let cwd = h.join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let w = cwd.to_str().unwrap();
        let kiro_dir = ".kiro/sessions/0123456789abcdef/sess_k-1";
        put(h, &format!("{kiro_dir}/messages.jsonl"), &kiro_log());
        put(
            h,
            &format!("{kiro_dir}/session.json"),
            &json!({"id": "sess_k-1", "workspacePaths": [w]}).to_string(),
        );
        put(
            h,
            ".pi/agent/sessions/--work--/2026-10-11T09-00-00-000Z_p-1.jsonl",
            &pi_log().replace("/orchard", w),
        );
        put(
            h,
            ".qwen/projects/-work/chats/q-1.jsonl",
            &qwen_log().replace("/meadow", w),
        );
        cline_session(h, "1791000000000_abcde", w);
        let agy = "0a1b2c3d-1111-4222-8333-444455556666";
        logs::tests::make_agy_summaries(
            &h.join(".gemini/antigravity-cli"),
            &[(
                agy,
                "Water",
                &json!([format!("file://{w}")]).to_string(),
                "",
                0,
            )],
        );
        put(
            h,
            &format!(".gemini/antigravity-cli/brain/{agy}/.system_generated/logs/transcript.jsonl"),
            &agy_log(),
        );
        put(
            h,
            ".local/state/maki/sessions/3vQB7B6MrGQZaxCuFg4oh.jsonl",
            &maki_log().replace("/meadow", w),
        );
        let db = opencode_db(h, ".local/share/kilo/kilo.db", w);
        // Written just now, as a live session would be.
        let now = unix(SystemTime::now()) as i64;
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute("UPDATE session SET time_updated = ?1", [now])
            .unwrap();

        let mut r = Reader::new(h.to_path_buf(), state.path());
        let mut got = Vec::new();
        r.cycle(&mut |n| {
            if n["note"] == "session" {
                got.push(n["session"].clone());
            }
            true
        });
        let mut agents: Vec<(String, String)> = got
            .iter()
            .map(|s| {
                (
                    s["agent"].as_str().unwrap().into(),
                    s["id"].as_str().unwrap().into(),
                )
            })
            .collect();
        agents.sort();
        assert_eq!(
            agents,
            [
                ("agy".into(), agy.into()),
                ("cline".into(), "1791000000000_abcde".into()),
                ("kilo".into(), "ses_o1".into()),
                ("kiro".into(), "sess_k-1".into()),
                ("maki".into(), "3vQB7B6MrGQZaxCuFg4oh".into()),
                ("pi".into(), "p-1".into()),
                ("qwen".into(), "q-1".into()),
            ]
        );
        for s in &got {
            assert_eq!(s["cwd"], json!(cwd), "{s}");
            let text = s.to_string();
            for marker in NEVER {
                assert!(!text.contains(marker), "{marker} in {text}");
            }
        }

        // Nothing changed: nothing again.
        let mut again = 0;
        r.cycle(&mut |n| {
            again += usize::from(n["note"] == "session");
            true
        });
        assert_eq!(again, 0);

        // A new part in the database: that session again, whole.
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "INSERT INTO message VALUES ('msg_4', 'ses_o1', 1791100005000, 1791100005000, '{\"role\":\"user\"}');
             INSERT INTO part VALUES ('prt_10', 'msg_4', 'ses_o1', 1791100005000, 1791100005000,
               '{\"type\":\"text\",\"text\":\"And the steps\"}');",
        )
        .unwrap();
        drop(c);
        r.sent_at.clear();
        let mut grown = Vec::new();
        r.cycle(&mut |n| {
            if n["note"] == "session" {
                grown.push(n["session"].clone());
            }
            true
        });
        assert_eq!(grown.len(), 1, "{grown:?}");
        assert_eq!(grown[0]["agent"], "kilo");
        assert_eq!(grown[0]["turns_total"], 5);
        assert_eq!(grown[0]["turns"][4]["text"], "And the steps");
    }

    /// herdr's session by id or path finds each harness's log; without
    /// one, the only recent log of that harness in the pane's folder does,
    /// and two such logs, or none, are no log.
    #[test]
    fn logs_are_found_for_every_harness() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let s = |kind: &str, value: &str| herdr::AgentSession {
            agent: String::new(),
            kind: kind.into(),
            value: value.into(),
        };
        let kiro_dir = ".kiro/sessions/0123456789abcdef/sess_k-1";
        let kiro = put(h, &format!("{kiro_dir}/messages.jsonl"), &kiro_log());
        put(
            h,
            &format!("{kiro_dir}/session.json"),
            &json!({"id": "sess_k-1", "workspacePaths": ["/garden"]}).to_string(),
        );
        let pi = put(
            h,
            ".pi/agent/sessions/--orchard--/2026-10-11T09-00-00-000Z_p-1.jsonl",
            &pi_log(),
        );
        let qwen = put(h, ".qwen/projects/-meadow/chats/q-1.jsonl", &qwen_log());
        let cline = cline_session(h, "1791000000000_abcde", "/roses");
        let db = opencode_db(h, ".local/share/opencode/opencode.db", "/porch");
        let porch = logs::db_path(&db, "ses_o1");
        let nowhere = Path::new("/nowhere");
        let found = |agent: &str, session: Option<herdr::AgentSession>, cwd: &str| {
            find_log(h, agent, session.as_ref(), Path::new(cwd))
        };
        assert_eq!(
            found("kiro", Some(s("id", "sess_k-1")), "/x"),
            Ok((kiro.clone(), "kiro"))
        );
        assert_eq!(
            found("kiro", Some(s("id", "k-1")), "/x"),
            Ok((kiro.clone(), "kiro"))
        );
        assert_eq!(
            found("pi", Some(s("path", pi.to_str().unwrap())), "/x"),
            Ok((pi.clone(), "pi"))
        );
        assert_eq!(
            found("pi", Some(s("id", "p-1")), "/x"),
            Ok((pi.clone(), "pi"))
        );
        assert_eq!(
            found("qwen", Some(s("id", "q-1")), "/x"),
            Ok((qwen.clone(), "qwen"))
        );
        assert_eq!(
            found("cline", Some(s("id", "1791000000000_abcde")), "/x"),
            Ok((cline.clone(), "cline"))
        );
        assert_eq!(
            found("opencode", Some(s("id", "ses_o1")), "/x"),
            Ok((porch.clone(), "opencode"))
        );
        // Another harness's session, or a path that is not the harness's log.
        for (agent, session) in [
            ("kilo", s("id", "ses_o1")),
            ("qwen", s("id", "p-1")),
            ("opencode", s("path", porch.to_str().unwrap())),
            ("pi", s("path", qwen.to_str().unwrap())),
            ("opencode", s("id", "ses_sub")),
            ("opencode", s("id", "ses_o1' OR 1=1 --")),
        ] {
            assert!(
                matches!(
                    found(agent, Some(session.clone()), "/x"),
                    Err(NoReply::NoLog(_))
                ),
                "{agent} {session:?}"
            );
        }
        // No session reported: the one recent log in the pane's folder.
        assert_eq!(found("kiro", None, "/garden"), Ok((kiro.clone(), "kiro")));
        assert_eq!(found("pi", None, "/orchard"), Ok((pi, "pi")));
        assert_eq!(found("qwen", None, "/meadow"), Ok((qwen, "qwen")));
        assert_eq!(found("cline", None, "/roses"), Ok((cline, "cline")));
        // The database session was last written long ago.
        assert!(matches!(
            found("opencode", None, "/porch"),
            Err(NoReply::NoLog(_))
        ));
        assert!(matches!(
            found("kiro", None, nowhere.to_str().unwrap()),
            Err(NoReply::NoLog(_))
        ));
        // Two recent Kiro sessions in one folder: neither is guessed.
        let second = ".kiro/sessions/0123456789abcdef/sess_k-2";
        put(h, &format!("{second}/messages.jsonl"), &kiro_log());
        put(
            h,
            &format!("{second}/session.json"),
            &json!({"id": "sess_k-2", "workspacePaths": ["/garden"]}).to_string(),
        );
        assert!(matches!(
            found("kiro", None, "/garden"),
            Err(NoReply::NoLog(_))
        ));
        // Claude Code and Codex are never guessed.
        assert!(matches!(
            found("claude", None, "/garden"),
            Err(NoReply::NoLog(_))
        ));
    }
}
