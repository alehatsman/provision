//! The human renderer. Spec §9.1 and §9.2 describe the same lines twice —
//! once with a spinner and color, once without — so this is one renderer with
//! two switches rather than two that drift apart.

use super::Sink;
use super::event::{Event, Status, Summary, human, tail};
use anstyle::{AnsiColor, Style};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How many stderr lines the failure block shows. Spec §9.1 says 20.
const TAIL: usize = 20;
const NAME_WIDTH: usize = 48;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Out,
    Err,
}

pub struct Text {
    target: Target,
    /// Animate a live line while a step runs. Off when output is redirected,
    /// and off under `--stream`, where the child is writing to the same
    /// terminal and the two would garble each other.
    spinner: bool,
    verbose: bool,
    hide_skipped: bool,
    /// Spec §9.1. The flag exists on `apply` too, where there is no diff to
    /// suppress, so a script can pass both commands the same arguments.
    no_diff: bool,
    base: PathBuf,
    /// The file whose header was printed last, so nested `import`/`use` shows
    /// as one dim header rather than a path on every line.
    current: Option<PathBuf>,
    live: Option<Spinner>,
}

impl Text {
    pub fn new(target: Target, base: PathBuf) -> Text {
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
            live: None,
        }
    }

    pub fn verbose(mut self, on: bool) -> Text {
        self.verbose = on;
        self
    }

    pub fn hide_skipped(mut self, on: bool) -> Text {
        self.hide_skipped = on;
        self
    }

    pub fn no_diff(mut self, on: bool) -> Text {
        self.no_diff = on;
        self
    }

    /// `--stream` hands the terminal to the child, so the spinner stands down.
    pub fn no_spinner(mut self) -> Text {
        self.spinner = false;
        self
    }

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

    fn header(&mut self, file: &Path, depth: usize) {
        if self.current.as_deref() == Some(file) {
            return;
        }
        self.current = Some(file.to_path_buf());
        let name = file.strip_prefix(&self.base).unwrap_or(file).display();
        let dim = Style::new().dimmed();
        self.write(format_args!("  {}{dim}{name}{dim:#}\n", indent(depth)));
    }
}

impl Sink for Text {
    fn start(&mut self, name: &str, depth: usize) {
        if !self.spinner {
            return;
        }
        self.live = Some(Spinner::start(self.target, format!("{}{name}", indent(depth))));
    }

    fn step(&mut self, ev: &Event) {
        if let Some(s) = self.live.take() {
            s.stop();
        }
        if self.hide_skipped && matches!(ev.status, Status::Skipped(_)) {
            return;
        }
        self.header(&ev.file, ev.depth);

        let style = style_for(&ev.status);
        let name = truncate(&format!("{}{}", indent(ev.depth), ev.name), NAME_WIDTH);
        let mut label = ev.status.label();
        if ev.attempts > 1 {
            label.push_str(&format!("   attempt {}/{}", ev.attempt, ev.attempts));
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

    fn summary(&mut self, plan: &Path, s: &Summary) {
        let name = plan.strip_prefix(&self.base).unwrap_or(plan).display();
        let mut parts = vec![format!("{} step{}", s.total, if s.total == 1 { "" } else { "s" })];
        parts.extend(s.parts());
        parts.push(human(s.duration));
        self.write(format_args!("\n  {name} · {}\n", parts.join(" · ")));
        if s.interrupted {
            let red = Style::new().fg_color(Some(AnsiColor::Red.into()));
            self.write(format_args!("  {red}interrupted{red:#}\n"));
        }
    }
}

impl Text {
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

    fn failure(&self, ev: &Event, f: &super::event::Failure) {
        let dim = Style::new().dimmed();
        let red = Style::new().fg_color(Some(AnsiColor::Red.into()));
        let body = if f.stderr.trim().is_empty() { &ev.stdout } else { &f.stderr };
        let lines = if self.verbose {
            body.lines().collect::<Vec<_>>()
        } else {
            tail(body, TAIL)
        };
        for line in &lines {
            self.write(format_args!("    {red}│{red:#} {line}\n"));
        }
        let mut note = Vec::new();
        if let Some(rc) = f.rc {
            note.push(format!("exit {rc}"));
        }
        if f.msg != "exit" {
            note.push(f.msg.clone());
        }
        if !lines.is_empty() && !self.verbose {
            note.push(format!("stderr, last {TAIL} lines · --verbose for all"));
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
                emit(target, &format!("\r  {} {name}", FRAMES[i % FRAMES.len()]));
                i += 1;
                std::thread::sleep(Duration::from_millis(80));
            }
        });
        Spinner { stop, handle, target }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
        // \r then erase-to-end-of-line: the finished line is about to be
        // written over the top of this one.
        emit(self.target, "\r\x1b[2K");
    }
}

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
