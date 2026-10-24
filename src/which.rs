//! Minimal cross-platform executable lookup.
//!
//! On Windows, npm-installed CLIs are `.cmd` shims that `Command::new("claude")`
//! won't find on its own, so PATHEXT is honoured here.

use std::env;
use std::path::{Path, PathBuf};

pub fn find(program: &str) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.components().count() > 1 || p.is_absolute() {
        return candidates(p).into_iter().find(|c| c.is_file());
    }
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .flat_map(|dir| candidates(&dir.join(program)))
        .find(|c| is_executable(c))
}

#[cfg(windows)]
fn candidates(base: &Path) -> Vec<PathBuf> {
    if base.extension().is_some() {
        return vec![base.to_path_buf()];
    }
    let exts = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    exts.split(';')
        .filter(|e| !e.is_empty())
        .map(|e| {
            let mut s = base.as_os_str().to_owned();
            s.push(e);
            PathBuf::from(s)
        })
        .collect()
}

#[cfg(not(windows))]
fn candidates(base: &Path) -> Vec<PathBuf> {
    vec![base.to_path_buf()]
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_a_system_binary() {
        let name = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(super::find(name).is_some());
    }

    #[test]
    fn missing_binary_is_none() {
        assert!(super::find("definitely-not-a-real-binary-mharness").is_none());
    }
}
