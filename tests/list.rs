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

// Spec §10: `provision list tasks/ | head -3` is ordinary use, and it used to
// exit 101 with a panic and a backtrace — Rust ignores SIGPIPE, so a closed
// pipe arrives as an EPIPE write error and `println!` panics on it.
//
// The pipe buffer is what makes the naive version of this test a coin flip: a
// short listing is written in one go and lands in the buffer before `head`
// ever exits, so nothing fails. This generates a listing far larger than the
// buffer, which makes provision block mid-write with the reader already gone —
// the one arrangement where EPIPE is certain.
//
// `set -o pipefail` is load-bearing too. Without it the pipeline reports
// `head`'s status, which is 0 whatever provision did.
#[test]
fn a_closed_stdout_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    // ~600 bytes of description each, 400 files: comfortably past the 64 KiB
    // a pipe holds on both Linux and macOS.
    let filler = "x".repeat(600);
    for i in 0..400 {
        std::fs::write(
            dir.path().join(format!("task{i:03}.yml")),
            format!("description: {filler}\nsteps: []\n"),
        )
        .unwrap();
    }

    let script = format!(
        "set -o pipefail; '{}' list '{}' | head -3 >/dev/null",
        env!("CARGO_BIN_EXE_provision"),
        dir.path().display()
    );
    let out = Command::new("bash")
        .args(["-c", &script])
        .output()
        .expect("bash failed to start");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !stderr.contains("panicked"),
        "list panicked when its reader went away:\n{stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "the reader asking for less is not an error:\n{stderr}"
    );
}

#[test]
fn listing_a_path_that_is_not_there_is_a_usage_error() {
    let dir = format!("{}/", listing_dir().join("nosuch").display());
    let (code, out) = run(&["list", &dir]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    // "no such directory" until the argument could also be a file. The verb
    // takes both now, so the message names both.
    assert!(out.contains("no such file or directory"), "{out}");
}

// Spec §8: the directory form says what can be run here, the file form says
// what one of them takes. `deploy.yml` carries one prop of each shape.
//
// The assertion that earns this snapshot is `dest`: its default is
// `{{ home }}/out` in the file, and §3.2 says a default is data and is never
// rendered — so the listing has to print the braces. A listing that rendered
// it would show this machine's home directory, which is both wrong and, in a
// snapshot, wrong differently on every machine.
#[test]
fn a_component_file_lists_its_props() {
    let path = listing_dir().join("deploy.yml");
    let (code, out) = run(&["list", path.to_str().expect("fixture paths are UTF-8")]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("{{ home }}/out"),
        "a default was rendered:\n{out}"
    );
    insta::assert_snapshot!("component", out);
}

// An empty column would read as "props it did not bother to name". Saying it
// out loud is the difference between a component with no inputs and a listing
// that failed to find them.
#[test]
fn a_component_with_no_props_says_so() {
    let path = listing_dir().join("undescribed.yml");
    let (code, out) = run(&["list", path.to_str().expect("fixture paths are UTF-8")]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("no props"), "{out}");
}

// Spec §8: a plan declares no props, and `--prop` on one is already a usage
// error. Asking a plan for its interface is that same mistake and gets that
// same answer, rather than an empty listing implying it simply has none.
#[test]
fn listing_a_plan_is_a_usage_error() {
    let path = listing_dir().join("a_plan.yml");
    let (code, out) = run(&["list", path.to_str().expect("fixture paths are UTF-8")]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("is a plan, not a component"), "{out}");
}
