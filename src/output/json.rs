//! `--json`: one object per line on stdout. Spec §9.3.
//!
//! The shape is a contract, so the words come from `Status::key`, which is
//! stable, and never from the terminal wording, which is not.

use super::Sink;
use super::event::{Event, Summary};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) struct Json {
    base: PathBuf,
}

impl Json {
    pub(crate) fn new(base: PathBuf) -> Json {
        Json { base }
    }

    #[expect(
        clippy::let_underscore_must_use,
        reason = "a failed write to the terminal has nowhere to be reported: the terminal is where a report would go"
    )]
    fn emit(v: &serde_json::Value) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.base)
            .unwrap_or(p)
            .display()
            .to_string()
    }
}

impl Sink for Json {
    fn step(&mut self, ev: &Event) {
        let mut o = json!({
            "event": "step",
            "index": ev.index,
            "name": ev.name,
            "status": ev.status.key(),
            "duration_ms": ev.duration.as_millis(),
            "file": self.rel(&ev.file),
            "line": ev.line,
        });
        let m = o.as_object_mut().expect("built as an object");
        // Spec §9.3: `rc`, `stdout` and `stderr` are the step's own command's,
        // present exactly when the step ran one, whatever its status, in full.
        // That is the whole of what a CI runner reads per step, so it is not
        // reserved for failures the way it used to be. A typed action ran no
        // command and a skipped step ran none, so neither carries them, and a
        // gate's result is never reported: it decides whether the step runs
        // and is not the step's own result.
        if let Some(rc) = ev.rc {
            m.insert("rc".into(), json!(rc));
            m.insert("stdout".into(), json!(ev.stdout));
            m.insert("stderr".into(), json!(ev.stderr));
        }
        if let super::event::Status::Skipped(why) = &ev.status {
            m.insert("reason".into(), json!(why));
        }
        // A failure's `rc` and `stderr` are written after the pair above, not
        // instead of them: they are the same values for a command that
        // exited, and for one that never did — a timeout, an interrupt — the
        // failure's `rc: null` is the honest answer and the command's is not
        // there to give. A typed action's failure has only these.
        if let super::event::Status::Failed(f) = &ev.status {
            m.insert("message".into(), json!(f.msg));
            m.insert("rc".into(), json!(f.rc));
            m.insert("stderr".into(), json!(f.stderr));
            if f.interrupted {
                m.insert("interrupted".into(), json!(true));
            }
        }
        if let Some(n) = &ev.note {
            m.insert("note".into(), json!(n));
        }
        if let Some(d) = &ev.detail {
            m.insert("diff".into(), json!(d));
        }
        if ev.attempts > 1 {
            m.insert("attempt".into(), json!(ev.attempt));
            m.insert("attempts".into(), json!(ev.attempts));
        }
        Self::emit(&o);
    }

    fn summary(&mut self, plan: &Path, s: &Summary) {
        Self::emit(&json!({
            "event": "summary",
            "plan": self.rel(plan),
            "total": s.total,
            "ok": s.ok,
            "changed": s.changed,
            "skipped": s.skipped,
            "unknown": s.unknown,
            "failed": s.failed,
            // The step lines have always carried `would_change`; the summary
            // did not, so a consumer counting from the summary alone read a
            // plan with pending file changes as having nothing to do.
            "would_change": s.would_change,
            "would_run": s.would_run,
            "would_run_unprobed": s.unprobed,
            "interrupted": s.interrupted,
            "duration_ms": s.duration.as_millis(),
        }));
    }
}
