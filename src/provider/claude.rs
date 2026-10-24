//! Claude Code (`claude -p --output-format stream-json`).
//!
//! Uses the user's existing Claude Code login (subscription or API key),
//! whatever `claude` itself is authenticated with.

use std::collections::HashSet;

use serde_json::Value;

use super::{Invocation, Request, content_text};
use crate::event::Event;

pub fn invocation(req: &Request) -> Invocation {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]
    .map(String::from)
    .to_vec();
    if let Some(m) = &req.model {
        args.extend(["--model".into(), m.clone()]);
    }
    if let Some(id) = &req.resume {
        args.extend(["--resume".into(), id.clone()]);
    }
    // Prompt goes over stdin: no argv length limits and no cmd.exe quoting on Windows.
    Invocation {
        args,
        stdin: Some(req.prompt.clone()),
    }
}

#[derive(Default)]
pub struct Parser {
    session_announced: bool,
    current_msg: Option<String>,
    /// Message ids whose text already arrived as deltas.
    streamed: HashSet<String>,
}

impl super::Parser for Parser {
    fn line(&mut self, line: &str, out: &mut Vec<Event>) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match v["type"].as_str() {
            Some("system") if v["subtype"] == "init" => {
                if let Some(id) = v["session_id"].as_str() {
                    self.session_announced = true;
                    out.push(Event::Session { id: id.into() });
                }
            }
            Some("stream_event") => self.stream_event(&v["event"], out),
            Some("assistant") => {
                let msg = &v["message"];
                let streamed = msg["id"]
                    .as_str()
                    .is_some_and(|id| self.streamed.contains(id));
                for block in msg["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") if !streamed => {
                            if let Some(t) = block["text"].as_str() {
                                out.push(Event::Text { text: t.into() });
                            }
                        }
                        Some("tool_use") => out.push(Event::ToolUse {
                            name: block["name"].as_str().unwrap_or_default().into(),
                            input: block["input"].clone(),
                        }),
                        _ => {}
                    }
                }
            }
            Some("user") => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        out.push(Event::ToolResult {
                            output: content_text(&block["content"]),
                            is_error: block["is_error"].as_bool().unwrap_or(false),
                        });
                    }
                }
            }
            Some("result") => {
                if !self.session_announced
                    && let Some(id) = v["session_id"].as_str()
                {
                    out.push(Event::Session { id: id.into() });
                }
                out.push(Event::Done {
                    result: v["result"].as_str().map(String::from),
                    is_error: v["is_error"].as_bool().unwrap_or(false),
                    cost_usd: v["total_cost_usd"].as_f64(),
                    duration_ms: v["duration_ms"].as_u64(),
                });
            }
            _ => {}
        }
    }
}

impl Parser {
    fn stream_event(&mut self, e: &Value, out: &mut Vec<Event>) {
        match e["type"].as_str() {
            Some("message_start") => {
                self.current_msg = e["message"]["id"].as_str().map(String::from);
            }
            Some("content_block_delta") if e["delta"]["type"] == "text_delta" => {
                if let Some(t) = e["delta"]["text"].as_str() {
                    if let Some(id) = &self.current_msg {
                        self.streamed.insert(id.clone());
                    }
                    out.push(Event::Text { text: t.into() });
                }
            }
            _ => {}
        }
    }
}
