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
    assert!(out.contains("`cmd` is a list of arguments"), "{out}");
    assert_eq!(
        out.matches("expected true or false, found a string")
            .count(),
        3,
        "a quoted bool was taken as false:\n{out}"
    );
    assert!(
        out.contains("`content` does not apply to `state: link`"),
        "{out}"
    );
    assert!(
        out.contains("`src` does not apply to `state: dir`"),
        "{out}"
    );
    assert_positioned(&out);
}

#[test]
fn validate_rejects_what_only_plan_or_apply_used_to() {
    // The same rule, three more holes: an undefined variable in `name`
    // failed `plan`, and a `changed_when` or `failed_when` that does not
    // parse failed `apply` — after the step's command had already run.
    let (code, out) = validate("apply_only_errors.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("undefined variable `pkgname`"), "{out}");
    assert!(
        out.contains("apply_only_errors.yml:14:"),
        "changed_when:\n{out}"
    );
    assert!(
        out.contains("apply_only_errors.yml:19:"),
        "failed_when:\n{out}"
    );
    assert!(out.contains("3 problems"), "{out}");
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

// `use` and `import` build a scope and walk it, and never reach `do_action`,
// so none of these is ever read. All eleven were accepted and dropped on the
// floor. The file's own rule -- silently ignoring a misplaced modifier is how
// a typo becomes a two-hour debug -- had been applied to `props` and
// `optional` and to nothing else.
//
// `register` is the one with teeth: D5 names it as the handler replacement,
// so it was the prescribed recipe doing nothing across a component boundary.
#[test]
fn a_modifier_a_structural_step_cannot_honour_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib.yml");
    std::fs::write(
        &lib,
        "description: a component\nsteps:\n  - name: inner\n    shell: \"true\"\n    changed_when: \"false\"\n",
    )
    .unwrap();

    for (key, body) in [
        ("unless", "unless: \"true\""),
        ("creates", "creates: /tmp"),
        ("changed_when", "changed_when: \"false\""),
        ("failed_when", "failed_when: \"false\""),
        ("env", "env: { A: b }"),
        ("register", "register: r"),
        ("cwd", "cwd: /tmp"),
        ("sudo", "sudo: true"),
        ("timeout", "timeout: 30s"),
        ("retry", "retry: { attempts: 2 }"),
        ("raw", "raw: true"),
    ] {
        for verb in ["use", "import"] {
            let plan = dir.path().join(format!("{key}_{verb}.yml"));
            std::fs::write(
                &plan,
                format!("- name: call\n  {verb}: {}\n  {body}\n", lib.display()),
            )
            .unwrap();
            let (code, out) = {
                let o = run(&["validate", &plan.display().to_string()]);
                (
                    o.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&o.stderr).into_owned(),
                )
            };
            assert_eq!(code, EXIT_VALIDATION, "`{key}` on a `{verb}`: {out}");
            assert!(
                out.contains(&format!("`{key}` has no effect on a `{verb}`")),
                "`{key}` on a `{verb}`: {out}"
            );
        }
    }
}

// The two that do propagate, and must keep doing so: they gate the subtree
// before the walk descends, which is a different thing from being handed to
// a command a structural step never runs.
#[test]
fn when_and_tags_still_reach_a_structural_step() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib.yml");
    std::fs::write(
        &lib,
        "description: a component\nsteps:\n  - name: inner\n    shell: \"true\"\n    changed_when: \"false\"\n",
    )
    .unwrap();
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        format!(
            "- name: call\n  use: {}\n  when: os == \"darwin\"\n  tags: [mac]\n",
            lib.display()
        ),
    )
    .unwrap();
    let out = run(&["validate", &plan.display().to_string()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// `optional` belongs to `vars_file`, which is itself structural -- so the
// blanket rule must not eat the one modifier a structural step does honour.
#[test]
fn optional_still_works_on_a_vars_file() {
    let dir = tempfile::tempdir().unwrap();
    let vars = dir.path().join("v.yml");
    std::fs::write(&vars, "a: 1\n").unwrap();
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        format!(
            "- name: load\n  vars_file: {}\n  optional: true\n",
            vars.display()
        ),
    )
    .unwrap();
    let out = run(&["validate", &plan.display().to_string()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_modifier_bool_given_as_a_string_is_an_error() {
    // `sudo: "true"` read as false would run the step unprivileged.
    let (code, out) = validate("bad_modifier_bools.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    for line in [3, 6, 9] {
        let at = format!("bad_modifier_bools.yml:{line}:");
        assert!(
            out.lines()
                .any(|l| l.contains(&at) && l.contains("expected true or false")),
            "{out}"
        );
    }
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
fn a_file_src_no_step_could_create_still_fails_before_anything_runs() {
    // Spec §8, D18: the deferral is for a source the plan itself builds. A
    // plan with no command in it builds nothing, so this is the fail-fast the
    // deferral must not cost — and it says so twice, once for the source and
    // once for the bytes, both at the `src` that named it.
    let (code, out) = validate("missing_file_src.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("file src not found"), "{out}");
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

// The same boundary for a `register`. Registered names were tracked in one set
// for the whole run, so a parent `when` reading a component's register looked
// like a read of a result that has not run yet: `validate` and `plan` skipped
// it as unprobed (D14), and only `apply` found the name undefined — after
// every step before it had run.
#[test]
fn a_components_register_does_not_leak_back_to_its_parent() {
    let (code, out) = validate("register_isolation.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    // The name, not just "undefined value": the parent is reading
    // `inner_result`, and saying so is the difference between this error and
    // a scavenger hunt (§3.4).
    assert!(out.contains("undefined variable `inner_result`"), "{out}");
    assert!(out.contains("register_isolation.yml:5:"), "{out}");
    assert_positioned(&out);
}

// The headline case for §3.4. One typo at a call site, one diagnostic, naming
// the variable the reader has to fix -- not the prop, and not the component's
// declaration line in another file.
//
// A sole-expression field that evaluated to undefined used to come back
// `Ok(undefined)`, because strict mode fires only when a value is *used*. The
// prop type check then rejected it as "got nothing", and `fill_defaults`
// added a second error calling it missing.
#[test]
fn an_undefined_variable_in_a_prop_value_names_the_variable() {
    let (code, out) = validate("undefined_in_prop.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("undefined variable `nope`"), "{out}");
    assert!(
        !out.contains("got nothing"),
        "that is the symptom, not the cause: {out}"
    );
    assert!(
        !out.contains("missing required prop"),
        "it was given a value; it is wrong, not missing: {out}"
    );
    assert!(out.contains("1 problem"), "exactly one: {out}");
    assert_positioned(&out);
}

// `--prop` values are templates too (§8), and the same context was thrown
// away there.
#[test]
fn an_undefined_variable_in_a_cli_prop_names_the_variable() {
    let path = fixture_arg("component/needs_repo.yml");
    let out = run(&["validate", "--prop", "repo={{ nope }}", &path]);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("undefined variable `nope`"), "{text}");
    assert!(!text.contains("missing required prop"), "{text}");
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

// ── tags: a filter value that matches nothing ─────────────────────────────
//
// A divergence from D7's "Ansible semantics", amended there. Ansible accepts
// an unknown tag and runs nothing; here that was an apply which did nothing
// reporting success, and the inverse typo ran everything — both silent, and
// `break-glass.md` §3b has an operator typing one from memory mid-incident.

#[test]
fn an_unknown_tag_is_a_usage_error_not_an_empty_run() {
    let (code, out) = plan(&[&fixture_arg("tags.yml"), "--tags", "handlr"]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(
        out.contains("`handlr` matches no step in this run"),
        "{out}"
    );
    // The note is the fix, not decoration: there is no other way to ask a
    // plan what tags it has (`list` on a plan refuses).
    assert!(out.contains("tags in this plan: always, x, y"), "{out}");
}

#[test]
fn an_unknown_skip_tag_is_refused_the_same_way() {
    // The inverse typo is the worse one: it silently ran *everything*. Not
    // `tags.yml`: excluding nothing there renders its deliberately-undefined
    // step, and the check stands down once the walk has other problems.
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        "- name: a\n  shell: \"true\"\n  changed_when: \"false\"\n  tags: [slow]\n",
    )
    .unwrap();
    let out = run(&[
        "apply",
        "--skip-tags",
        "handlr",
        &plan.display().to_string(),
    ]);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("`handlr` matches no step in this run"),
        "{err}"
    );
    assert!(err.contains("tags in this plan: slow"), "{err}");
}

#[test]
fn one_good_tag_does_not_excuse_a_typo_beside_it() {
    // Per value, not per flag: `--tags x,handlr` still selects steps, so a
    // "selected nothing" check would miss this entirely.
    let (code, out) = plan(&[&fixture_arg("tags.yml"), "--tags", "x,handlr"]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("`handlr`"), "{out}");
    assert!(!out.contains("`x` matches"), "x is a real tag: {out}");
}

#[test]
fn always_is_a_keyword_not_a_tag_the_plan_must_declare() {
    // `--skip-tags always` is documented (§8) and is written for plans that
    // have an `always` step somewhere below — refusing it on a plan that
    // happens not to declare one would reject the idiom where it is needed.
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        "- name: a\n  shell: \"true\"\n  changed_when: \"false\"\n",
    )
    .unwrap();
    let out = run(&[
        "apply",
        "--skip-tags",
        "always",
        &plan.display().to_string(),
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// The load-bearing one. An unknown `--tags` value does not skip everything:
// `always` steps run regardless, so the typo'd filter still touches the
// machine. This pins the refusal to the preflight — move the check into the
// executing walk and the marker appears, because a diag raised mid-walk only
// stops it at the *next* step (D26, src/expand.rs).
#[test]
fn apply_refuses_an_unknown_tag_before_an_always_step_runs() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        format!(
            "- name: always runs\n  shell: touch {}\n  creates: {}\n  tags: [always]\n",
            marker.display(),
            marker.display()
        ),
    )
    .unwrap();

    let out = run(&["apply", "--tags", "handlr", &plan.display().to_string()]);
    assert_eq!(
        out.status.code(),
        Some(EXIT_VALIDATION),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !marker.exists(),
        "the always-tagged step ran before the typo was caught"
    );
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

// D25: a problem is something the program said, so under `--json` it rides
// the same stream as everything else. Before this the stream carried steps
// and a summary and nothing about why a run was refused, so a consumer had
// to regex the prose on stderr.
#[test]
fn validate_json_emits_one_diagnostic_event_per_problem() {
    let path = fixture_arg("three_problems_a.yml");
    let out = run(&["validate", "--json", &path]);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line is one object"))
        .collect();
    assert_eq!(events.len(), 3, "{stdout}");
    for e in &events {
        assert_eq!(e["event"], "diagnostic");
        assert!(e["file"].is_string(), "{e}");
        assert!(e["line"].is_number(), "{e}");
        // A diagnostic knows which key on the line is wrong; a step does not.
        assert!(e["col"].is_number(), "{e}");
        assert!(e["message"].is_string(), "{e}");
    }
    assert!(
        events[0]["message"]
            .as_str()
            .unwrap()
            .contains("unknown key `creats`"),
        "{stdout}"
    );
    assert_eq!(events[0]["note"], "did you mean `creates`?");

    // The human text is unchanged and still on stderr.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("3 problems validating"), "{stderr}");
}

// The actual bug behind the issue: apply's preflight runs before the apply
// expander and its sink exist, so `apply --json` on a bad file put nothing
// at all on the stream a runner was reading.
#[test]
fn apply_json_emits_its_preflight_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.yml");
    std::fs::write(&plan, "- name: ungated\n  shell: \"true\"\n").unwrap();

    let out = run(&["apply", "--json", &plan.display().to_string()]);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first: serde_json::Value =
        serde_json::from_str(stdout.lines().next().unwrap_or("")).expect("a diagnostic event");
    assert_eq!(first["event"], "diagnostic");
    assert!(
        first["message"]
            .as_str()
            .unwrap()
            .contains("no idempotency gate"),
        "{stdout}"
    );
    // Nothing ran, so there is no step event and no summary to mislead a
    // consumer that reads the last line.
    assert!(!stdout.contains("\"event\":\"summary\""), "{stdout}");
    assert!(!stdout.contains("\"event\":\"step\""), "{stdout}");
}

// ── strict ────────────────────────────────────────────────────────────────

// `strict_gate.yml` is a plan (list root), so `--strict` is on by default
// there (D3's reversal, issue #5) — the whole point is that an operator
// never had to remember the flag on the file that most needs it.
#[test]
fn strict_rejects_only_the_ungated_shell_step() {
    let path = fixture_arg("strict_gate.yml");
    let (code, text) = validate("strict_gate.yml");
    assert_eq!(code, EXIT_VALIDATION, "a plan defaults to --strict: {text}");
    assert_eq!(text.matches("no idempotency gate").count(), 1, "{text}");
    assert!(text.contains("strict_gate.yml:7:3"), "{text}");

    // The flag still works, redundantly, on a plan.
    let out = run(&["validate", "--strict", &path]);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION));
    let text = String::from_utf8_lossy(&out.stderr);
    assert_eq!(text.matches("no idempotency gate").count(), 1, "{text}");
    assert!(text.contains("strict_gate.yml:7:3"), "{text}");
}

// A task or component file legitimately has no state to declare (D3's
// carve-out survives), so it stays opt-in there — `--strict` only ever
// turns the check on, never off.
#[test]
fn strict_defaults_off_for_a_task_or_component() {
    let path = fixture_arg("component/task.yml");
    let out = run(&["validate", "--prop", "target=x", &path]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a task file must not default to --strict: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run(&["validate", "--strict", "--prop", "target=x", &path]);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("no idempotency gate"), "{text}");
}

// D26: the gate is a property of the file, so every verb that walks a plan
// runs it. Before this, `strict_gate.yml` was exit 3 from `validate` and
// exit 0 from both `plan` and `apply` — the regression D20 named in writing,
// live on the only verb that touches the machine.
#[test]
fn the_gate_fires_under_plan_and_apply_too() {
    let path = fixture_arg("strict_gate.yml");
    for verb in [
        vec!["validate"],
        vec!["plan", "--plan-no-probe"],
        vec!["apply"],
    ] {
        let mut args = verb.clone();
        args.push(&path);
        let out = run(&args);
        let text = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(EXIT_VALIDATION),
            "{verb:?} must refuse an ungated plan: {text}"
        );
        assert_eq!(
            text.matches("no idempotency gate").count(),
            1,
            "{verb:?}: {text}"
        );
        assert!(text.contains("strict_gate.yml:7:3"), "{verb:?}: {text}");
    }
}

// The load-bearing half of D26. A gate error raised during the walk that
// *executes* arrives too late: `walk` only re-reads `diags` at the top of the
// next step, so the ungated command has already run. The check belongs to
// apply's preflight, and this is the test that says so — without it a later
// refactor could move it into the apply walk and everything else would still
// pass.
#[test]
fn apply_refuses_before_the_first_step_runs() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        format!(
            "- name: writes a marker\n  \
               shell: touch {}\n  \
               changed_when: \"false\"\n\n\
             - name: ungated\n  \
               shell: \"true\"\n",
            marker.display()
        ),
    )
    .unwrap();

    let out = run(&["apply", &plan.display().to_string()]);
    let text = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION), "{text}");
    assert!(text.contains("no idempotency gate"), "{text}");
    assert!(
        !marker.exists(),
        "apply must refuse before running step 1, not after"
    );
}

// The check answers for the steps the run would run. `validate` takes no tag
// flags, so it answers for the whole file; `plan` and `apply` do, so a
// filtered run is judged on its own selection. Checking steps the run has
// excluded would make `apply --tags db` refuse over work it will not do, and
// the invariant D20 names is pointwise over executed commands anyway: a
// violation `--skip-tags` hides from the operator is hidden from the machine
// too.
#[test]
fn a_filtered_run_is_judged_on_the_steps_it_would_run() {
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        "- name: gated\n  shell: \"true\"\n  changed_when: \"false\"\n\n\
         - name: ungated\n  shell: \"true\"\n  tags: [slow]\n",
    )
    .unwrap();
    let path = plan.display().to_string();

    let out = run(&["apply", &path]);
    assert_eq!(
        out.status.code(),
        Some(EXIT_VALIDATION),
        "unfiltered, the ungated step is in the run: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run(&["apply", "--skip-tags", "slow", &path]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "skipped out of the run, so not this run's problem: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

// `defaults_strict.yml` is a plan too, so this is on by default now.
#[test]
fn strict_asks_a_defaults_step_where_it_runs() {
    let (code, text) = validate("defaults_strict.yml");
    assert_eq!(code, EXIT_VALIDATION, "a plan defaults to --strict: {text}");
    // Two: the guarded step says where it runs, and the lookalike only reads
    // a variable whose name happens to contain "os".
    assert_eq!(text.matches("naming the os").count(), 2, "{text}");
    assert!(text.contains("defaults_strict.yml:9:3"), "{text}");
    assert!(text.contains("defaults_strict.yml:19:3"), "{text}");
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
