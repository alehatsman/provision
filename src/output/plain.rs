//! Plain, non-TTY output. Spec §9.2. Phase 0 ships this only; the TTY
//! renderer with its spinner arrives with execution in phase 1.

use crate::error::Diags;
use crate::expand::{Flat, Status};
use std::io::Write;
use std::path::Path;

/// Glyph and label per spec §9.1, in their non-colored form.
fn glyph_and_label(s: &Status) -> (&'static str, String) {
    match s {
        Status::Skipped(why) => ("-", format!("skipped   {why}")),
        Status::WouldRunUnprobed => ("?", "would run (unprobed)".to_string()),
    }
}

pub fn steps(
    out: &mut impl Write,
    steps: &[Flat],
    hide_skipped: bool,
    base: &Path,
) -> std::io::Result<()> {
    // Spec §9.1: a nested import/use is shown as a header line naming the
    // file, and its steps are indented one level. Execution stays flat.
    let mut current: Option<&Path> = None;
    for s in steps {
        if hide_skipped && matches!(s.status, Status::Skipped(_)) {
            continue;
        }
        if current != Some(s.file.as_path()) {
            current = Some(&s.file);
            let name = s.file.strip_prefix(base).unwrap_or(&s.file).display();
            writeln!(out, "  {}{name}", "  ".repeat(s.depth.min(4)))?;
        }
        let (glyph, label) = glyph_and_label(&s.status);
        let indent = "  ".repeat(s.depth.min(4));
        let name = format!("{indent}{}", s.name);
        writeln!(out, "  {glyph} {:<48} {label}", truncate(&name, 48))?;
    }
    Ok(())
}

pub fn summary(out: &mut impl Write, plan: &Path, steps: &[Flat]) -> std::io::Result<()> {
    let total = steps.len();
    let skipped = steps.iter().filter(|s| matches!(s.status, Status::Skipped(_))).count();
    let unprobed = total - skipped;
    writeln!(out)?;
    writeln!(
        out,
        "  {} · {total} steps · {unprobed} would run · {skipped} skipped",
        plan.display()
    )?;
    Ok(())
}

pub fn diagnostics(out: &mut impl Write, diags: &Diags, base: &Path) -> std::io::Result<()> {
    for d in diags.iter() {
        writeln!(out, "  error: {}", d.display_from(base))?;
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max - 1).collect();
    format!("{head}…")
}
