use mharness::{Event, Kind, Request, Spec, runner};

fn parse(kind: Kind, fixture: &str) -> Vec<Event> {
    let path = format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).unwrap();
    let mut parser = Spec::builtin(kind).parser();
    let mut out = Vec::new();
    for line in text.lines() {
        parser.line(line, &mut out);
    }
    parser.finish(&mut out);
    out
}

fn texts(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn claude_stream_json() {
    let ev = parse(Kind::Claude, "claude.jsonl");
    assert_eq!(
        ev[0],
        Event::Session {
            id: "sess-123".into()
        }
    );
    assert!(
        matches!(&ev[1], Event::ToolUse { name, input } if name == "Bash" && input["command"] == "echo hello")
    );
    assert_eq!(
        ev[2],
        Event::ToolResult {
            output: "hello".into(),
            is_error: false
        }
    );
    // Streamed deltas are used; the full assistant message must not duplicate them.
    assert_eq!(texts(&ev), "done");
    assert!(
        matches!(ev.last(), Some(Event::Done { result: Some(r), is_error: false, cost_usd: Some(_), .. }) if r == "done")
    );
}

#[test]
fn claude_without_partial_messages_uses_full_text() {
    let mut p = Spec::builtin(Kind::Claude).parser();
    let mut out = Vec::new();
    p.line(
        r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"hi"}]}}"#,
        &mut out,
    );
    assert_eq!(out, vec![Event::Text { text: "hi".into() }]);
}

#[test]
fn codex_exec_json() {
    let ev = parse(Kind::Codex, "codex.jsonl");
    assert_eq!(
        ev[0],
        Event::Session {
            id: "0199-codex-thread".into()
        }
    );
    assert!(matches!(&ev[1], Event::ToolUse { name, .. } if name == "shell"));
    assert_eq!(
        ev[2],
        Event::ToolResult {
            output: "README.md\nsrc\n".into(),
            is_error: false
        }
    );
    assert_eq!(texts(&ev), "There are two entries.");
    assert!(
        matches!(ev.last(), Some(Event::Done { result: Some(r), is_error: false, .. }) if r == "There are two entries.")
    );
}

#[test]
fn gemini_stream_json() {
    let ev = parse(Kind::Gemini, "gemini.jsonl");
    assert_eq!(
        ev[0],
        Event::Session {
            id: "gem-42".into()
        }
    );
    assert!(matches!(&ev[1], Event::ToolUse { name, .. } if name == "list_directory"));
    assert_eq!(texts(&ev), "Only README.md.");
    assert!(matches!(
        ev.last(),
        Some(Event::Done {
            duration_ms: Some(1234),
            is_error: false,
            ..
        })
    ));
}

#[test]
fn invocations_put_prompt_on_stdin_and_append_passthrough() {
    let req = Request {
        prompt: "hi".into(),
        model: Some("opus".into()),
        resume: Some("s1".into()),
        extra_args: vec!["--max-turns".into(), "2".into()],
    };
    let mut spec = Spec::builtin(Kind::Claude);
    spec.args = vec!["--permission-mode".into(), "plan".into()];
    let inv = spec.invocation(&req);
    assert_eq!(inv.stdin.as_deref(), Some("hi"));
    assert_eq!(&inv.args[..2], ["--permission-mode", "plan"]);
    assert!(inv.args.windows(2).any(|w| w == ["--resume", "s1"]));
    assert!(inv.args.windows(2).any(|w| w == ["--model", "opus"]));
    assert_eq!(&inv.args[inv.args.len() - 2..], ["--max-turns", "2"]);

    let text = Spec {
        name: "t".into(),
        kind: Kind::Text,
        program: "x".into(),
        args: vec!["-m".into(), "{prompt}".into()],
    };
    let inv = text.invocation(&req);
    assert_eq!(inv.stdin, None);
    assert_eq!(&inv.args[..2], ["-m", "hi"]);
}

#[test]
fn runner_streams_a_real_process() {
    // Use our own binary as a "text" provider: portable across macOS, Linux and Windows.
    let spec = Spec {
        name: "self".into(),
        kind: Kind::Text,
        program: env!("CARGO_BIN_EXE_mharness").into(),
        args: vec!["--version".into()],
    };
    let mut seen = Vec::new();
    let outcome = runner::run(
        &spec,
        &Request {
            prompt: "ignored".into(),
            ..Default::default()
        },
        None,
        &mut |e: &Event| seen.push(e.clone()),
    )
    .unwrap();
    assert!(outcome.success());
    assert!(texts(&seen).starts_with("mharness "));
    assert!(matches!(seen.last(), Some(Event::Done { .. })));
}

#[test]
fn runner_reports_missing_program() {
    let spec = Spec {
        name: "nope".into(),
        kind: Kind::Text,
        program: "mharness-missing-cli".into(),
        args: vec![],
    };
    let err = runner::run(&spec, &Request::default(), None, &mut |_: &Event| {}).unwrap_err();
    assert!(err.to_string().contains("not found on PATH"));
}
