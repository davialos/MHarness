//! Task engine: runs agent turns and git operations on background threads.
//!
//! All state lives behind one mutex. Workers mutate it and set `dirty`; the
//! UI polls `dirty` on a timer, so streaming output never floods the event loop.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use anyhow::{Result, bail};
use mharness::runner::{self, Cancel};
use mharness::{Config, Event, Request, render};

use crate::git::{self, Workspace};
use crate::store::{Status, Store, Task, now};

pub struct State {
    pub store: Store,
    pub running: HashMap<u64, Cancel>,
    /// Something changed that the UI hasn't shown yet.
    pub dirty: bool,
    /// Something changed that hasn't been written to disk yet.
    pub unsaved: bool,
}

#[derive(Clone)]
pub struct Engine(Arc<Mutex<State>>);

pub struct NewTask {
    pub title: String,
    pub prompt: String,
    pub repo: String,
    pub branch: String,
    pub base: String,
    pub provider: String,
    pub model: String,
    pub worktree: bool,
}

fn opt(s: &str) -> Option<String> {
    Some(s.trim().to_string()).filter(|s| !s.is_empty())
}

impl Engine {
    pub fn new(store: Store) -> Self {
        Engine(Arc::new(Mutex::new(State {
            store,
            running: HashMap::new(),
            dirty: true,
            unsaved: false,
        })))
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Mutate one task and flag the change.
    fn with_task(&self, id: u64, f: impl FnOnce(&mut Task)) {
        let mut s = self.lock();
        if let Some(t) = s.store.get_mut(id) {
            f(t);
        }
        s.dirty = true;
        s.unsaved = true;
    }

    pub fn create(&self, n: NewTask) -> u64 {
        let mut s = self.lock();
        let id = s.store.next_id.max(1);
        s.store.next_id = id + 1;
        let branch = opt(&n.branch).unwrap_or_else(|| format!("mh/{}-{id}", git::slug(&n.title)));
        s.store.tasks.push(Task {
            id,
            title: n.title.trim().into(),
            prompt: n.prompt,
            provider: opt(&n.provider).unwrap_or_else(|| "claude".into()),
            model: opt(&n.model),
            repo: n.repo.trim().into(),
            branch,
            base: opt(&n.base),
            worktree: n.worktree,
            status: Status::Backlog,
            repo_path: None,
            dir: None,
            session: None,
            cost_usd: 0.0,
            review_url: None,
            log: String::new(),
            updated: now(),
        });
        s.dirty = true;
        s.unsaved = true;
        id
    }

    /// Run the task's prompt, or `follow_up` as a continuation of its session.
    pub fn start(&self, id: u64, follow_up: Option<String>) {
        let cancel = Cancel::default();
        let task = {
            let mut s = self.lock();
            if s.running.contains_key(&id) {
                return;
            }
            let Some(t) = s.store.get_mut(id) else { return };
            t.status = Status::Running;
            match &follow_up {
                Some(p) => t.note(&format!("\n## you: {p}")),
                None => t.note(&format!(
                    "## run {}",
                    if t.session.is_some() {
                        "(fresh session)"
                    } else {
                        ""
                    }
                )),
            }
            let task = t.clone();
            s.running.insert(id, cancel.clone());
            s.dirty = true;
            s.unsaved = true;
            task
        };
        let engine = self.clone();
        thread::spawn(move || {
            let result = engine.run_turn(&task, follow_up, &cancel);
            let mut s = engine.lock();
            s.running.remove(&id);
            if let Some(t) = s.store.get_mut(id) {
                t.status = match result {
                    Ok(true) => Status::Review,
                    Ok(false) => Status::Failed,
                    Err(e) => {
                        t.note(&format!("error: {e:#}"));
                        Status::Failed
                    }
                };
                if cancel.is_cancelled() {
                    t.note("(stopped)");
                }
                t.touch();
            }
            s.dirty = true;
            s.unsaved = true;
        });
    }

    fn run_turn(&self, task: &Task, follow_up: Option<String>, cancel: &Cancel) -> Result<bool> {
        let id = task.id;
        let spec = Config::load()?.resolve(Some(&task.provider))?;
        self.with_task(id, |t| {
            t.note(&format!("preparing {} on {}…", t.repo, t.branch))
        });
        let repo = git::resolve_repo(&task.repo)?;
        let ws = git::prepare(&repo, &task.branch, task.base.as_deref(), task.worktree)?;
        {
            let mut s = self.lock();
            let clash = s.store.tasks.iter().find(|o| {
                o.id != id
                    && s.running.contains_key(&o.id)
                    && o.dir.as_deref() == Some(ws.dir.as_path())
            });
            if let Some(o) = clash {
                bail!("task #{} is already running in {}", o.id, ws.dir.display());
            }
            if let Some(t) = s.store.get_mut(id) {
                t.repo_path = Some(ws.repo.clone());
                t.dir = Some(ws.dir.clone());
                t.note(&format!("workspace {}", ws.dir.display()));
            }
            s.dirty = true;
        }
        if cancel.is_cancelled() {
            return Ok(false);
        }

        let req = Request {
            prompt: follow_up.clone().unwrap_or_else(|| task.prompt.clone()),
            model: task.model.clone(),
            resume: follow_up.and(task.session.clone()),
            extra_args: Vec::new(),
        };
        let engine = self.clone();
        let mut sink = move |e: &Event| {
            engine.with_task(id, |t| match e {
                Event::Session { id } => t.session = Some(id.clone()),
                Event::Text { text } => t.append(text),
                Event::ToolUse { name, input } => {
                    t.note(&format!("> {name}({})", render::summarize(input)))
                }
                Event::ToolResult {
                    output,
                    is_error: true,
                } => t.note(&format!(
                    "  ! {}",
                    output.lines().next().unwrap_or_default()
                )),
                Event::ToolResult { .. } => {}
                Event::Error { message } => t.note(&format!("error: {message}")),
                Event::Done {
                    cost_usd, is_error, ..
                } => {
                    t.cost_usd += cost_usd.unwrap_or(0.0);
                    if *is_error {
                        t.note("(agent reported an error)");
                    }
                }
            })
        };
        let out = runner::run_cancellable(&spec, &req, Some(&ws.dir), &mut sink, cancel)?;
        Ok(out.success())
    }

    pub fn stop(&self, id: u64) {
        if let Some(c) = self.lock().running.get(&id) {
            c.cancel();
        }
    }

    pub fn move_by(&self, id: u64, delta: i32) {
        let running = self.lock().running.contains_key(&id);
        self.with_task(id, |t| {
            if running {
                return;
            }
            let i = (t.status.index() as i32 + delta).clamp(0, Status::ALL.len() as i32 - 1);
            // Running is only ever entered by actually starting the task.
            let mut next = Status::ALL[i as usize];
            if next == Status::Running {
                next = Status::ALL[(i + delta.signum()) as usize];
            }
            t.status = next;
            t.touch();
        });
    }

    pub fn set_status(&self, id: u64, status: Status) {
        self.with_task(id, |t| {
            t.status = status;
            t.touch();
        });
    }

    pub fn delete(&self, id: u64) {
        let mut s = self.lock();
        if s.running.contains_key(&id) {
            return;
        }
        s.store.tasks.retain(|t| t.id != id);
        s.dirty = true;
        s.unsaved = true;
    }

    fn workspace(&self, id: u64) -> Option<(Workspace, Task)> {
        let s = self.lock();
        let t = s.store.get(id)?;
        let dir = t.dir.clone()?;
        let repo = t.repo_path.clone().unwrap_or_else(|| dir.clone());
        Some((Workspace { repo, dir }, t.clone()))
    }

    /// Run a git operation off the UI thread and log its outcome on the task.
    fn background(
        &self,
        id: u64,
        what: &'static str,
        f: impl FnOnce(&Workspace, &Task) -> Result<String> + Send + 'static,
    ) {
        let Some((ws, task)) = self.workspace(id) else {
            return;
        };
        let engine = self.clone();
        engine.with_task(id, |t| t.note(&format!("{what}…")));
        thread::spawn(move || {
            let res = f(&ws, &task);
            engine.with_task(id, |t| match res {
                Ok(msg) => t.note(&msg),
                Err(e) => t.note(&format!("{what} failed: {e:#}")),
            });
        });
    }

    pub fn commit_and_push(&self, id: u64) {
        self.background(id, "commit & push", |ws, t| {
            git::commit_and_push(&ws.dir, &t.branch, &t.title)
        });
    }

    pub fn open_review(&self, id: u64) {
        let engine = self.clone();
        self.background(id, "opening PR/MR", move |ws, t| {
            let url = git::open_review(ws, &t.branch, t.base.as_deref(), &t.title)?;
            engine.with_task(t.id, |t| t.review_url = Some(url.clone()));
            Ok(format!("review: {url}"))
        });
    }

    pub fn remove_worktree(&self, id: u64) {
        let engine = self.clone();
        self.background(id, "removing worktree", move |ws, t| {
            git::remove_worktree(ws)?;
            engine.with_task(t.id, |t| t.dir = None);
            Ok("worktree removed (branch kept)".into())
        });
    }

    pub fn folder(&self, id: u64) -> Option<PathBuf> {
        self.lock().store.get(id)?.dir.clone()
    }

    /// Save if needed. Called from the UI timer.
    pub fn flush(&self) {
        let mut s = self.lock();
        if s.unsaved {
            match s.store.save() {
                Ok(()) => s.unsaved = false,
                Err(e) => eprintln!("mharness-board: saving board: {e:#}"),
            }
        }
    }
}

pub fn changes(dir: &Path) -> String {
    git::changes(dir)
}
