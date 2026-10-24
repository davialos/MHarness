# MHarness

Rust CLI + library that drives already-authenticated agent CLIs (claude, codex, gemini, any text CLI) as subprocesses and normalizes their output into `Event`s.

- `src/provider/*`: one file per provider: `invocation()` builds argv/stdin, `Parser` maps stdout lines to `Event`.
- `src/runner.rs`: spawn + stream (std threads only, no async runtime; keep it that way for footprint).
- `src/render.rs`: human/JSON/raw sinks. Answer on stdout, tool activity on stderr.
- `src/config.rs`: optional TOML provider config. `src/which.rs`: PATH/PATHEXT lookup (Windows `.cmd` shims).

Checks before pushing: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.
New provider: add a `Kind`, a module with `invocation` + `Parser`, and a fixture test in `tests/parsers.rs`.
