//! Spawns a provider CLI and streams its output through a parser.
//!
//! No async runtime: one child process, one stdin writer thread, and a
//! line-buffered reader on the calling thread.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use anyhow::{Context, Result};

use crate::event::Event;
use crate::provider::{Request, Spec};
use crate::which;

/// Receives output while a turn runs.
pub trait Sink {
    fn event(&mut self, event: &Event);
    /// Every stdout line exactly as the provider printed it.
    fn raw(&mut self, _line: &str) {}
}

impl<F: FnMut(&Event)> Sink for F {
    fn event(&mut self, event: &Event) {
        self(event)
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub session: Option<String>,
    pub status: ExitStatus,
    /// True if the provider reported an error result.
    pub is_error: bool,
    /// True if the run was stopped through [`Cancel`].
    pub cancelled: bool,
}

impl Outcome {
    pub fn success(&self) -> bool {
        self.status.success() && !self.is_error && !self.cancelled
    }
}

/// Stops a running turn from another thread by killing the provider process.
#[derive(Clone, Default)]
pub struct Cancel(Arc<Mutex<CancelState>>);

#[derive(Default)]
struct CancelState {
    cancelled: bool,
    child: Option<Child>,
}

impl Cancel {
    pub fn cancel(&self) {
        let mut s = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        s.cancelled = true;
        if let Some(child) = s.child.as_mut() {
            let _ = child.kill();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancelled
    }
}

pub fn run(spec: &Spec, req: &Request, cwd: Option<&Path>, sink: &mut dyn Sink) -> Result<Outcome> {
    run_cancellable(spec, req, cwd, sink, &Cancel::default())
}

pub fn run_cancellable(
    spec: &Spec,
    req: &Request,
    cwd: Option<&Path>,
    sink: &mut dyn Sink,
    cancel: &Cancel,
) -> Result<Outcome> {
    let program = which::find(&spec.program).with_context(|| {
        format!(
            "`{}` not found on PATH (provider `{}`)",
            spec.program, spec.name
        )
    })?;
    let inv = spec.invocation(req);

    let mut cmd = Command::new(&program);
    cmd.args(&inv.args)
        .stdin(if inv.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to start {}", program.display()))?;

    // Feed stdin from its own thread so a chatty child can't deadlock us.
    let writer = match (inv.stdin, child.stdin.take()) {
        (Some(input), Some(mut pipe)) => Some(thread::spawn(move || {
            // A child that exits early closes the pipe; that is not our error to report.
            let _ = pipe.write_all(input.as_bytes());
        })),
        _ => None,
    };

    let mut parser = spec.parser();
    let mut events = Vec::with_capacity(8);
    let mut session = req.resume.clone();
    let mut is_error = false;
    let mut handle = |events: &mut Vec<Event>, sink: &mut dyn Sink| {
        for ev in events.drain(..) {
            match &ev {
                Event::Session { id } => session = Some(id.clone()),
                Event::Done { is_error: e, .. } => is_error |= *e,
                _ => {}
            }
            sink.event(&ev);
        }
    };

    let stdout = child.stdout.take().expect("stdout is piped");
    {
        let mut state = cancel.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.cancelled {
            let _ = child.kill();
        }
        state.child = Some(child);
    }
    let mut reader = BufReader::with_capacity(64 * 1024, stdout);
    let mut buf = Vec::with_capacity(4096);
    loop {
        buf.clear();
        if reader
            .read_until(b'\n', &mut buf)
            .context("reading provider output")?
            == 0
        {
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        sink.raw(line);
        parser.line(line, &mut events);
        handle(&mut events, sink);
    }
    parser.finish(&mut events);
    handle(&mut events, sink);

    if let Some(w) = writer {
        let _ = w.join();
    }
    let mut child = cancel
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .child
        .take()
        .expect("child was stored above");
    let status = child.wait().context("waiting for provider")?;
    Ok(Outcome {
        session,
        status,
        is_error,
        cancelled: cancel.is_cancelled(),
    })
}
