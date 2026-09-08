//! A component as the root of `validate`, `plan` and `apply` — D17 as
//! amended by phase 5b.
//!
//! The point of the phase is that the same file means the same thing under
//! every verb, so where a fact is verb-independent this file asserts it under
//! each of them rather than picking one.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Spec §8: `3` is "usage or validation error".
const EXIT_VALIDATION: i32 = 3;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/component")
        .join(name)
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
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

/// A component as `apply`'s root (spec §8, D17). `--verbose` throughout: the
/// props are asserted through what the step echoed, and a captured step
/// prints nothing without it.
fn task(extra: &[&str]) -> (i32, String) {
    let path = fixture("task.yml");
    let mut args = vec![
        "apply",
        path.to_str().expect("fixture paths are UTF-8"),
        "--verbose",
    ];
    args.extend_from_slice(extra);
    run(&args)
}

// ── props ─────────────────────────────────────────────────────────────────

#[test]
fn a_prop_takes_its_default_and_an_override_wins() {
    let (code, out) = task(&["--prop", "target=/tmp/x"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("/tmp/x dev 1 False 0"),
        "defaults did not reach the step:\n{out}"
    );

    let (code, out) = task(&["--prop", "target=/tmp/x", "--prop", "label=prod"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("/tmp/x prod 1 False 0"),
        "the override did not win:\n{out}"
    );
}

#[test]
fn a_missing_required_prop_is_an_error_not_a_placeholder() {
    let (code, out) = task(&[]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("missing required prop `target`"), "{out}");
    // The declaration is where the reader has to go to fix it.
    assert!(
        out.contains("task.yml:4"),
        "the note should point at the prop:\n{out}"
    );
}

#[test]
fn an_unknown_prop_names_the_ones_that_exist() {
    let (code, out) = task(&["--prop", "target=/tmp/x", "--prop", "nope=1"]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("has no prop `nope`"), "{out}");
    assert!(
        out.contains("it declares: target, label, count, flag, items"),
        "{out}"
    );
}

// Spec §8: a `--prop` value is rendered, then read as the prop's *declared*
// type. A shell cannot type a value, so the declaration is the only place a
// type can come from — which is why this is not §3.4's sole-expression rule.
#[test]
fn a_prop_is_read_as_the_type_its_declaration_gives_it() {
    // `True` rather than `true`: minijinja renders booleans Python-style,
    // the same quirk `provision facts` works around.
    let (code, out) = task(&[
        "--prop",
        "target=/tmp/x",
        "--prop",
        "count=3",
        "--prop",
        "flag=true",
        "--prop",
        "items=[a, b]",
    ]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("/tmp/x dev 3 True 2"), "{out}");

    // A `string` prop given something that looks like a number stays text.
    let (code, out) = task(&["--prop", "target=3"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("3 dev 1 False 0"),
        "a string prop should stay a string:\n{out}"
    );

    // The template runs first, so a var can carry the value in.
    let (code, out) = task(&[
        "--prop",
        "target=/tmp/x",
        "--prop",
        "count={{ n }}",
        "--var",
        "n=7",
    ]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("/tmp/x dev 7"), "{out}");
}

#[test]
fn a_prop_that_does_not_read_as_its_type_names_what_was_expected() {
    for (arg, ty, takes) in [
        ("count=abc", "int", "a whole number, like 3"),
        ("flag=maybe", "bool", "true or false"),
        ("items=a, b", "list", "a flow sequence, like [a, b]"),
    ] {
        let (code, out) = task(&["--prop", "target=/tmp/x", "--prop", arg]);
        assert_eq!(code, EXIT_VALIDATION, "{arg}:\n{out}");
        assert!(
            out.contains(&format!("is declared {ty}, and")),
            "{arg}:\n{out}"
        );
        assert!(
            out.contains(takes),
            "the note should say what it takes:\n{out}"
        );
        // The note is one line; a wrapped literal used to smear it across
        // thirty spaces of indentation.
        assert!(!out.contains("  takes"), "the note wrapped:\n{out}");
    }
}

// ── the root file is a plan or a component, under every verb ──────────────

// Spec §8: the root's own shape says which it is, so no verb needs a flag.
// The three that walk a root each get the same component, because "the same
// file means the same thing under every verb" is the whole of phase 5b.
#[test]
fn validate_checks_a_component_and_its_props_without_running_it() {
    let path = fixture("task.yml");
    let (code, out) = run(&[
        "validate",
        path.to_str().unwrap(),
        "--prop",
        "target=/tmp/x",
    ]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("  ok  "), "{out}");
}

// All three prop errors, at validate time. No placeholder stands in for a
// required prop just because nothing is going to execute.
#[test]
fn validate_reports_every_prop_error_a_use_site_would_get() {
    let path = fixture("task.yml");
    let p = path.to_str().unwrap();

    let (code, out) = run(&["validate", p]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("missing required prop `target`"), "{out}");

    let (code, out) = run(&["validate", p, "--prop", "target=/tmp/x", "--prop", "nope=1"]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("has no prop `nope`"), "{out}");

    let (code, out) = run(&[
        "validate",
        p,
        "--prop",
        "target=/tmp/x",
        "--prop",
        "count=abc",
    ]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("is declared int, and"), "{out}");
}

// `plan tasks/ci.yml` previews a task's gates. The gated step is probed and
// reports a plan verdict; the ungated one is `unknown` here exactly as it is
// under `apply`, which is why the exit code is 2.
#[test]
fn plan_previews_a_component_the_same_way_it_previews_a_plan() {
    let path = fixture("task.yml");
    let (code, out) = run(&["plan", path.to_str().unwrap(), "--prop", "target=/tmp/x"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("Echo the props"), "{out}");
    assert!(out.contains("A gated step"), "{out}");
}

// A plan has no props to set, so a `--prop` aimed at one is a usage error
// rather than a value quietly dropped.
#[test]
fn a_prop_on_a_plan_is_a_usage_error() {
    let path = fixture("not_a_component.yml");
    for verb in ["validate", "plan", "apply"] {
        let (code, out) = run(&[verb, path.to_str().unwrap(), "--prop", "target=/tmp/x"]);
        assert_eq!(code, EXIT_VALIDATION, "{verb}:\n{out}");
        assert!(
            out.contains("this file is a plan, not a component"),
            "{verb}:\n{out}"
        );
    }
}

// The listing is its own verb now, and its own test file: `tests/list.rs`.
#[test]
fn a_directory_is_not_something_to_run() {
    let dir = format!("{}/", fixture("nested").display());
    let (code, out) = run(&["apply", &dir]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("provision list <dir>/"), "{out}");
}

// ── the verdict that no longer moves ──────────────────────────────────────

// Spec §6.1, D17 as amended. Phase 5 made an ungated step `ok` under `run`
// and `unknown` under `apply`; 5b withdrew that, because a verb is not a
// declaration. The same component now reads `unknown` wherever it is entered,
// and a step whose exit code is its whole contract says so itself.
#[test]
fn an_ungated_step_is_unknown_under_every_verb() {
    let path = fixture("plain.yml");
    let p = path.to_str().unwrap();

    let (code, out) = run(&["apply", p]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("unknown"),
        "an ungated step is unknown under apply:\n{out}"
    );

    // §8: plan exits 2 on an `unknown`, because it is provision saying it
    // does not know, which is not the same as nothing to do (D15).
    let (code, out) = run(&["plan", p]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("unknown"), "{out}");

    // The declaration, not the verb, is what makes it `ok`.
    let (code, out) = run(&["apply", fixture("declared.yml").to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(" ok "), "{out}");
    assert!(!out.contains("unknown"), "{out}");
}

// ── component_dir and the one cwd rule ────────────────────────────────────

// Spec §3.2 and §4, the two things a shared quality gate needs, and the two
// that are easiest to confuse. `inner.yml` lives one directory down and is
// `use`d from above, so the component's own directory and the invocation
// directory are different answers and a test that mixed them up would say so.
//
// Under `apply`, because these two answers are only observable from a step
// that actually ran. Before 5b this held under `run` and not under `apply`;
// now there is one rule and one verb that executes it.
#[test]
fn a_used_component_knows_its_own_directory_and_runs_in_the_invocation_one() {
    let path = fixture("outer.yml");
    let root = env!("CARGO_MANIFEST_DIR");

    let (code, out) = run(&["apply", path.to_str().unwrap(), "--verbose"]);
    assert_eq!(code, 0, "{out}");

    // §3.2: absolute, and the component's own directory — not the caller's,
    // and not where provision was invoked.
    assert!(
        out.contains(&format!("dir={root}/tests/fixtures/component/nested")),
        "component_dir should be the component's own directory:\n{out}"
    );
    // §4: the invocation directory, even for a step inside a `use`. A shared
    // gate checked out under ~/.cache must gate the repo you are standing in,
    // not `cd` to its own toplevel and gate itself.
    assert!(
        out.contains(&format!("cwd={root}\n")),
        "a step should default to the invocation directory:\n{out}"
    );
}
