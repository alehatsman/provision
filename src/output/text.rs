//! The human renderer. Spec §9.1 and §9.2 describe the same lines twice —
//! once with a spinner and color, once without — so this is one renderer with
//! two switches rather than two that drift apart.

use super::Sink;
use super::window::window;
use crate::report::{Event, Manifest, Status, Summary, human};
use anstyle::{AnsiColor, Style};
use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The head-and-tail window of §9.1, per captured stream and not shared
/// between the two: the cap is there so a noisy step cannot bury the summary,
/// and a shared budget, spent by whichever stream printed first, is how stdout
/// used to vanish. The tail is the larger end because a command that fails
/// usually fails last; the head is there at all because that is where a tool
/// says what is wrong (#59).
const HEAD: usize = 10;
const TAIL: usize = 20;
const NAME_WIDTH: usize = 48;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Out,
    Err,
}

pub(crate) struct Text {
    target: Target,
    /// Animate a live line while a step runs. Off when output is redirected,
    /// and off under `--stream`, where the child is writing to the same
    /// terminal and the two would garble each other.
    spinner: bool,
    verbose: bool,
    hide_skipped: bool,
    /// Spec §9.1. `apply` prints diffs under `changed` lines too, so this
    /// suppresses them there as well; it is not a plan-only flag.
    no_diff: bool,
    base: PathBuf,
    /// The call path and file whose header was printed last, so nested
    /// `import`/`use` shows as one dim header rather than a path on every
    /// line. The call path is part of the key because two `use`s of one
    /// component are two pieces of work: keying on the file alone printed
    /// one header for both and made a plan under-report what it was doing.
    current: Option<(Vec<String>, PathBuf)>,
    /// The nested call sites the walk is currently inside, outermost first,
    /// each accumulating the steps under it. §9.1: a call site closes with
    /// its own summary line under `--verbose`, so the counts have to be kept
    /// while it is open — the run's own summary cannot answer for a block.
    open: Vec<Call>,
    live: Option<Spinner>,
}

/// One open call site: the label its header printed, and what has happened
/// under it so far.
struct Call {
    label: String,
    summary: Summary,
}

impl Text {
    pub(crate) fn new(target: Target, base: PathBuf) -> Text {
        let tty = match target {
            Target::Out => std::io::stdout().is_terminal(),
            Target::Err => std::io::stderr().is_terminal(),
        };
        Text {
            target,
            spinner: tty,
            verbose: false,
            hide_skipped: false,
            no_diff: false,
            base,
            current: None,
            open: Vec::new(),
            live: None,
        }
    }

    pub(crate) fn verbose(mut self, on: bool) -> Text {
        self.verbose = on;
        self
    }

    pub(crate) fn hide_skipped(mut self, on: bool) -> Text {
        self.hide_skipped = on;
        self
    }

    pub(crate) fn no_diff(mut self, on: bool) -> Text {
        self.no_diff = on;
        self
    }

    /// `--stream` hands the terminal to the child, so the spinner stands down.
    pub(crate) fn no_spinner(mut self) -> Text {
        self.spinner = false;
        self
    }

    #[expect(
        clippy::let_underscore_must_use,
        reason = "a failed write to the terminal has nowhere to be reported: the terminal is where a report would go"
    )]
    fn write(&self, f: std::fmt::Arguments<'_>) {
        match self.target {
            Target::Out => {
                let mut w = anstream::stdout();
                let _ = w.write_fmt(f);
                let _ = w.flush();
            }
            Target::Err => {
                let mut w = anstream::stderr();
                let _ = w.write_fmt(f);
                let _ = w.flush();
            }
        }
    }

    fn header(&mut self, ev: &Event) {
        // Every level of the call path that is new since the last step gets
        // its own line, indented to its depth: the call site's `name:` when
        // it has one, the file it names otherwise. Printing only the
        // innermost one hid the components in between, and keying on the
        // file alone printed one header for two `use`s of the same component
        // — two pieces of work reported as one (§9.1).
        let previous = self.current.take().unwrap_or_default();
        let dim = Style::new().dimmed();
        // A level the step's call path no longer carries is a block the walk
        // has left, and that is where its summary line belongs. Innermost
        // first, mirroring the headers that opened them.
        let keep = self
            .open
            .iter()
            .zip(ev.via.iter())
            .take_while(|(call, label)| &call.label == *label)
            .count();
        self.close_to(keep);
        // The latch: once a level differs from the last step's path, every
        // level under it is new too, whatever its label says. Two components
        // that each `use` the same file unnamed produce the same label at
        // the same index — `lib/common.yml` under `A` and again under `B` —
        // and comparing index by index swallowed the second, so the step
        // read as if it came straight from `B`. That is the under-reporting
        // of nesting §9.1's "one header per call site" rules out.
        let mut diverged = false;
        for (i, label) in ev.via.iter().enumerate() {
            if !diverged && previous.0.get(i) == Some(label) {
                continue;
            }
            diverged = true;
            self.write(format_args!("  {}{dim}{label}{dim:#}\n", indent(i)));
        }
        // `close_to` left the open stack at the levels this step shares, so
        // what remains of its call path is what the header loop just printed.
        for label in ev.via.iter().skip(self.open.len()) {
            self.open.push(Call {
                label: label.clone(),
                summary: Summary::default(),
            });
        }
        // At depth 0 there is no call site, so the file is the header — and
        // a bare `import` chain inside one file still changes it.
        if ev.via.is_empty() && previous.1 != ev.file {
            let name = crate::error::show(ev.file.strip_prefix(&self.base).unwrap_or(&ev.file));
            self.write(format_args!("  {}{dim}{name}{dim:#}\n", indent(ev.depth)));
        }
        self.current = Some((ev.via.clone(), ev.file.clone()));
    }
}

impl Sink for Text {
    fn start(&mut self, name: &str, depth: usize) {
        if !self.spinner {
            return;
        }
        self.live = Some(Spinner::start(
            self.target,
            format!("{}{name}", indent(depth)),
        ));
    }

    fn step(&mut self, ev: &Event) {
        if let Some(s) = self.live.take() {
            s.stop();
        }
        let hidden = self.hide_skipped && matches!(ev.status, Status::Skipped(_));
        if !hidden {
            self.header(ev);
        }
        // Counted before the hidden step leaves: `--hide-skipped` drops the
        // line and the counts still stand (§9.1). A hidden step cannot open a
        // block — nothing of it was shown — so it is counted into the levels
        // already open that its call path still carries, and a block whose
        // every step was hidden was never opened and reports nothing.
        for (i, call) in self.open.iter_mut().enumerate() {
            if ev.via.get(i) != Some(&call.label) {
                break;
            }
            call.summary.count(&ev.status);
            call.summary.duration += ev.duration;
        }
        if hidden {
            return;
        }

        let style = style_for(&ev.status);
        let name = truncate(&format!("{}{}", indent(ev.depth), ev.name), NAME_WIDTH);
        let mut label = ev.status.label();
        if let Some(n) = &ev.note {
            write!(label, "   {n}").expect("writing to a String cannot fail");
        }
        if ev.attempts > 1 {
            write!(label, "   attempt {}/{}", ev.attempt, ev.attempts)
                .expect("writing to a String cannot fail");
        }
        let dim = Style::new().dimmed();
        self.write(format_args!(
            "  {style}{}{style:#} {name:<NAME_WIDTH$} {style}{label}{style:#}  {dim}{}{dim:#}\n",
            ev.status.glyph(),
            human(ev.duration),
        ));

        if let Some(detail) = ev.detail.as_ref().filter(|_| !self.no_diff) {
            self.detail(detail);
        }
        if let Status::Failed(f) = &ev.status {
            self.failure(ev, f);
        } else if self.verbose {
            for line in ev.stdout.lines().chain(ev.stderr.lines()) {
                self.write(format_args!("    {dim}│ {line}{dim:#}\n"));
            }
        }
    }

    /// One line before the first step: how much is about to happen, and
    /// whether any of it is root. Not a diff — see `Manifest`.
    fn manifest(&mut self, plan: &Path, m: &Manifest) {
        // This *is* the root file's header, with the counts on it, so claim
        // the slot rather than printing a second line saying the same name.
        self.current = Some((Vec::new(), plan.to_path_buf()));
        let name = crate::error::show(plan.strip_prefix(&self.base).unwrap_or(plan));
        // "to run", not bare "steps" (#45). This count is taken on the
        // preflight walk, which evaluates `when` and the tag filter and so
        // excludes the steps they rule out -- while the summary below counts
        // every step the walk reached, skipped ones included. Two different
        // sets, and printing both as "N steps" meant one run reporting two
        // step counts. The number stays as it is: `when` is the one gate the
        // preflight can honestly answer up front, and a count that ignored it
        // would overstate the run on every plan with an os-gated step, which
        // is most of them. It is the word that was wrong.
        let mut parts = vec![format!(
            "{} step{} to run",
            m.steps,
            if m.steps == 1 { "" } else { "s" }
        )];
        // One file is the ordinary case and saying so is noise; more than one
        // means the run reaches past the file the operator named.
        if m.files.len() > 1 {
            parts.push(format!("{} files", m.files.len()));
        }
        if m.sudo > 0 {
            parts.push(format!("{} sudo", m.sudo));
        }
        // Dimmed like the plain header it replaces: it is still the file's
        // name, and the step lines under it are what a reader is scanning.
        let dim = Style::new().dimmed();
        self.write(format_args!(
            "  {dim}{name} · {}{dim:#}\n",
            parts.join(" · ")
        ));
    }

    fn summary(&mut self, plan: &Path, s: &Summary) {
        // The run ending is the last way to leave a call site, and the one a
        // walk that reached the end of the file always takes.
        self.close_to(0);
        let name = crate::error::show(plan.strip_prefix(&self.base).unwrap_or(plan));
        let mut parts = vec![format!(
            "{} step{}",
            s.total,
            if s.total == 1 { "" } else { "s" }
        )];
        parts.extend(s.parts());
        parts.push(human(s.duration));
        self.write(format_args!("\n  {name} · {}\n", parts.join(" · ")));
        if s.interrupted {
            let red = Style::new().fg_color(Some(AnsiColor::Red.into()));
            self.write(format_args!("  {red}interrupted{red:#}\n"));
        }
        // Both can be true of one run, and both are printed when they are: the
        // exit code has to pick one (§8) and a reader does not.
        if s.deadline_exceeded {
            let red = Style::new().fg_color(Some(AnsiColor::Red.into()));
            self.write(format_args!("  {red}deadline exceeded{red:#}\n"));
        }
    }
}

impl Text {
    /// Close the open call sites below `keep`, innermost first, printing each
    /// one's summary line. Spec §9.1: the duration is the sum of the steps it
    /// covers, not a wall clock — those are the numbers the lines above it
    /// already showed, and a reader adding them up has to get this answer.
    ///
    /// The counts are kept whatever the verbosity and printed only under
    /// `--verbose`: a block's roll-up is detail, and the run's own summary is
    /// what a quiet run reports.
    fn close_to(&mut self, keep: usize) {
        while self.open.len() > keep {
            let call = self.open.pop().expect("the loop condition is the test");
            if !self.verbose {
                continue;
            }
            let mut parts = vec![format!(
                "{} step{}",
                call.summary.total,
                if call.summary.total == 1 { "" } else { "s" }
            )];
            parts.extend(call.summary.parts());
            parts.push(human(call.summary.duration));
            let dim = Style::new().dimmed();
            self.write(format_args!(
                "  {}{dim}└ {} · {}{dim:#}\n",
                indent(self.open.len()),
                call.label,
                parts.join(" · ")
            ));
        }
    }

    /// A diff, or a metadata delta, under the step's own line. Unified diffs
    /// arrive already marked up; `+`/`-` get their color here so the diff
    /// itself stays plain text everywhere else (json, tests).
    fn detail(&self, text: &str) {
        let dim = Style::new().dimmed();
        let green = Style::new().fg_color(Some(AnsiColor::Green.into()));
        let red = Style::new().fg_color(Some(AnsiColor::Red.into()));
        for line in text.lines() {
            let style = match line.as_bytes().first() {
                Some(b'+') => green,
                Some(b'-') => red,
                _ => dim,
            };
            self.write(format_args!("    {dim}│{dim:#} {style}{line}{style:#}\n"));
        }
    }

    /// Spec §6.1 and §9.1: both captured streams, stdout first, each tailed on
    /// its own. A step that wrote its report to stdout and its verdict to
    /// stderr used to render as the verdict alone — the one stream this picked
    /// — which is the report missing at the only moment it was for.
    ///
    /// `Failure::stderr` is not always a stream. On the paths that never
    /// reached a command — a typed action, an `unless` that could not run, a
    /// program that could not be spawned — it is a synthesized reason and the
    /// event carries no output at all, so it fills the same slot there and
    /// nowhere else. Where a command did run it is a copy of `ev.stderr`, and
    /// reading the event instead keeps one of those two from going stale.
    fn failure(&self, ev: &Event, f: &crate::report::Failure) {
        let dim = Style::new().dimmed();
        let red = Style::new().fg_color(Some(AnsiColor::Red.into()));
        let captured = [("stdout", &ev.stdout), ("stderr", &ev.stderr)];
        let mut streams: Vec<(&str, &String)> = captured
            .into_iter()
            .filter(|(_, s)| !s.trim().is_empty())
            .collect();
        if streams.is_empty() && !f.stderr.trim().is_empty() {
            streams.push(("stderr", &f.stderr));
        }

        // A body that survived the trim has a line with something on it, so a
        // stream in this list always prints and the note can name the list.
        // Only the streams the window actually narrowed are named there: a
        // note claiming a cap bit a stream it let through whole is the kind
        // of half-truth that sends a reader looking for lines that are
        // already on the screen.
        let mut narrowed: Vec<&str> = Vec::new();
        for (name, body) in &streams {
            if self.verbose {
                for line in body.lines() {
                    self.write(format_args!("    {red}│{red:#} {line}\n"));
                }
                continue;
            }
            let w = window(body, HEAD, TAIL);
            for line in w.head {
                self.write(format_args!("    {red}│{red:#} {line}\n"));
            }
            if w.elided > 0 {
                narrowed.push(name);
                // The marker sits between the two ends, in the gutter of the
                // stream it cut, because two streams have two totals and one
                // note cannot honestly carry both.
                self.write(format_args!(
                    "    {red}│{red:#} {dim}… {} of {} lines elided …{dim:#}\n",
                    w.elided, w.total
                ));
            }
            for line in w.tail {
                self.write(format_args!("    {red}│{red:#} {line}\n"));
            }
        }

        let mut note = Vec::new();
        if let Some(rc) = f.rc {
            note.push(format!("exit {rc}"));
        }
        if f.msg != "exit" {
            note.push(f.msg.clone());
        }
        if !narrowed.is_empty() {
            // `each` only when the window bit both, so the one-stream note —
            // which is every failure that only ever had one — reads as a
            // sentence about that stream.
            let each = if narrowed.len() > 1 { " each" } else { "" };
            note.push(format!(
                "{}, first {HEAD} + last {TAIL} lines{each} · --verbose for all",
                narrowed.join("+")
            ));
        }
        if !note.is_empty() {
            self.write(format_args!("    {dim}│ ({}){dim:#}\n", note.join(" · ")));
        }
    }
}

fn style_for(s: &Status) -> Style {
    let c = |c: AnsiColor| Style::new().fg_color(Some(c.into()));
    match s {
        Status::Ok => c(AnsiColor::Green),
        Status::Changed | Status::WouldChange | Status::WouldRun => c(AnsiColor::Yellow),
        Status::Unknown | Status::WouldRunUnprobed => c(AnsiColor::Magenta),
        Status::Skipped(_) => Style::new().dimmed(),
        Status::Failed(_) => c(AnsiColor::Red).bold(),
    }
}

/// Spec §9.1: depth is a display concern; execution is flat. Four levels is
/// where indentation stops earning its width.
fn indent(depth: usize) -> String {
    "  ".repeat(depth.min(4))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max - 1).collect();
    format!("{head}…")
}

// ── the spinner ───────────────────────────────────────────────────────────

/// One live line, erased when the step finishes. `indicatif` does this and a
/// great deal more; this is the part provision needs.
struct Spinner {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
    target: Target,
}

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl Spinner {
    fn start(target: Target, name: String) -> Spinner {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let name = truncate(&name, NAME_WIDTH);
            let mut i = 0usize;
            while !flag.load(Ordering::Relaxed) {
                let frame = FRAMES.get(i % FRAMES.len()).copied().unwrap_or(" ");
                emit(target, &format!("\r  {frame} {name}"));
                i += 1;
                std::thread::sleep(Duration::from_millis(80));
            }
        });
        Spinner {
            stop,
            handle,
            target,
        }
    }

    /// The spinner thread only ever loops on an atomic flag, so a join
    /// error means it panicked -- and the run is already reporting whatever
    /// caused that.
    #[expect(
        clippy::let_underscore_must_use,
        reason = "nothing to do if the spinner thread panicked"
    )]
    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
        // \r then erase-to-end-of-line: the finished line is about to be
        // written over the top of this one.
        emit(self.target, "\r\x1b[2K");
    }
}

#[expect(
    clippy::let_underscore_must_use,
    reason = "a failed write to the terminal has nowhere to be reported: the terminal is where a report would go"
)]
fn emit(target: Target, s: &str) {
    match target {
        Target::Out => {
            let mut w = std::io::stdout();
            let _ = w.write_all(s.as_bytes());
            let _ = w.flush();
        }
        Target::Err => {
            let mut w = std::io::stderr();
            let _ = w.write_all(s.as_bytes());
            let _ = w.flush();
        }
    }
}
