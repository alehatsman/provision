//! End-to-end tests for the phase 0 commands.
//!
//! The Phase 0 gate is "100% of validation errors carry <file:line>", so every
//! error assertion here checks the position, not only the message.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Spec §8: `3` is "usage or validation error".
const EXIT_VALIDATION: i32 = 3;

macro_rules! snapshot {
    ($name:expr, $text:expr) => {
        insta::assert_snapshot!($name, $text)
    };
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(args)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .output()
        .expect("provision failed to start")
}

fn validate(fixture: &str) -> (i32, String) {
    let path = fixtures().join(fixture);
    let out = run(&["validate", path.to_str().expect("fixture paths are UTF-8")]);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

fn plan(args: &[&str]) -> (i32, String) {
    let mut all = vec!["plan", "--plan-no-probe"];
    all.extend_from_slice(args);
    let out = run(&all);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn fixture_arg(name: &str) -> String {
    fixtures().join(name).display().to_string()
}

/// Every error must name a file, a line and a column.
fn assert_positioned(out: &str) {
    let errors: Vec<&str> = out
        .lines()
        .filter(|l| l.trim_start().starts_with("error:"))
        .collect();
    assert!(!errors.is_empty(), "expected at least one error in:\n{out}");
    #[expect(clippy::panic, reason = "a test's assertion failure")]
    for e in errors {
        let Some((_, after)) = e.split_once("error:") else {
            panic!("not an error line: {e}")
        };
        let after = after.trim();
        let (loc, _) = after.split_once(' ').unwrap_or((after, ""));
        let parts: Vec<&str> = loc.trim_end_matches(':').rsplitn(3, ':').collect();
        // rsplitn yields col, line, file — so a well-formed position is
        // exactly three parts with the first two numeric.
        let located = matches!(
            parts.as_slice(),
            [col, line, _file] if col.parse::<usize>().is_ok() && line.parse::<usize>().is_ok()
        );
        assert!(located, "error has no file:line:col — {e}");
    }
}

// ── schema ────────────────────────────────────────────────────────────────

#[test]
fn two_action_keys_is_an_error_at_the_second_one() {
    let (code, out) = validate("two_actions.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("2 action keys"), "{out}");
    assert!(out.contains("two_actions.yml:3:3"), "{out}");
    assert_positioned(&out);
}

#[test]
fn an_unknown_key_is_rejected_and_a_typo_is_suggested() {
    let (code, out) = validate("unknown_key.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("unknown key `creats`"), "{out}");
    assert!(out.contains("did you mean `creates`?"), "{out}");
    assert!(out.contains("unknown_key.yml:3:3"), "{out}");
}

#[test]
fn an_unknown_field_in_an_action_body_is_an_error() {
    // Step keys were checked from the start; action bodies were not, so
    // `service: {daemon_reload: true}` and `shell: {cmd: ...}` passed
    // validation silently. Both are mooncake spellings a migration hits.
    let (code, out) = validate("unknown_action_field.yml");
    assert_eq!(code, EXIT_VALIDATION);
    assert!(
        out.contains("unknown key `daemon_reload` in `service`"),
        "{out}"
    );
    assert!(out.contains("name, state, enabled, scope"), "{out}");
    assert_positioned(&out);
}

#[test]
fn validate_rejects_what_plan_would_reject() {
    // Spec §8: validate is the subset of plan that runs nothing, not a weaker
    // check. Action bodies used to be parsed only on the way to running them,
    // so these three passed validation and failed on the next command — which
    // happened in this repo's own example and in the fleet on the day `pkg`
    // landed.
    let (code, out) = validate("bad_action_bodies.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("`pkg` requires `name` or `names`"), "{out}");
    assert!(
        out.contains("`file` state file needs `content` or `src`"),
        "{out}"
    );
    assert!(
        out.contains("`service` needs `state` or `enabled`"),
        "{out}"
    );
    assert_positioned(&out);
}

#[test]
fn snapshot_three_problems_across_two_files() {
    // Spec §8: validate collects every problem in one pass rather than
    // stopping at the first, across an import too. The trailer counts them
    // together and names the file given on the command line — and says
    // "validating" rather than "in", because two of these three problems
    // are in the imported file, not in the one named.
    let (code, out) = validate("three_problems_a.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(
        out.contains("three_problems_a.yml") && out.contains("three_problems_b.yml"),
        "{out}"
    );
    assert!(out.contains("3 problems validating"), "{out}");
    assert_positioned(&out);
    snapshot!("three_problems", out);
}

#[test]
fn a_step_with_no_action_is_an_error() {
    let (code, out) = validate("no_action.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("no action"), "{out}");
    assert_positioned(&out);
}

#[test]
fn a_bad_duration_names_the_syntax() {
    let (code, out) = validate("bad_duration.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("is not a duration"), "{out}");
    assert!(out.contains("30s, 5m, 1h"), "{out}");
    assert_positioned(&out);
}

#[test]
fn a_modifier_on_the_wrong_step_is_an_error() {
    let (code, out) = validate("misplaced_modifier.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("`props` is only valid on `use`"), "{out}");
    assert_positioned(&out);
}

// ── composition ───────────────────────────────────────────────────────────

#[test]
fn an_import_cycle_is_named() {
    let (code, out) = validate("cycle_a.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("import cycle"), "{out}");
    assert!(
        out.contains("cycle_a.yml") && out.contains("cycle_b.yml"),
        "{out}"
    );
    assert_positioned(&out);
}

#[test]
fn a_missing_import_says_where_it_looked() {
    let (code, out) = validate("missing_import.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("import not found"), "{out}");
    assert!(
        out.contains("relative to the file that names them"),
        "{out}"
    );
    assert_positioned(&out);
}

#[test]
fn a_child_scope_does_not_leak_back_to_its_parent() {
    let (code, out) = validate("scope_isolation.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    // The component sees `from_parent`; the parent must not see `inside_only`.
    assert!(out.contains("undefined variable `inside_only`"), "{out}");
    assert!(
        !out.contains("from_parent"),
        "the child could not read the parent: {out}"
    );
    assert_positioned(&out);
}

// ── props ─────────────────────────────────────────────────────────────────

#[test]
fn an_unknown_prop_lists_what_the_component_declares() {
    let (code, out) = validate("props_unknown.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("has no prop `nope`"), "{out}");
    assert!(out.contains("it declares: who, count"), "{out}");
    assert_positioned(&out);
}

#[test]
fn a_missing_required_prop_points_at_its_declaration() {
    let (code, out) = validate("props_missing.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("missing required prop `who`"), "{out}");
    assert!(out.contains("declared at"), "{out}");
    assert_positioned(&out);
}

#[test]
fn a_prop_of_the_wrong_type_is_rejected_after_rendering() {
    let (code, out) = validate("props_wrong_type.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(
        out.contains("prop `count` is declared int but got the string `two`"),
        "{out}"
    );
    assert_positioned(&out);
}

// ── templating ────────────────────────────────────────────────────────────

#[test]
fn an_undefined_variable_is_named_with_its_position() {
    let (code, out) = validate("undefined_var.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("undefined variable `palette`"), "{out}");
    assert!(out.contains("undefined_var.yml:2:10"), "{out}");
}

#[test]
fn a_non_boolean_condition_is_an_error_not_truthiness() {
    let (code, out) = validate("non_boolean_when.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("not true or false"), "{out}");
    assert_positioned(&out);
}

#[test]
fn a_field_that_is_one_expression_keeps_its_type() {
    let (code, out) = validate("typed_field.yml");
    assert_eq!(code, 0, "{out}");
}

#[test]
fn a_cli_var_beats_a_plan_var() {
    let (code, out) = plan(&[&fixture_arg("var_precedence.yml"), "--var", "k=from_cli"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("k is from_cli"), "{out}");
}

// ── tags: the two mooncake bugs D7 exists to fix ──────────────────────────

#[test]
fn a_positive_tag_filter_excludes_untagged_steps() {
    // mooncake #196: `--tags x` ran untagged steps too.
    let (code, out) = plan(&[&fixture_arg("tags.yml"), "--tags", "x"]);
    assert_eq!(code, 0, "{out}");
    let line = |name: &str| {
        out.lines()
            .find(|l| l.contains(name))
            .unwrap_or_else(|| panic!("no line for {name}:\n{out}"))
    };
    assert!(line("tagged-x").contains("would run"), "{out}");
    assert!(line("tagged-always").contains("would run"), "{out}");
    assert!(line("untagged").contains("skipped"), "{out}");
}

#[test]
fn an_excluded_step_is_never_rendered() {
    // mooncake #191: `plan -t` failed on a step the tag excluded. The y-tagged
    // step reads a variable nobody defines; selecting `x` must not touch it.
    let (code, out) = plan(&[&fixture_arg("tags.yml"), "--tags", "x"]);
    assert_eq!(code, 0, "excluded step was rendered:\n{out}");
    assert!(!out.contains("never_defined_anywhere"), "{out}");
}

#[test]
fn skip_tags_wins_over_everything() {
    // `always` is excluded despite its name, and `y` stays excluded so the
    // step that reads an undefined variable is never rendered.
    let (code, out) = plan(&[&fixture_arg("tags.yml"), "--skip-tags", "always,y"]);
    assert_eq!(code, 0, "{out}");
    let line = out.lines().find(|l| l.contains("tagged-always")).unwrap();
    assert!(line.contains("skipped"), "{out}");
    let untagged = out.lines().find(|l| l.contains("untagged")).unwrap();
    assert!(
        untagged.contains("would run"),
        "no positive filter, so untagged runs: {out}"
    );
}

// ── strict ────────────────────────────────────────────────────────────────

#[test]
fn strict_rejects_only_the_ungated_shell_step() {
    let path = fixture_arg("strict_gate.yml");
    let (code, _) = validate("strict_gate.yml");
    assert_eq!(code, 0, "plain validate must accept an ungated shell step");

    let out = run(&["validate", "--strict", &path]);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION));
    let text = String::from_utf8_lossy(&out.stderr);
    assert_eq!(text.matches("no idempotency gate").count(), 1, "{text}");
    assert!(text.contains("strict_gate.yml:7:3"), "{text}");
}

// ── the documented example ────────────────────────────────────────────────

#[test]
fn the_readme_example_validates_and_plans() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/x1.yml");
    let out = run(&["validate", example.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );

    let (code, text) = plan(&[example.to_str().unwrap()]);
    assert_eq!(code, 0, "{text}");
    // The step gated on a `register` cannot be judged before the run.
    let hint = text
        .lines()
        .find(|l| l.contains("Reload shell hint"))
        .unwrap();
    assert!(hint.contains("would run (unprobed)"), "{text}");
}

#[test]
fn a_probed_plan_runs_the_asserts() {
    // Spec §6.7: plan runs asserts, because an assert failing at plan time is
    // the cheapest way to learn the plan is aimed at the wrong machine.
    //
    // The fixture's guard fails everywhere. This used to plan `examples/x1.yml`
    // and lean on that example's guard failing — but that guard refuses a list
    // of the author's own hostnames, so the test passed on three machines and
    // failed on every other one. It also meant planning against the running
    // user's real home directory, printing a diff of their dotfiles into the
    // test output on the way.
    let path = fixtures().join("failing_assert.yml");
    let out = run(&["plan", path.to_str().expect("fixture paths are UTF-8")]);
    let text = String::from_utf8_lossy(&out.stdout);
    let guard = text
        .lines()
        .find(|l| l.contains("wrong machine"))
        .unwrap_or_else(|| panic!("no guard line in:\n{text}"));
    assert!(guard.contains("FAILED"), "{text}");
    assert_eq!(out.status.code(), Some(1), "{text}");
    // Spec §6.7: plan reports the failure and keeps walking, so the rest of
    // the plan is still on screen. Only apply stops at the first failure.
    assert!(text.contains("After the guard"), "{text}");
    assert!(text.contains("2 steps"), "{text}");
}

#[test]
fn facts_reports_yaml_booleans() {
    let out = run(&["facts"]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("os "), "{text}");
    assert!(!text.contains("True") && !text.contains("False"), "{text}");
}

// ── usage errors ──────────────────────────────────────────────────────────

// Spec §8 has said `3` is the usage code since phase 0. clap's default is
// `2`, which is the code that means "plan found changes" — so a script
// asking `provision plan --bogus x.yml` was told the plan had work to do.
#[test]
fn a_bad_command_line_exits_3_for_every_command() {
    for args in [
        vec!["plan", "--bogus", "x.yml"],
        vec!["apply"],
        vec!["validate"],
        vec!["facts", "--bogus"],
        // A verb that takes an argument, given none.
        vec!["list"],
        vec!["nonsense"],
    ] {
        let out = run(&args);
        assert_eq!(
            out.status.code(),
            Some(EXIT_VALIDATION),
            "{args:?} should be a usage error:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.stderr.is_empty(),
            "{args:?} should still say what was wrong, in clap's own words"
        );
    }
}

// `--help` and `--version` reach the same code path as an error, and must
// not be turned into one: exit 0, and on stdout.
#[test]
fn help_and_version_are_not_usage_errors() {
    for args in [vec!["--help"], vec!["--version"], vec!["apply", "--help"]] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(!out.stdout.is_empty(), "{args:?} should print to stdout");
        assert!(out.stderr.is_empty(), "{args:?} should not touch stderr");
    }
}

// ── `defaults` (spec §6.10) ───────────────────────────────────────────────

#[test]
fn strict_asks_a_defaults_step_where_it_runs() {
    let (code, _) = validate("defaults_strict.yml");
    assert_eq!(code, 0, "plain validate accepts a `defaults` step as it is");

    let out = run(&["validate", "--strict", &fixture_arg("defaults_strict.yml")]);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION));
    let text = String::from_utf8_lossy(&out.stderr);
    // Exactly one: the guarded step above it says where it runs.
    assert_eq!(text.matches("naming the os").count(), 1, "{text}");
    assert!(text.contains("defaults_strict.yml:9:3"), "{text}");
}

#[test]
fn a_defaults_step_that_cannot_mean_what_it_says_is_rejected_at_parse() {
    // `array` and `dict` are deferred, and the error says where they went.
    let (code, text) = validate("defaults_bad_type.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert!(text.contains("unknown defaults type `array`"), "{text}");
    assert_positioned(&text);

    // The declared `type` decides how the value is read, so a value the type
    // cannot mean is a mistake now rather than a key that never converges.
    let (code, text) = validate("defaults_bad_value.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert!(text.contains("is not an integer"), "{text}");
    assert_positioned(&text);

    // §6.10: a root write lands in root's preferences.
    let (code, text) = validate("defaults_sudo.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert!(text.contains("must not run under `sudo`"), "{text}");
    assert_positioned(&text);
}

// ── `download` (spec §6.9) ────────────────────────────────────────────────

#[test]
fn a_sha256_that_is_not_a_digest_is_rejected_at_parse() {
    // A truncated or mistyped digest would fail every fetch with a mismatch,
    // which reads as a bad download rather than as a bad plan.
    let (code, text) = validate("download_bad_sha.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert!(text.contains("is not a sha256 digest"), "{text}");
    assert_positioned(&text);
}
