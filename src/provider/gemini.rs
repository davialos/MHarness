//! Gemini CLI (`gemini -p ... --output-format stream-json`). Uses the user's Google login.

use serde_json::Value;

use super::{Invocation, Request};
use crate::event::Event;

pub fn invocation(req: &Request) -> Invocation {
    let mut args = vec!["--output-format".to_string(), "stream-json".into()];
    if let Some(m) = &req.model {
        args.extend(["--model".into(), m.clone()]);
    }
    if let Some(id) = &req.resume {
        args.extend(["--resume".into(), id.clone()]);
    }
    args.extend(["--prompt".into(), req.prompt.clone()]);
    Invocation { args, stdin: None }
}

#[derive(Default)]
pub struct Parser {
    text: String,
}

impl super::Parser for Parser {
    fn line(&mut self, line: &str, out: &mut Vec<Event>) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match v["type"].as_str() {
            Some("init") => {
                if let Some(id) = v["session_id"].as_str() {
                    out.push(Event::Session { id: id.into() });
                }
            }
            Some("message") if v["role"] == "assistant" => {
                if let Some(t) = v["content"].as_str() {
                    self.text.push_str(t);
                    out.push(Event::Text { text: t.into() });
                }
            }
            Some("tool_use") => out.push(Event::ToolUse {
                name: v["tool_name"].as_str().unwrap_or_default().into(),
                input: v["parameters"].clone(),
            }),
            Some("tool_result") => out.push(Event::ToolResult {
                output: match &v["output"] {
                    Value::String(s) => s.clone(),
                    Value::Null => v["error"]["message"].as_str().unwrap_or_default().into(),
                    other => other.to_string(),
                },
                is_error: v["status"].as_str().is_some_and(|s| s != "success"),
            }),
            Some("error") => out.push(Event::Error {
                message: v["message"].as_str().unwrap_or("unknown error").into(),
            }),
            Some("result") => out.push(Event::Done {
                result: Some(std::mem::take(&mut self.text)).filter(|t| !t.is_empty()),
                is_error: v["status"].as_str().is_some_and(|s| s != "success"),
                cost_usd: None,
                duration_ms: v["stats"]["duration_ms"].as_u64(),
            }),
            _ => {}
        }
    }
}
