//! Rendering. The output layer knows nothing about actions: it is handed a
//! name, a status and a duration (spec §9).

pub(crate) mod event;
pub(crate) mod json;
pub(crate) mod text;

use event::{Event, Summary};
use std::path::Path;

pub(crate) trait Sink {
    /// The step is about to run. Only the TTY renderer does anything with
    /// this — it is where the spinner lives.
    fn start(&mut self, _name: &str, _depth: usize) {}
    fn step(&mut self, ev: &Event);
    fn summary(&mut self, plan: &Path, s: &Summary);
}

/// `validate` produces no step list, so it needs somewhere for the walk to
/// report into that costs nothing.
pub(crate) struct Silent;

impl Sink for Silent {
    fn step(&mut self, _ev: &Event) {}
    fn summary(&mut self, _plan: &Path, _s: &Summary) {}
}

/// `--json` puts machine output on stdout and human output on stderr (§9.3),
/// which means two renderers, not one that switches.
pub(crate) struct Both(pub Box<dyn Sink>, pub Box<dyn Sink>);

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

/// One line to stdout, for the commands that print directly rather than
/// through a `Sink` — `list`, `facts`, and `validate`'s one ok line.
///
/// Spec §10: `provision list tasks/ | head -3` is ordinary use, and the reader
/// asking for less is not an error. Rust ignores `SIGPIPE`, so a closed pipe
/// arrives as an `EPIPE` write error and `println!` *panics* on it — a
/// backtrace on a command a person had every reason to pipe.
///
/// The usual one-line fix is to restore `SIGPIPE` to its default and let the
/// process die from the signal. Not here: `exec/process.rs` writes a sudo
/// password into a child's stdin and depends on that write returning `EPIPE`
/// rather than killing the run, so the signal stays ignored and the printing
/// is what changes. The `Sink` renderers already drop failed writes and were
/// never affected; this is the same rule, said once, for the rest.
pub(crate) fn line(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    #[expect(
        clippy::let_underscore_must_use,
        reason = "a stdout nobody is reading is the documented case above"
    )]
    let _ = out.write_fmt(args);
    #[expect(clippy::let_underscore_must_use, reason = "as above")]
    let _ = out.write_all(b"\n");
}

/// Diagnostics are not events: they have no step, and they are the only thing
/// `validate` prints.
pub(crate) fn diagnostics(
    out: &mut impl std::io::Write,
    diags: &crate::error::Diags,
    base: &Path,
) -> std::io::Result<()> {
    for d in diags.iter() {
        writeln!(out, "  error: {}", d.display_from(base))?;
    }
    Ok(())
}
