//! One diagnostic type. Every error the user sees carries `file:line:col`.

use std::fmt;
use std::path::{Path, PathBuf};

/// A single problem, anchored at a position in a plan file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Diag {
    pub file: PathBuf,
    pub line: usize,
    pub col: usize,
    pub msg: String,
    /// A second line, indented under the message. Use it for the fix.
    pub note: Option<String>,
}

impl Diag {
    pub(crate) fn new(
        file: impl Into<PathBuf>,
        line: usize,
        col: usize,
        msg: impl Into<String>,
    ) -> Self {
        Diag {
            file: file.into(),
            line,
            col,
            msg: msg.into(),
            note: None,
        }
    }

    /// A diagnostic about a whole file rather than a position inside it.
    pub(crate) fn file_level(file: impl Into<PathBuf>, msg: impl Into<String>) -> Self {
        Diag {
            file: file.into(),
            line: 0,
            col: 0,
            msg: msg.into(),
            note: None,
        }
    }

    pub(crate) fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Render the path relative to `base` when it is underneath it, so output
    /// reads `components/zsh/index.yml:12:3`, not an absolute path.
    pub(crate) fn display_from(&self, base: &Path) -> String {
        let path = self.file.strip_prefix(base).unwrap_or(&self.file).display();
        let mut out = if self.line == 0 {
            format!("{path}: {}", self.msg)
        } else {
            format!("{path}:{}:{}: {}", self.line, self.col, self.msg)
        };
        if let Some(note) = &self.note {
            out.push_str("\n      ");
            out.push_str(note);
        }
        out
    }
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_from(Path::new("")))
    }
}

impl std::error::Error for Diag {}

pub(crate) type Result<T> = std::result::Result<T, Diag>;

/// Collects diagnostics so `validate` can report every problem in one pass
/// instead of one per run.
#[derive(Debug, Default)]
pub(crate) struct Diags(Vec<Diag>);

impl Diags {
    pub(crate) fn new() -> Self {
        Diags(Vec::new())
    }

    /// Spec §9.3: one fault, one diagnostic. A fault *is* its file, line,
    /// column and message, so the same four arriving twice is the walk
    /// passing over one line twice, not two problems — a step's `name` is
    /// rendered once by the field check and again for the step's own line,
    /// and an unknown filter in it used to be reported twice and counted as
    /// `2 problems`. Dropping the duplicate here rather than at the report
    /// keeps `len()` and the fatality checks reading the same number the
    /// operator does.
    pub(crate) fn push(&mut self, mut d: Diag) {
        // Attached here rather than at the dozen places a render error turns
        // into a diagnostic: the hint is a property of the message, and one
        // choke point cannot miss a path. A diagnostic that already carries
        // a note keeps it — the note the author wrote is the better one.
        // Before the duplicate check, not after: the note is part of what
        // makes two diagnostics equal, so attaching it second would make the
        // second copy of one fault look like a different one.
        if d.note.is_none()
            && let Some(note) = crate::template::vocabulary_note(&d.msg)
        {
            d.note = Some(note);
        }
        if self.0.contains(&d) {
            return;
        }
        self.0.push(d);
    }

    /// Record the error and keep going. Returns None so callers can `?`-like
    /// bail out of one branch without aborting the whole walk.
    pub(crate) fn absorb<T>(&mut self, r: Result<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(d) => {
                self.push(d);
                None
            }
        }
    }

    /// Give each note-less diagnostic a second line, when the caller can
    /// say something about it that was not knowable when it was pushed.
    pub(crate) fn annotate(&mut self, f: impl Fn(&Diag) -> Option<String>) {
        for d in &mut self.0 {
            if d.note.is_none()
                && let Some(note) = f(d)
            {
                d.note = Some(note);
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Diag> {
        self.0.iter()
    }
}
