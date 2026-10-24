// Hide the console window for release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod engine;
mod git;
mod store;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ModelRc, SharedString, Timer, TimerMode, VecModel};

use engine::Engine;
use store::{Status, Store, Task};

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    let engine = Engine::new(Store::load()?);
    let ui = AppWindow::new()?;

    // Default provider first, so it is what the new-task dialog preselects.
    let providers: Vec<SharedString> = mharness::Config::load()
        .map(|c| {
            let default = c.default.clone().unwrap_or_else(|| "claude".into());
            let mut names: Vec<String> = c.providers().into_iter().map(|p| p.name).collect();
            names.sort_by_key(|n| *n != default);
            names.into_iter().map(Into::into).collect()
        })
        .unwrap_or_else(|_| vec!["claude".into()]);
    ui.set_providers(ModelRc::new(VecModel::from(providers)));
    if let Ok(cwd) = std::env::current_dir()
        && git::git(&cwd, &["rev-parse", "--show-toplevel"]).is_ok()
    {
        ui.set_default_repo(cwd.to_string_lossy().as_ref().into());
    }

    let selected = Rc::new(RefCell::new(None::<u64>));
    let view = Rc::new(RefCell::new(View::default()));

    wire_callbacks(&ui, &engine, &selected, &view);

    // Poll engine state; cheap when nothing changed.
    let timer = Timer::default();
    {
        let (ui_weak, engine, selected, view) =
            (ui.as_weak(), engine.clone(), selected.clone(), view.clone());
        timer.start(TimerMode::Repeated, Duration::from_millis(150), move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            refresh(
                &ui,
                &engine,
                *selected.borrow(),
                &mut view.borrow_mut(),
                false,
            );
            if view.borrow().last_save.elapsed() > Duration::from_secs(2) {
                engine.flush();
                view.borrow_mut().last_save = Instant::now();
            }
        });
    }

    ui.show()?;
    ui.window().set_size(slint::LogicalSize::new(1360.0, 840.0));
    slint::run_event_loop()?;
    // Stop agents we started, then persist.
    for cancel in engine.lock().running.values() {
        cancel.cancel();
    }
    engine.flush();
    Ok(())
}

struct View {
    last_save: Instant,
    changes_for: Option<u64>,
    changes_at: Instant,
    changes: String,
}

impl Default for View {
    fn default() -> Self {
        View {
            last_save: Instant::now(),
            changes_for: None,
            changes_at: Instant::now(),
            changes: String::new(),
        }
    }
}

fn wire_callbacks(
    ui: &AppWindow,
    engine: &Engine,
    selected: &Rc<RefCell<Option<u64>>>,
    view: &Rc<RefCell<View>>,
) {
    let id = |i: i32| i as u64;
    {
        let (ui_weak, engine, selected, view) =
            (ui.as_weak(), engine.clone(), selected.clone(), view.clone());
        ui.on_select(move |i| {
            *selected.borrow_mut() = (i >= 0).then_some(i as u64);
            if let Some(ui) = ui_weak.upgrade() {
                refresh(
                    &ui,
                    &engine,
                    *selected.borrow(),
                    &mut view.borrow_mut(),
                    true,
                );
            }
        });
    }
    {
        let (ui_weak, engine, selected, view) =
            (ui.as_weak(), engine.clone(), selected.clone(), view.clone());
        ui.on_create(move |n| {
            let start = n.start;
            let new_id = engine.create(engine::NewTask {
                title: n.title.into(),
                prompt: n.prompt.into(),
                repo: n.repo.to_string(),
                branch: n.branch.into(),
                base: n.base.into(),
                provider: n.provider.into(),
                model: n.model.into(),
                worktree: n.worktree,
            });
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_default_repo(n.repo);
                *selected.borrow_mut() = Some(new_id);
                refresh(&ui, &engine, Some(new_id), &mut view.borrow_mut(), true);
            }
            if start {
                engine.start(new_id, None);
            }
        });
    }
    macro_rules! on {
        ($setter:ident, |$e:ident, $i:ident| $body:expr) => {{
            let $e = engine.clone();
            ui.$setter(move |$i| {
                let $i = id($i);
                $body;
            });
        }};
    }
    on!(on_start, |e, i| e.start(i, None));
    on!(on_stop, |e, i| e.stop(i));
    on!(on_set_done, |e, i| e.set_status(i, Status::Done));
    on!(on_push, |e, i| e.commit_and_push(i));
    on!(on_open_review, |e, i| e.open_review(i));
    on!(on_remove_worktree, |e, i| e.remove_worktree(i));
    on!(on_open_folder, |e, i| if let Some(d) = e.folder(i) {
        open_folder(&d)
    });
    {
        let (engine, selected) = (engine.clone(), selected.clone());
        ui.on_delete(move |i| {
            engine.delete(id(i));
            *selected.borrow_mut() = None;
        });
    }
    {
        let engine = engine.clone();
        ui.on_move(move |i, d| engine.move_by(id(i), d));
    }
    {
        let engine = engine.clone();
        ui.on_follow_up(move |i, text| {
            let text = text.trim().to_string();
            if !text.is_empty() {
                engine.start(id(i), Some(text));
            }
        });
    }
}

/// Rebuild the board models from engine state if anything changed.
fn refresh(ui: &AppWindow, engine: &Engine, selected: Option<u64>, view: &mut View, force: bool) {
    let mut s = engine.lock();
    if !s.dirty && !force {
        return;
    }
    s.dirty = false;

    let mut columns: Vec<Vec<Card>> = vec![Vec::new(); Status::ALL.len()];
    let mut tasks: Vec<&Task> = s.store.tasks.iter().collect();
    tasks.sort_by_key(|t| std::cmp::Reverse(t.updated));
    for t in &tasks {
        columns[t.status.index()].push(Card {
            id: t.id as i32,
            title: t.title.as_str().into(),
            subtitle: format!("{} · {}", repo_name(&t.repo), t.branch).into(),
            provider: t
                .model
                .as_ref()
                .map_or(t.provider.clone(), |m| format!("{} · {m}", t.provider))
                .into(),
            last: t.last_line().chars().take(120).collect::<String>().into(),
            status: t.status.index() as i32,
            running: s.running.contains_key(&t.id),
        });
    }
    let running = s.running.len();
    let total = s.store.tasks.len();
    let model: Vec<Column> = Status::ALL
        .iter()
        .zip(columns)
        .map(|(st, cards)| Column {
            name: st.label().into(),
            status: st.index() as i32,
            cards: ModelRc::new(VecModel::from(cards)),
        })
        .collect();
    ui.set_columns(ModelRc::new(VecModel::from(model)));
    ui.set_status_text(
        format!(
            "{total} tasks · {running} running · data in {}",
            git::data_dir().display()
        )
        .into(),
    );

    let detail = selected.and_then(|id| s.store.get(id)).map(|t| {
        let is_running = s.running.contains_key(&t.id);
        // `git status` is cheap but not free; refresh it on selection or every 3 s.
        if let Some(dir) = &t.dir
            && (view.changes_for != Some(t.id)
                || view.changes_at.elapsed() > Duration::from_secs(3))
        {
            view.changes = engine::changes(dir);
            view.changes_for = Some(t.id);
            view.changes_at = Instant::now();
        } else if t.dir.is_none() {
            view.changes.clear();
        }
        Detail {
            has: true,
            id: t.id as i32,
            title: t.title.as_str().into(),
            prompt: t.prompt.as_str().into(),
            status: t.status.index() as i32,
            status_label: t.status.label().into(),
            provider: t
                .model
                .as_ref()
                .map_or(t.provider.clone(), |m| format!("{} ({m})", t.provider))
                .into(),
            repo: t.repo.as_str().into(),
            branch: match &t.base {
                Some(b) => format!("{} ← {b}", t.branch),
                None => t.branch.clone(),
            }
            .into(),
            dir: t
                .dir
                .as_ref()
                .map(|d| d.display().to_string())
                .unwrap_or_default()
                .into(),
            session: t.session.clone().unwrap_or_default().into(),
            cost: if t.cost_usd > 0.0 {
                format!("${:.4}", t.cost_usd)
            } else {
                String::new()
            }
            .into(),
            review_url: t.review_url.clone().unwrap_or_default().into(),
            changes: view.changes.as_str().into(),
            log: format!("{}\n\n{}", t.prompt, t.log).into(),
            running: is_running,
            worktree: t.worktree,
        }
    });
    drop(s);
    ui.set_detail(detail.unwrap_or_default());
}

fn repo_name(repo: &str) -> &str {
    repo.trim_end_matches(['/', '\\'])
        .trim_end_matches(".git")
        .rsplit(['/', '\\', ':'])
        .next()
        .unwrap_or(repo)
}

fn open_folder(dir: &Path) {
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(opener).arg(dir).spawn() {
        eprintln!("mharness-board: {opener}: {e}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn repo_names() {
        assert_eq!(super::repo_name("github:davialos/MHarness"), "MHarness");
        assert_eq!(super::repo_name("git@gitlab.com:g/p.git"), "p");
        assert_eq!(super::repo_name("/home/me/code/app/"), "app");
        assert_eq!(super::repo_name(r"C:\code\app"), "app");
    }
}
