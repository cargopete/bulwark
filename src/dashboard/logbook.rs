//! A session's logbook: its narration and the steps between, read from the live event stream.

use super::status::{self, Persona, Scope, State, tool_summary, truncate};
use crate::workspace::Workspace;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

/// Only the tail is served; a long hunter writes thousands of entries.
pub const MAX_ENTRIES: usize = 400;
const ERROR_CHARS: usize = 240;

#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Entry {
    Say {
        text: String,
        at: Option<String>,
    },
    Step {
        tool: String,
        summary: String,
        at: Option<String>,
    },
    Error {
        text: String,
        at: Option<String>,
    },
    Restart {
        at: Option<String>,
    },
    End {
        subtype: String,
        turns: Option<u64>,
        duration_ms: Option<u64>,
        cost_usd: Option<f64>,
        at: Option<String>,
    },
}

#[derive(Debug, Serialize)]
pub struct Logbook {
    pub label: String,
    pub persona: Option<Persona>,
    pub state: Option<State>,
    pub total: usize,
    pub truncated: bool,
    pub entries: Vec<Entry>,
}

pub fn extract(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut inits = 0;
    for line in text.lines() {
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = ev
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string);
        let blocks = || {
            ev["message"]
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        };
        match ev.get("type").and_then(Value::as_str) {
            Some("system") if ev.get("subtype").and_then(Value::as_str) == Some("init") => {
                inits += 1;
                if inits > 1 {
                    out.push(Entry::Restart { at });
                }
            }
            Some("assistant") => {
                for b in blocks() {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let t = b.get("text").and_then(Value::as_str).unwrap_or("").trim();
                            if !t.is_empty() {
                                out.push(Entry::Say {
                                    text: t.to_string(),
                                    at: at.clone(),
                                });
                            }
                        }
                        Some("tool_use") => out.push(Entry::Step {
                            tool: b
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("?")
                                .to_string(),
                            summary: tool_summary(b),
                            at: at.clone(),
                        }),
                        _ => {}
                    }
                }
            }
            Some("user") => {
                for b in blocks() {
                    if b.get("type").and_then(Value::as_str) == Some("tool_result")
                        && b.get("is_error").and_then(Value::as_bool) == Some(true)
                    {
                        out.push(Entry::Error {
                            text: truncate(&result_text(b), ERROR_CHARS),
                            at: at.clone(),
                        });
                    }
                }
            }
            Some("result") => out.push(Entry::End {
                subtype: ev
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                turns: ev.get("num_turns").and_then(Value::as_u64),
                duration_ms: ev.get("duration_ms").and_then(Value::as_u64),
                cost_usd: ev.get("total_cost_usd").and_then(Value::as_f64),
                at,
            }),
            _ => {}
        }
    }
    out
}

/// Tool results carry a string or an array of text blocks.
fn result_text(block: &Value) -> String {
    let raw = match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `None` when nothing in the ledger or the workspace knows the label.
pub fn read(ws: &Workspace, scope: &Scope, label: &str, now: DateTime<Utc>) -> Option<Logbook> {
    let session = status::collect(ws, scope, now)
        .sessions
        .into_iter()
        .find(|s| s.label == label);
    let log = status::live_log_for(ws, label);
    if session.is_none() && log.is_none() {
        return None;
    }
    let mut entries = log
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| extract(&t))
        .unwrap_or_default();
    let total = entries.len();
    let truncated = total > MAX_ENTRIES;
    if truncated {
        entries.drain(..total - MAX_ENTRIES);
    }
    Some(Logbook {
        label: label.to_string(),
        persona: scope.persona(label),
        state: session.map(|s| s.state),
        total,
        truncated,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(events: &[Value]) -> String {
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn init() -> Value {
        json!({"type": "system", "subtype": "init", "model": "m"})
    }

    fn say_then_run(text: &str, cmd: &str) -> Value {
        json!({"type": "assistant", "timestamp": "2026-11-04T10:00:00Z", "message": {"content": [
            {"type": "text", "text": text},
            {"type": "tool_use", "name": "Bash", "input": {"command": cmd}}
        ]}})
    }

    #[test]
    fn entries_keep_stream_order() {
        let text = lines(&[
            init(),
            say_then_run(
                "Right. The fee path. How hard can it be?",
                "forge test --mt testFee\n-vvv",
            ),
            json!({"type": "user", "timestamp": "2026-11-04T10:00:05Z", "message": {"content": [
                {"type": "tool_result", "is_error": true, "content": [{"type": "text", "text": "Error:\n  compiler   failed"}]}
            ]}}),
            json!({"type": "assistant", "message": {"content": [{"type": "thinking", "thinking": "x"}]}}),
            init(),
            json!({"type": "result", "subtype": "success", "num_turns": 9, "duration_ms": 61000, "total_cost_usd": 0.5}),
        ]);
        let e = extract(&text);
        assert_eq!(
            e,
            vec![
                Entry::Say {
                    text: "Right. The fee path. How hard can it be?".into(),
                    at: Some("2026-11-04T10:00:00Z".into())
                },
                Entry::Step {
                    tool: "Bash".into(),
                    summary: "Bash: forge test --mt testFee".into(),
                    at: Some("2026-11-04T10:00:00Z".into())
                },
                Entry::Error {
                    text: "Error: compiler failed".into(),
                    at: Some("2026-11-04T10:00:05Z".into())
                },
                Entry::Restart { at: None },
                Entry::End {
                    subtype: "success".into(),
                    turns: Some(9),
                    duration_ms: Some(61000),
                    cost_usd: Some(0.5),
                    at: None
                },
            ]
        );
    }

    #[test]
    fn long_errors_are_cut_and_successful_results_skipped() {
        let long = "e".repeat(1000);
        let text = lines(&[json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "is_error": true, "content": long},
            {"type": "tool_result", "content": "fine"}
        ]}})]);
        let e = extract(&text);
        assert_eq!(e.len(), 1);
        let Entry::Error { text, .. } = &e[0] else {
            panic!("{e:?}")
        };
        assert_eq!(text.chars().count(), ERROR_CHARS + 3);
    }

    #[test]
    fn served_logbook_is_the_tail_with_a_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path(), "ws");
        let mut events = vec![init()];
        events.extend((0..300).map(|i| say_then_run(&format!("note {i}"), "ls")));
        let log = ws
            .findings_logs_dir()
            .join("hunt-fees-agent.log.live.jsonl");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, lines(&events)).unwrap();

        let scope = Scope {
            personas: [(
                "hunter".to_string(),
                Persona {
                    key: "juliette".into(),
                    name: "Juliette Nichols".into(),
                },
            )]
            .into(),
            ..Scope::default()
        };
        let book = read(&ws, &scope, "agent:hunt-fees", Utc::now()).unwrap();
        assert_eq!(book.total, 600);
        assert!(book.truncated);
        assert_eq!(book.entries.len(), MAX_ENTRIES);
        assert_eq!(book.persona.unwrap().name, "Juliette Nichols");
        assert_eq!(book.state, Some(State::Running));
        let Entry::Step { .. } = book.entries.last().unwrap() else {
            panic!("newest entry should be last")
        };
        assert!(read(&ws, &scope, "agent:nobody", Utc::now()).is_none());
    }
}
