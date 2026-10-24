//! Output sinks for the CLI.
//!
//! Human mode keeps stdout clean (assistant text only) and sends tool
//! activity and run metadata to stderr, so `mharness run ... > out.md` works.

use std::io::{self, IsTerminal, Write};

use serde_json::Value;

use crate::event::Event;
use crate::runner::Sink;

pub struct Human {
    color: bool,
    verbose: bool,
    at_line_start: bool,
}

impl Human {
    pub fn new(verbose: bool) -> Self {
        let color = io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Human {
            color,
            verbose,
            at_line_start: true,
        }
    }

    fn meta(&mut self, line: &str) {
        if !self.at_line_start {
            println!();
            self.at_line_start = true;
        }
        let _ = io::stdout().flush();
        if self.color {
            eprintln!("\x1b[2m{line}\x1b[0m");
        } else {
            eprintln!("{line}");
        }
    }

    /// End the turn on a fresh line.
    pub fn finish(&mut self) {
        if !self.at_line_start {
            println!();
            self.at_line_start = true;
        }
        let _ = io::stdout().flush();
    }
}

impl Sink for Human {
    fn event(&mut self, event: &Event) {
        match event {
            Event::Text { text } => {
                let mut out = io::stdout().lock();
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
                if let Some(c) = text.chars().last() {
                    self.at_line_start = c == '\n';
                }
            }
            Event::ToolUse { name, input } => self.meta(&format!("> {name}({})", summarize(input))),
            Event::ToolResult { output, is_error } => {
                if *is_error {
                    self.meta(&format!("  ! {}", first_line(output)));
                } else if self.verbose {
                    self.meta(&format!("  < {}", first_line(output)));
                }
            }
            Event::Error { message } => self.meta(&format!("error: {message}")),
            Event::Session { id } if self.verbose => self.meta(&format!("session {id}")),
            Event::Done {
                cost_usd,
                duration_ms,
                is_error,
                ..
            } if self.verbose || *is_error => {
                let mut s = String::from(if *is_error { "failed" } else { "done" });
                if let Some(ms) = duration_ms {
                    s.push_str(&format!(" in {:.1}s", *ms as f64 / 1000.0));
                }
                if let Some(c) = cost_usd {
                    s.push_str(&format!(", ${c:.4}"));
                }
                self.meta(&s);
            }
            _ => {}
        }
    }
}

/// NDJSON of normalized events on stdout.
pub struct Json;

impl Sink for Json {
    fn event(&mut self, event: &Event) {
        let mut out = io::stdout().lock();
        let _ = serde_json::to_writer(&mut out, event);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }
}

/// Provider stdout passed through untouched.
pub struct Raw;

impl Sink for Raw {
    fn event(&mut self, _: &Event) {}
    fn raw(&mut self, line: &str) {
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// A short one-line description of a tool input.
pub fn summarize(input: &Value) -> String {
    const KEYS: [&str; 7] = [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
    ];
    let s = KEYS
        .iter()
        .find_map(|k| input.get(k).and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| input.to_string());
    truncate(first_line(&s), 100)
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or_default()
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summarize_prefers_known_keys() {
        assert_eq!(
            summarize(&json!({"command": "ls -la\npwd", "x": 1})),
            "ls -la"
        );
        assert_eq!(summarize(&json!({"a": 1})), r#"{"a":1}"#);
        assert_eq!(
            summarize(&json!({"query": "é".repeat(120)}))
                .chars()
                .count(),
            101
        );
    }
}
