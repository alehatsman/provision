//! `provision run` — D17. A component executed as a task, a directory
//! listed, and one step from a string.
//!
//! The `--step` form is moongit's exec contract, so its exit code and its
//! message are asserted, not just that it worked.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Spec §8: `3` is "usage or validation error".
const EXIT_VALIDATION: i32 = 3;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run").join(name)
}

fn provision(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(args)
        .env("NO_COLOR", "1")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .output()
        .expect("provision failed to start")
}

fn run(args: &[&str]) -> (i32, String) {
    let out = provision(args);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr),
    )
}

/// `--verbose` throughout: the props are asserted through what the step
/// echoed, and a captured step prints nothing without it.
fn task(extra: &[&str]) -> (i32, String) {
    let path = fixture("task.yml");
    let mut args = vec!["run", path.to_str().unwrap(), "--verbose"];
    args.extend_from_slice(extra);
    run(&args)
}

// ── props ─────────────────────────────────────────────────────────────────

#[test]
fn a_prop_takes_its_default_and_an_override_wins() {
    let (code, out) = task(&["--prop", "target=/tmp/x"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("/tmp/x dev 1"), "defaults did not reach the step:\n{out}");

    let (code, out) = task(&["--prop", "target=/tmp/x", "--prop", "label=prod"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("/tmp/x prod 1"), "the override did not win:\n{out}");
}

#[test]
fn a_missing_required_prop_is_an_error_not_a_placeholder() {
    let (code, out) = task(&[]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("missing required prop `target`"), "{out}");
    // The declaration is where the reader has to go to fix it.
    assert!(out.contains("task.yml:4"), "the note should point at the prop:\n{out}");
}

#[test]
fn an_unknown_prop_names_the_ones_that_exist() {
    let (code, out) = task(&["--prop", "target=/tmp/x", "--prop", "nope=1"]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("has no prop `nope`"), "{out}");
    assert!(out.contains("it declares: target, label, count"), "{out}");
}

#[test]
fn a_prop_of_the_wrong_type_says_how_to_pass_the_right_one() {
    // Spec §3.4 reaches the command line: a bare value is a string, and one
    // expression keeps its type. `count` is declared `int`.
    let (code, out) = task(&["--prop", "target=/tmp/x", "--prop", "count=3"]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("prop `count` is declared int but got"), "{out}");
    assert!(out.contains("--prop n='{{ 3 }}'"), "the note should show the fix:\n{out}");

    let (code, out) = task(&["--prop", "target=/tmp/x", "--prop", "count={{ 3 }}"]);
    assert_eq!(code, 0, "one expression should keep its type:\n{out}");
    assert!(out.contains("/tmp/x dev 3"), "{out}");
}

#[test]
fn validate_checks_a_component_and_its_props_without_running_it() {
    let path = fixture("task.yml");
    let (code, out) = run(&["validate", path.to_str().unwrap(), "--prop", "target=/tmp/x"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("  ok  "), "{out}");

    // Same three checks as `run`. No placeholder stands in for a required
    // prop just because nothing is going to execute.
    let (code, out) = run(&["validate", path.to_str().unwrap()]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("missing required prop `target`"), "{out}");
}

// ── the one verdict that moves ────────────────────────────────────────────

// Spec §6.1, D17. This is the whole behavioural difference between `run` and
// `apply`, so it is asserted from both sides of the same file.
#[test]
fn an_ungated_step_is_ok_under_run_and_unknown_under_apply() {
    let path = fixture("plain.yml");
    let (code, out) = run(&["run", path.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(" ok "), "an ungated step should be ok under run:\n{out}");
    assert!(!out.contains("unknown"), "{out}");

    // `apply` cannot read the component form, so the same step is applied
    // from the plan that holds only it.
    let plain = fixture("not_a_component.yml");
    let (code, out) = run(&["apply", plain.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("unknown"), "apply must be unchanged:\n{out}");
}

// ── listing ───────────────────────────────────────────────────────────────

#[test]
fn a_trailing_slash_lists_the_directory() {
    let dir = format!("{}/", fixture("").display());
    let (code, out) = run(&["run", &dir]);
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("listing", out);
}

#[test]
fn listing_a_directory_that_is_not_there_is_a_usage_error() {
    let dir = format!("{}/", fixture("nosuch").display());
    let (code, out) = run(&["run", &dir]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("no such directory"), "{out}");
}

// ── --step: the CI runner's contract ──────────────────────────────────────

#[test]
fn one_step_from_a_string_runs_and_reports() {
    let (code, out) = run(&["run", "--step", "name: from a string\nshell: echo hi"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("from a string"), "{out}");
    // Ungated and exited 0, so `ok` — the same rule as a component's steps.
    assert!(out.contains(" ok "), "{out}");
}

#[test]
fn a_failing_step_from_a_string_exits_1() {
    let (code, out) = run(&["run", "--step", "shell: exit 7"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("FAILED"), "{out}");
    assert!(out.contains("exit 7"), "{out}");
}

#[test]
fn a_list_is_not_one_step() {
    // clap would take the leading `- ` for a flag and exit 2, which `run`
    // never does; `allow_hyphen_values` is what lets this message happen.
    let (code, out) = run(&["run", "--step", "- shell: \"true\""]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("--step takes one step, not a list"), "{out}");
}

#[test]
fn a_structural_key_is_not_one_step() {
    for key in ["use: ./x.yml", "import: ./x.yml", "vars_file: ./x.yml"] {
        let (code, out) = run(&["run", "--step", key]);
        assert_eq!(code, EXIT_VALIDATION, "{key}:\n{out}");
        assert!(out.contains("cannot be a --step"), "{key}:\n{out}");
    }
}

#[test]
fn a_step_from_a_string_carries_its_own_position() {
    let (code, out) = run(&["run", "--step", "name: x\nshell: y\nbogus: 1"]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("<step>:"), "the diagnostic should be located:\n{out}");
}
