//! OpenAI Codex CLI (`codex exec --json`). Uses the user's `codex login`.

use serde_json::Value;

use super::{Invocation, Request};
use crate::event::Event;

pub fn invocation(req: &Request) -> Invocation {
    let mut args = vec![
        "exec".to_string(),
        "--json".into(),
        "--skip-git-repo-check".into(),
    ];
    if let Some(m) = &req.model {
        args.extend(["--model".into(), m.clone()]);
    }
    if let Some(id) = &req.resume {
        args.extend(["resume".into(), id.clone()]);
    }
    // `-` reads the prompt from stdin.
    args.push("-".into());
    Invocation {
        args,
        stdin: Some(req.prompt.clone()),
    }
}

#[derive(Default)]
pub struct Parser {
    last_message: Option<String>,
}

impl super::Parser for Parser {
    fn line(&mut self, line: &str, out: &mut Vec<Event>) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match v["type"].as_str() {
            Some("thread.started") => {
                if let Some(id) = v["thread_id"].as_str() {
                    out.push(Event::Session { id: id.into() });
                }
            }
            Some("item.started") => {
                let item = &v["item"];
                if item["type"] == "command_execution" {
                    out.push(Event::ToolUse {
                        name: "shell".into(),
                        input: serde_json::json!({ "command": item["command"] }),
                    });
                }
            }
            Some("item.completed") => self.item(&v["item"], out),
            Some("turn.completed") => out.push(Event::Done {
                result: self.last_message.take(),
                is_error: false,
                cost_usd: None,
                duration_ms: None,
            }),
            Some("turn.failed") => out.push(Event::Done {
                result: v["error"]["message"].as_str().map(String::from),
                is_error: true,
                cost_usd: None,
                duration_ms: None,
            }),
            Some("error") => out.push(Event::Error {
                message: v["message"].as_str().unwrap_or("unknown error").into(),
            }),
            _ => {}
        }
    }
}

impl Parser {
    fn item(&mut self, item: &Value, out: &mut Vec<Event>) {
        match item["type"].as_str() {
            Some("agent_message") => {
                if let Some(t) = item["text"].as_str() {
                    self.last_message = Some(t.into());
                    out.push(Event::Text { text: t.into() });
                }
            }
            Some("command_execution") => out.push(Event::ToolResult {
                output: item["aggregated_output"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                is_error: item["exit_code"].as_i64().is_some_and(|c| c != 0),
            }),
            Some("file_change") => out.push(Event::ToolUse {
                name: "file_change".into(),
                input: item["changes"].clone(),
            }),
            Some("mcp_tool_call") => out.push(Event::ToolUse {
                name: format!(
                    "{}.{}",
                    item["server"].as_str().unwrap_or("mcp"),
                    item["tool"].as_str().unwrap_or("?")
                ),
                input: item["arguments"].clone(),
            }),
            Some("web_search") => out.push(Event::ToolUse {
                name: "web_search".into(),
                input: serde_json::json!({ "query": item["query"] }),
            }),
            Some("error") => out.push(Event::Error {
                message: item["message"].as_str().unwrap_or_default().into(),
            }),
            _ => {}
        }
    }
}
