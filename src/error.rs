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

    pub(crate) fn push(&mut self, d: Diag) {
        self.0.push(d);
    }

    /// Record the error and keep going. Returns None so callers can `?`-like
    /// bail out of one branch without aborting the whole walk.
    pub(crate) fn absorb<T>(&mut self, r: Result<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(d) => {
                self.0.push(d);
                None
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
