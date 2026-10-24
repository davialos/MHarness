//! MHarness: drive already-authenticated agent CLIs as subprocesses and
//! expose a single normalized event stream.
//!
//! ```no_run
//! use mharness::{Config, Event, Request, runner};
//!
//! let spec = Config::load()?.resolve(Some("claude"))?;
//! let req = Request { prompt: "Summarize README.md".into(), ..Default::default() };
//! let outcome = runner::run(&spec, &req, None, &mut |e: &Event| {
//!     if let Event::Text { text } = e { print!("{text}") }
//! })?;
//! println!("\nsession: {:?}", outcome.session);
//! # Ok::<(), anyhow::Error>(())
//! ```

pub mod config;
pub mod event;
pub mod provider;
pub mod render;
pub mod runner;
pub mod which;

pub use config::Config;
pub use event::Event;
pub use provider::{Kind, Request, Spec};
