//! Provider definitions: how to invoke a CLI and how to parse its output.

mod claude;
mod codex;
mod gemini;
mod text;

use serde::Deserialize;

use crate::event::Event;

/// Output protocol spoken by a provider CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Claude Code: `claude -p --output-format stream-json`.
    Claude,
    /// OpenAI Codex CLI: `codex exec --json`.
    Codex,
    /// Gemini CLI: `gemini -p ... --output-format stream-json`.
    Gemini,
    /// Any CLI that prints plain text. Prompt goes to `{prompt}` in args, or stdin.
    Text,
}

/// A configured provider. Built-ins exist for each `Kind`; more come from config.
#[derive(Debug, Clone, Deserialize)]
pub struct Spec {
    #[serde(skip)]
    pub name: String,
    pub kind: Kind,
    pub program: String,
    /// Extra arguments always passed (for `text`, may contain `{prompt}`).
    #[serde(default)]
    pub args: Vec<String>,
}

impl Spec {
    pub fn builtin(kind: Kind) -> Self {
        let (name, program) = match kind {
            Kind::Claude => ("claude", "claude"),
            Kind::Codex => ("codex", "codex"),
            Kind::Gemini => ("gemini", "gemini"),
            Kind::Text => ("text", "cat"),
        };
        Spec {
            name: name.into(),
            kind,
            program: program.into(),
            args: Vec::new(),
        }
    }

    pub fn builtins() -> [Spec; 3] {
        [Kind::Claude, Kind::Codex, Kind::Gemini].map(Spec::builtin)
    }

    /// Build the argv (excluding the program) and the optional stdin payload.
    pub fn invocation(&self, req: &Request) -> Invocation {
        let mut inv = match self.kind {
            Kind::Claude => claude::invocation(req),
            Kind::Codex => codex::invocation(req),
            Kind::Gemini => gemini::invocation(req),
            Kind::Text => text::invocation(&self.args, req),
        };
        if self.kind != Kind::Text {
            inv.args.splice(0..0, self.args.iter().cloned());
        }
        inv.args.extend(req.extra_args.iter().cloned());
        inv
    }

    pub fn parser(&self) -> Box<dyn Parser> {
        match self.kind {
            Kind::Claude => Box::<claude::Parser>::default(),
            Kind::Codex => Box::<codex::Parser>::default(),
            Kind::Gemini => Box::<gemini::Parser>::default(),
            Kind::Text => Box::new(text::Parser),
        }
    }

    /// Whether this provider can continue a session from an id.
    pub fn supports_resume(&self) -> bool {
        matches!(self.kind, Kind::Claude | Kind::Codex | Kind::Gemini)
    }
}

/// A single turn request.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub prompt: String,
    pub model: Option<String>,
    pub resume: Option<String>,
    /// Passed straight through to the provider CLI (after `--`).
    pub extra_args: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Invocation {
    pub args: Vec<String>,
    pub stdin: Option<String>,
}

/// Turns provider stdout lines into normalized events.
pub trait Parser: Send {
    fn line(&mut self, line: &str, out: &mut Vec<Event>);
    /// Called once at EOF; text-mode providers use it to emit `Done`.
    fn finish(&mut self, _out: &mut Vec<Event>) {}
}

/// Render a JSON tool result `content` field (string or block list) as text.
pub(crate) fn content_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}
