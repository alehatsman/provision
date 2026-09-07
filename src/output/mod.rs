//! Rendering. The output layer knows nothing about actions: it is handed a
//! name, a status and a duration (spec §9).

pub mod event;
pub mod json;
pub mod text;

use event::{Event, Summary};
use std::path::Path;

pub trait Sink {
    /// The step is about to run. Only the TTY renderer does anything with
    /// this — it is where the spinner lives.
    fn start(&mut self, _name: &str, _depth: usize) {}
    fn step(&mut self, ev: &Event);
    fn summary(&mut self, plan: &Path, s: &Summary);
}

/// `validate` produces no step list, so it needs somewhere for the walk to
/// report into that costs nothing.
pub struct Silent;

impl Sink for Silent {
    fn step(&mut self, _ev: &Event) {}
    fn summary(&mut self, _plan: &Path, _s: &Summary) {}
}

/// `--json` puts machine output on stdout and human output on stderr (§9.3),
/// which means two renderers, not one that switches.
pub struct Both(pub Box<dyn Sink>, pub Box<dyn Sink>);

impl Sink for Both {
    fn start(&mut self, name: &str, depth: usize) {
        self.0.start(name, depth);
        self.1.start(name, depth);
    }
    fn step(&mut self, ev: &Event) {
        self.0.step(ev);
        self.1.step(ev);
    }
    fn summary(&mut self, plan: &Path, s: &Summary) {
        self.0.summary(plan, s);
        self.1.summary(plan, s);
    }
}

/// Diagnostics are not events: they have no step, and they are the only thing
/// `validate` prints.
pub fn diagnostics(
    out: &mut impl std::io::Write,
    diags: &crate::error::Diags,
    base: &Path,
) -> std::io::Result<()> {
    for d in diags.iter() {
        writeln!(out, "  error: {}", d.display_from(base))?;
    }
    Ok(())
}
