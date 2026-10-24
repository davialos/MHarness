//! Provider-neutral event stream emitted by every backend.

use serde::Serialize;
use serde_json::Value;

/// One normalized event from an agent run. Serialized as NDJSON with `--json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The provider announced (or confirmed) the session/thread id.
    Session { id: String },
    /// A chunk of assistant text. With streaming providers this is a delta.
    Text { text: String },
    /// The agent invoked a tool.
    ToolUse { name: String, input: Value },
    /// A tool returned.
    ToolResult { output: String, is_error: bool },
    /// The turn finished.
    Done {
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    /// A provider-reported error that did not end the process.
    Error { message: String },
}
