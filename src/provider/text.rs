//! Generic plain-text provider for any CLI (aider, ollama, llm, a shell script, ...).
//!
//! If any configured arg contains `{prompt}` it is substituted there;
//! otherwise the prompt is written to stdin.

use super::{Invocation, Request};
use crate::event::Event;

pub fn invocation(template: &[String], req: &Request) -> Invocation {
    let inline = template.iter().any(|a| a.contains("{prompt}"));
    let args = template
        .iter()
        .map(|a| a.replace("{prompt}", &req.prompt))
        .collect();
    Invocation {
        args,
        stdin: (!inline).then(|| req.prompt.clone()),
    }
}

pub struct Parser;

impl super::Parser for Parser {
    fn line(&mut self, line: &str, out: &mut Vec<Event>) {
        out.push(Event::Text {
            text: format!("{line}\n"),
        });
    }

    fn finish(&mut self, out: &mut Vec<Event>) {
        out.push(Event::Done {
            result: None,
            is_error: false,
            cost_usd: None,
            duration_ms: None,
        });
    }
}
