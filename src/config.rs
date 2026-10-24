//! Optional TOML config: default provider and custom provider definitions.

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::provider::Spec;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Provider used when `--provider` is not given. Defaults to `claude`.
    pub default: Option<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, Spec>,
}

/// `$MHARNESS_CONFIG`, else `%APPDATA%\mharness\config.toml` on Windows,
/// else `$XDG_CONFIG_HOME/mharness/config.toml` or `~/.config/mharness/config.toml`.
pub fn path() -> Option<PathBuf> {
    if let Some(p) = env::var_os("MHARNESS_CONFIG") {
        return Some(p.into());
    }
    let base = if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.map(|b| b.join("mharness").join("config.toml"))
}

impl Config {
    pub fn load() -> Result<Self> {
        match path() {
            Some(p) if p.is_file() => {
                let text = std::fs::read_to_string(&p)
                    .with_context(|| format!("reading {}", p.display()))?;
                Self::parse(&text).with_context(|| format!("parsing {}", p.display()))
            }
            _ => Ok(Self::default()),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut cfg: Config = toml::from_str(text)?;
        for (name, spec) in &mut cfg.providers {
            spec.name = name.clone();
        }
        Ok(cfg)
    }

    /// Built-ins first (a config entry with the same name replaces one in place),
    /// then the remaining config entries.
    pub fn providers(&self) -> Vec<Spec> {
        let mut all: Vec<Spec> = Spec::builtins()
            .into_iter()
            .map(|b| self.providers.get(&b.name).cloned().unwrap_or(b))
            .collect();
        let extra = self
            .providers
            .values()
            .filter(|p| !all.iter().any(|a| a.name == p.name));
        let extra: Vec<Spec> = extra.cloned().collect();
        all.extend(extra);
        all
    }

    pub fn resolve(&self, name: Option<&str>) -> Result<Spec> {
        let name = name.or(self.default.as_deref()).unwrap_or("claude");
        match self.providers().into_iter().find(|s| s.name == name) {
            Some(s) => Ok(s),
            None => bail!("unknown provider `{name}` (see `mharness providers`)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Kind;

    #[test]
    fn parses_custom_providers_and_overrides() {
        let cfg = Config::parse(
            r#"
            default = "opus"
            [providers.opus]
            kind = "claude"
            program = "claude"
            args = ["--permission-mode", "acceptEdits"]
            [providers.codex]
            kind = "codex"
            program = "/opt/codex/bin/codex"
            "#,
        )
        .unwrap();
        let s = cfg.resolve(None).unwrap();
        assert_eq!((s.name.as_str(), s.kind), ("opus", Kind::Claude));
        assert_eq!(
            cfg.resolve(Some("codex")).unwrap().program,
            "/opt/codex/bin/codex"
        );
        let names: Vec<_> = cfg.providers().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["claude", "codex", "gemini", "opus"]);
        assert!(cfg.resolve(Some("nope")).is_err());
    }

    #[test]
    fn example_config_parses() {
        let cfg = Config::parse(include_str!("../config.example.toml")).unwrap();
        assert_eq!(cfg.resolve(Some("aider")).unwrap().kind, Kind::Text);
    }

    #[test]
    fn defaults_to_claude() {
        assert_eq!(Config::default().resolve(None).unwrap().kind, Kind::Claude);
    }
}
