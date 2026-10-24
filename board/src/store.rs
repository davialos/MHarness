//! Board state: tasks persisted as JSON in the data dir.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Keep at most this much transcript per task in memory and on disk.
const LOG_CAP: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Backlog,
    Running,
    Review,
    Done,
    Failed,
}

impl Status {
    pub const ALL: [Status; 5] = [
        Status::Backlog,
        Status::Running,
        Status::Review,
        Status::Done,
        Status::Failed,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Backlog => "Backlog",
            Status::Running => "Running",
            Status::Review => "Review",
            Status::Done => "Done",
            Status::Failed => "Failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: u64,
    pub title: String,
    pub prompt: String,
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
    /// Local path, clone URL, or `github:owner/repo` / `gitlab:group/repo`.
    pub repo: String,
    pub branch: String,
    #[serde(default)]
    pub base: Option<String>,
    /// Run in a dedicated git worktree (true) or the repo checkout itself (false).
    pub worktree: bool,
    pub status: Status,
    #[serde(default)]
    pub repo_path: Option<PathBuf>,
    #[serde(default)]
    pub dir: Option<PathBuf>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub review_url: Option<String>,
    #[serde(default)]
    pub log: String,
    pub updated: u64,
}

impl Task {
    pub fn append(&mut self, s: &str) {
        self.log.push_str(s);
        if self.log.len() > LOG_CAP {
            let mut cut = self.log.len() - LOG_CAP / 2;
            while !self.log.is_char_boundary(cut) {
                cut += 1;
            }
            self.log.replace_range(..cut, "…\n");
        }
        self.touch();
    }

    /// Append on a fresh line.
    pub fn note(&mut self, s: &str) {
        if !self.log.is_empty() && !self.log.ends_with('\n') {
            self.log.push('\n');
        }
        self.append(s);
        self.append("\n");
    }

    pub fn touch(&mut self) {
        self.updated = now();
    }

    pub fn last_line(&self) -> &str {
        self.log
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    pub next_id: u64,
    pub tasks: Vec<Task>,
}

impl Store {
    pub fn path() -> PathBuf {
        crate::git::data_dir().join("board.json")
    }

    pub fn load() -> Result<Self> {
        let p = Self::path();
        if !p.exists() {
            return Ok(Self {
                next_id: 1,
                tasks: Vec::new(),
            });
        }
        let text =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        let mut s: Store =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        // A previous instance can't still be running these.
        for t in &mut s.tasks {
            if t.status == Status::Running {
                t.status = Status::Failed;
                t.note("(interrupted: board was closed while running)");
            }
        }
        Ok(s)
    }

    /// Atomic write: temp file then rename.
    pub fn save(&self) -> Result<()> {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &p)?;
        Ok(())
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    pub fn get(&self, id: u64) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> Task {
        Task {
            id: 1,
            title: "t".into(),
            prompt: "p".into(),
            provider: "claude".into(),
            model: None,
            repo: ".".into(),
            branch: "b".into(),
            base: None,
            worktree: true,
            status: Status::Backlog,
            repo_path: None,
            dir: None,
            session: None,
            cost_usd: 0.0,
            review_url: None,
            log: String::new(),
            updated: 0,
        }
    }

    #[test]
    fn log_is_capped_on_char_boundary() {
        let mut t = task();
        for _ in 0..LOG_CAP / 3 {
            t.append("é€");
        }
        assert!(t.log.len() <= LOG_CAP);
        assert!(t.log.starts_with('…'));
    }

    #[test]
    fn note_starts_on_new_line() {
        let mut t = task();
        t.append("partial");
        t.note("> Bash(ls)");
        assert_eq!(t.log, "partial\n> Bash(ls)\n");
        assert_eq!(t.last_line(), "> Bash(ls)");
    }

    #[test]
    fn roundtrips_json() {
        let s = Store {
            next_id: 2,
            tasks: vec![task()],
        };
        let back: Store = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.tasks[0].status, Status::Backlog);
    }
}
