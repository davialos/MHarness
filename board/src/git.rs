//! Git plumbing through the user's own `git`, `gh` and `glab` CLIs, so their
//! existing SSH keys, credential helpers and forge logins are used as-is.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use mharness::which;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forge {
    GitHub,
    GitLab,
    Other,
}

/// Where a task's agent runs.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub repo: PathBuf,
    pub dir: PathBuf,
}

/// Run git in `dir`, returning trimmed stdout or an error carrying stderr.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    run_tool("git", dir, args)
}

fn run_tool(tool: &str, dir: &Path, args: &[&str]) -> Result<String> {
    let program = which::find(tool).with_context(|| format!("`{tool}` is not installed"))?;
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .with_context(|| format!("running {tool}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{tool} {}: {}", args.join(" "), err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn data_dir() -> PathBuf {
    use std::env::var_os;
    if let Some(p) = var_os("MHARNESS_DATA") {
        return p.into();
    }
    let home = || {
        var_os("HOME")
            .or_else(|| var_os("USERPROFILE"))
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support"))
    } else {
        var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".local/share")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("mharness")
}

/// True for clone URLs and `github:owner/repo` / `gitlab:owner/repo` shorthands.
pub fn is_remote(source: &str) -> bool {
    source.contains("://")
        || source.starts_with("git@")
        || source.starts_with("github:")
        || source.starts_with("gitlab:")
}

/// Expand shorthands to a clone URL.
pub fn clone_url(source: &str) -> String {
    if let Some(r) = source.strip_prefix("github:") {
        format!("https://github.com/{}.git", r.trim_end_matches(".git"))
    } else if let Some(r) = source.strip_prefix("gitlab:") {
        format!("https://gitlab.com/{}.git", r.trim_end_matches(".git"))
    } else {
        source.to_string()
    }
}

/// `host/owner/repo` from any clone URL, used as the cache path.
fn url_key(url: &str) -> String {
    let s = url.split("://").nth(1).unwrap_or(url);
    let s = s.strip_prefix("git@").unwrap_or(s);
    let s = s.rsplit_once('@').map_or(s, |(_, rest)| rest);
    s.replacen(':', "/", 1)
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .split('/')
        .filter(|p| !p.is_empty() && *p != "..")
        .collect::<Vec<_>>()
        .join("/")
}

/// Local repository root for a task source: a path, or a remote that gets
/// cloned (first time) or fetched into the data dir.
pub fn resolve_repo(source: &str) -> Result<PathBuf> {
    let source = source.trim();
    if source.is_empty() {
        bail!("repository is empty");
    }
    if is_remote(source) {
        let url = clone_url(source);
        let dir = data_dir().join("repos").join(url_key(&url));
        if dir.join(".git").exists() {
            git(&dir, &["fetch", "--all", "--prune"])?;
        } else {
            std::fs::create_dir_all(dir.parent().unwrap_or(&dir))?;
            let parent = dir.parent().unwrap_or(&dir).to_path_buf();
            git(&parent, &["clone", &url, &dir.to_string_lossy()])?;
        }
        return Ok(dir);
    }
    let path = Path::new(source);
    if !path.is_dir() {
        bail!("{source} is not a directory");
    }
    let top = git(path, &["rev-parse", "--show-toplevel"])
        .with_context(|| format!("{source} is not a git repository"))?;
    Ok(PathBuf::from(top))
}

pub fn forge(repo: &Path) -> Forge {
    match git(repo, &["remote", "get-url", "origin"]) {
        Ok(url) if url.contains("github") => Forge::GitHub,
        Ok(url) if url.contains("gitlab") => Forge::GitLab,
        _ => Forge::Other,
    }
}

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.truncate(40);
    out.trim_end_matches('-').to_string()
}

fn has_ref(repo: &Path, r: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", r]).is_ok()
}

/// Create (or reuse) the branch, in a dedicated worktree or in the repo checkout itself.
pub fn prepare(repo: &Path, branch: &str, base: Option<&str>, worktree: bool) -> Result<Workspace> {
    let local = format!("refs/heads/{branch}");
    let remote = format!("refs/remotes/origin/{branch}");
    if !worktree {
        if has_ref(repo, &local) {
            git(repo, &["switch", branch])?;
        } else if has_ref(repo, &remote) {
            git(repo, &["switch", "--track", &format!("origin/{branch}")])?;
        } else {
            let mut args = vec!["switch", "-c", branch];
            args.extend(base);
            git(repo, &args)?;
        }
        return Ok(Workspace {
            repo: repo.into(),
            dir: repo.into(),
        });
    }

    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = data_dir()
        .join("worktrees")
        .join(slug(&name))
        .join(slug(branch));
    if dir.join(".git").exists() {
        return Ok(Workspace {
            repo: repo.into(),
            dir,
        });
    }
    std::fs::create_dir_all(dir.parent().unwrap_or(&dir))?;
    let d = dir.to_string_lossy().into_owned();
    if has_ref(repo, &local) {
        git(repo, &["worktree", "add", &d, branch])?;
    } else if has_ref(repo, &remote) {
        git(
            repo,
            &[
                "worktree",
                "add",
                "--track",
                "-b",
                branch,
                &d,
                &format!("origin/{branch}"),
            ],
        )?;
    } else {
        let mut args = vec!["worktree", "add", "-b", branch, &d];
        args.extend(base);
        git(repo, &args)?;
    }
    Ok(Workspace {
        repo: repo.into(),
        dir,
    })
}

/// `git status` summary of the task's checkout.
pub fn changes(dir: &Path) -> String {
    git(dir, &["status", "--short", "--branch"]).unwrap_or_else(|e| e.to_string())
}

/// Commit anything the agent left uncommitted, then push the branch.
pub fn commit_and_push(dir: &Path, branch: &str, message: &str) -> Result<String> {
    if !git(dir, &["status", "--porcelain"])?.is_empty() {
        git(dir, &["add", "-A"])?;
        git(dir, &["commit", "-m", message])?;
    }
    git(dir, &["push", "-u", "origin", branch])?;
    Ok(format!("pushed {branch}"))
}

/// Open a PR (GitHub, via `gh`) or MR (GitLab, via `glab`); returns its URL.
pub fn open_review(
    ws: &Workspace,
    branch: &str,
    base: Option<&str>,
    title: &str,
) -> Result<String> {
    let out = match forge(&ws.repo) {
        Forge::GitHub => {
            let mut args = vec!["pr", "create", "--fill", "--title", title, "--head", branch];
            if let Some(b) = base {
                args.extend(["--base", b]);
            }
            run_tool("gh", &ws.dir, &args)?
        }
        Forge::GitLab => {
            let mut args = vec![
                "mr",
                "create",
                "--fill",
                "--yes",
                "--title",
                title,
                "--source-branch",
                branch,
            ];
            if let Some(b) = base {
                args.extend(["--target-branch", b]);
            }
            run_tool("glab", &ws.dir, &args)?
        }
        Forge::Other => bail!("origin is neither GitHub nor GitLab"),
    };
    Ok(out
        .lines()
        .rev()
        .find(|l| l.contains("http"))
        .unwrap_or(&out)
        .trim()
        .to_string())
}

pub fn remove_worktree(ws: &Workspace) -> Result<()> {
    if ws.dir == ws.repo {
        bail!("task runs in the repository checkout, not a worktree");
    }
    git(
        &ws.repo,
        &["worktree", "remove", "--force", &ws.dir.to_string_lossy()],
    )?;
    let _ = git(&ws.repo, &["worktree", "prune"]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_sources() {
        assert!(is_remote("github:davialos/MHarness"));
        assert!(is_remote("git@gitlab.com:group/sub/proj.git"));
        assert!(!is_remote("/home/me/code"));
        assert!(!is_remote(r"C:\code\repo"));
        assert_eq!(clone_url("gitlab:g/p"), "https://gitlab.com/g/p.git");
        assert_eq!(
            url_key("git@gitlab.com:group/sub/proj.git"),
            "gitlab.com/group/sub/proj"
        );
        assert_eq!(url_key("https://user@github.com/a/b.git"), "github.com/a/b");
        assert_eq!(url_key("https://x.com/../../etc"), "x.com/etc");
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Fix: login bug!"), "fix-login-bug");
        assert_eq!(slug("mh/feature-x"), "mh-feature-x");
    }

    #[test]
    fn worktree_lifecycle_in_a_temp_repo() {
        let root = std::env::temp_dir().join(format!("mh-board-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        // SAFETY: single-threaded section of this test; other tests don't read MHARNESS_DATA.
        unsafe { std::env::set_var("MHARNESS_DATA", root.join("data")) };
        for args in [
            &["init", "-q", "-b", "main"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        ] {
            git(&repo, args).unwrap();
        }
        let repo = resolve_repo(&repo.to_string_lossy()).unwrap();
        let ws = prepare(&repo, "mh/task-1", Some("main"), true).unwrap();
        assert!(ws.dir.join(".git").exists());
        assert_eq!(
            git(&ws.dir, &["branch", "--show-current"]).unwrap(),
            "mh/task-1"
        );
        // Reuse on second prepare.
        assert_eq!(prepare(&repo, "mh/task-1", None, true).unwrap().dir, ws.dir);
        std::fs::write(ws.dir.join("a.txt"), "x").unwrap();
        assert!(changes(&ws.dir).contains("a.txt"));
        remove_worktree(&ws).unwrap();
        assert!(!ws.dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
