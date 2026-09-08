//! `provision list <dir>/` — spec §8. The task runner's "what can I run
//! here", and the only thing the verb does.
//!
//! Nothing below a file's root keys is parsed and nothing runs, so the
//! listing is the one command that cannot be broken by a broken file.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Spec §8: `3` is "usage or validation error".
const EXIT_VALIDATION: i32 = 3;

/// The listing has its own fixture directory rather than sharing one with the
/// component tests: a snapshot that changes whenever an unrelated fixture is
/// added is a snapshot nobody reads.
fn listing_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/list")
}

fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(args)
        .env("NO_COLOR", "1")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .output()
        .expect("provision failed to start");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

// `deploy` and `deploy-fast` are the pair that pins the sort. Sorting whole
// filenames puts `deploy-fast.yml` first, because `-` sorts below `.`; the
// stem is what the listing prints and what a reader scans for.
//
// `a_plan` is a sequence, not a component, and is named rather than dropped:
// a listing that silently omits a file is worse than one that says which
// file is wrong.
#[test]
fn a_directory_lists_its_components_by_stem() {
    let dir = format!("{}/", listing_dir().display());
    let (code, out) = run(&["list", &dir]);
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("listing", out);
}

// The trailing separator is how §8 writes it and how `run <dir>/` used to
// require it. The verb says what the argument is now, so both forms work and
// both give the same bytes.
#[test]
fn the_trailing_separator_is_optional_and_changes_nothing() {
    let bare = listing_dir().display().to_string();
    let (bare_code, bare_out) = run(&["list", &bare]);
    let (slash_code, slash_out) = run(&["list", &format!("{bare}/")]);
    assert_eq!(bare_code, 0, "{bare_out}");
    assert_eq!(slash_code, 0, "{slash_out}");
    assert_eq!(bare_out, slash_out);
}

#[test]
fn listing_a_directory_that_is_not_there_is_a_usage_error() {
    let dir = format!("{}/", listing_dir().join("nosuch").display());
    let (code, out) = run(&["list", &dir]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("no such directory"), "{out}");
}
