use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

use mharness::render::{Human, Json, Raw};
use mharness::runner::{self, Sink};
use mharness::{Config, Request, Spec, config, which};

#[derive(Parser)]
#[command(
    name = "mharness",
    version,
    about = "Drive your authenticated agent CLIs from one lightweight harness"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one prompt and stream the answer (prompt from args or stdin).
    Run {
        #[command(flatten)]
        opts: TurnOpts,
        /// Emit normalized NDJSON events instead of text.
        #[arg(long, conflicts_with = "raw")]
        json: bool,
        /// Pass the provider's own output through unchanged.
        #[arg(long)]
        raw: bool,
        /// Prompt text. Anything after `--` is passed to the provider CLI.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        prompt: Vec<String>,
    },
    /// Interactive multi-turn chat that keeps the provider session between turns.
    Chat {
        #[command(flatten)]
        opts: TurnOpts,
        /// Extra arguments for the provider CLI (after `--`).
        #[arg(last = true)]
        extra: Vec<String>,
    },
    /// List providers and whether their CLI is installed.
    Providers,
    /// Print the config file location.
    ConfigPath,
}

#[derive(Args)]
struct TurnOpts {
    /// Provider name: claude, codex, gemini, or one from your config.
    #[arg(short, long, env = "MHARNESS_PROVIDER")]
    provider: Option<String>,
    /// Model passed to the provider CLI.
    #[arg(short, long)]
    model: Option<String>,
    /// Resume an existing provider session id.
    #[arg(short, long)]
    resume: Option<String>,
    /// Working directory for the agent.
    #[arg(short = 'C', long)]
    cwd: Option<PathBuf>,
    /// Show session ids, tool results and cost on stderr.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("mharness: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn real_main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let cfg = Config::load()?;
    match cli.cmd {
        Cmd::Run {
            opts,
            json,
            raw,
            prompt,
        } => {
            let spec = cfg.resolve(opts.provider.as_deref())?;
            let (prompt, extra) = split_extra(prompt);
            let prompt = if prompt.is_empty() {
                read_stdin_prompt()?
            } else {
                prompt.join(" ")
            };
            let req = Request {
                prompt,
                model: opts.model,
                resume: opts.resume,
                extra_args: extra,
            };
            let mut human = Human::new(opts.verbose);
            let sink: &mut dyn Sink = if json {
                &mut Json
            } else if raw {
                &mut Raw
            } else {
                &mut human
            };
            let outcome = runner::run(&spec, &req, opts.cwd.as_deref(), sink)?;
            human.finish();
            Ok(exit(outcome.success()))
        }
        Cmd::Chat { opts, extra } => chat(&cfg.resolve(opts.provider.as_deref())?, opts, extra),
        Cmd::Providers => {
            let default = cfg.default.as_deref().unwrap_or("claude");
            for spec in cfg.providers() {
                let mark = if spec.name == default { "*" } else { " " };
                let status = match which::find(&spec.program) {
                    Some(path) => format!(
                        "{} ({})",
                        version(&path).unwrap_or_default(),
                        path.display()
                    ),
                    None => "not installed".into(),
                };
                println!(
                    "{mark} {:<12} {:<7} {status}",
                    spec.name,
                    format!("{:?}", spec.kind).to_lowercase()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::ConfigPath => {
            match config::path() {
                Some(p) => println!("{}", p.display()),
                None => bail!("could not determine a config directory"),
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn chat(spec: &Spec, opts: TurnOpts, extra: Vec<String>) -> Result<ExitCode> {
    let mut session = opts.resume;
    let mut sink = Human::new(opts.verbose);
    let stdin = io::stdin();
    let interactive = stdin.is_terminal();
    if interactive {
        eprintln!(
            "mharness chat with `{}`. /new resets the session, /session shows it, /exit quits.",
            spec.name
        );
    }
    let mut line = String::new();
    loop {
        if interactive {
            eprint!("\n> ");
            io::stderr().flush()?;
        }
        line.clear();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let prompt = line.trim();
        match prompt {
            "" => continue,
            "/exit" | "/quit" => break,
            "/new" => {
                session = None;
                eprintln!("(new session)");
                continue;
            }
            "/session" => {
                eprintln!("{}", session.as_deref().unwrap_or("(none yet)"));
                continue;
            }
            _ => {}
        }
        let req = Request {
            prompt: prompt.to_string(),
            model: opts.model.clone(),
            resume: session.clone().filter(|_| spec.supports_resume()),
            extra_args: extra.clone(),
        };
        let outcome = runner::run(spec, &req, opts.cwd.as_deref(), &mut sink)?;
        sink.finish();
        if outcome.session.is_some() {
            session = outcome.session;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Split `a b -- --flag x` into (["a","b"], ["--flag","x"]).
fn split_extra(mut words: Vec<String>) -> (Vec<String>, Vec<String>) {
    match words.iter().position(|w| w == "--") {
        Some(i) => {
            let extra = words.split_off(i + 1);
            words.pop();
            (words, extra)
        }
        None => (words, Vec::new()),
    }
}

fn read_stdin_prompt() -> Result<String> {
    if io::stdin().is_terminal() {
        bail!("no prompt given (pass it as an argument or pipe it on stdin)");
    }
    let mut s = String::new();
    io::stdin().read_to_string(&mut s)?;
    if s.trim().is_empty() {
        bail!("empty prompt on stdin");
    }
    Ok(s)
}

fn version(path: &std::path::Path) -> Option<String> {
    let out = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    Some(s.lines().next()?.trim().to_string())
}

fn exit(ok: bool) -> ExitCode {
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::split_extra;

    #[test]
    fn splits_passthrough_args() {
        let v = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(
            split_extra(v("fix the bug -- --max-turns 3")),
            (v("fix the bug"), v("--max-turns 3"))
        );
        assert_eq!(split_extra(v("hello")), (v("hello"), vec![]));
    }
}
