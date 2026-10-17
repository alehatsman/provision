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

fn validate_with(fixture: &str, extra: &[&str]) -> (i32, String) {
    let path = fixtures().join(fixture);
    let mut args = vec!["validate", path.to_str().expect("fixture paths are UTF-8")];
    args.extend_from_slice(extra);
    let out = run(&args);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
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
    // Derived from the same `cfg!` the code uses, not asserted as a literal
    // (the rule `tests/apply.rs` opens by stating). On Windows the action is
    // refused outright before its body is read, which is the right order: an
    // unsupported action's body is not worth a second diagnostic.
    assert!(
        out.contains(if cfg!(windows) {
            "`service` is not supported on Windows"
        } else {
            "`service` needs `state` or `enabled`"
        }),
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
    // plan with no command and no declaration in it builds nothing, so this
    // is the fail-fast the deferral must not cost.
    let (code, out) = validate("missing_file_src.yml");
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("file src not found"), "{out}");
    assert!(
        out.contains("relative to the file that names them"),
        "{out}"
    );
    // §9.3, one fault one diagnostic. It used to say this twice, once for the
    // source and once for the bytes, in two wordings at one position — and
    // only the first carries the note about how paths resolve.
    assert_eq!(
        out.matches("missing_file_src.yml:").count(),
        1,
        "one missing source, more than one diagnostic:\n{out}"
    );
    assert_positioned(&out);
}

// D18's overturn clause. `git`, `download` and `template` declare a `dest`
// and `file` declares a `path`, so a later `src` inside one of them is
// pending, not missing. Before this, the plan below was exit 3 — for a path
// it creates two lines above — while inserting any `shell` step unlocked it.
#[test]
fn a_file_src_under_a_declared_dest_is_deferred() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("declared.yml");
    std::fs::write(
        &path,
        "description: clone then read out of the checkout\n\
         steps:\n\
         \x20 - name: clone\n\
         \x20   git: {repo: \"https://example.invalid/x.git\", dest: ./dotfiles}\n\
         \x20 - name: link\n\
         \x20   file: {path: ./out/gitconfig, state: file, src: ./dotfiles/gitconfig}\n",
    )
    .unwrap();
    let out = run(&["validate", &path.display().to_string()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{text}");
}

// The other half: a declaration names one subtree and defers only that. A
// typo anywhere else is still caught before anything runs — which is what
// makes this a reading of what the plan said rather than the guess D18
// rejected by name.
#[test]
fn a_file_src_outside_every_declared_dest_still_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("typo.yml");
    std::fs::write(
        &path,
        "description: a typo outside the checkout\n\
         steps:\n\
         \x20 - name: clone\n\
         \x20   git: {repo: \"https://example.invalid/x.git\", dest: ./dotfiles}\n\
         \x20 - name: link\n\
         \x20   file: {path: ./out/gitconfig, state: file, src: ./elsewhere/typo}\n",
    )
    .unwrap();
    let out = run(&["validate", &path.display().to_string()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION), "{text}");
    assert!(text.contains("file src not found"), "{text}");
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

// Whether a tag exists is a property of the FILE; whether it selected
// anything is a property of THIS RUN. The first version of the check asked
// the second question and reported the answer as a typo.
#[test]
fn a_declared_tag_that_selects_nothing_here_is_not_a_typo() {
    // `otheros` is real, declared in a subtree gated off on this machine.
    // Selecting it does nothing, which is the ordinary state of a
    // cross-platform plan, and is not an error.
    for root in ["tag_scope/plan.yml", "tag_scope/task.yml"] {
        let path = fixtures().join(root);
        let out = run(&[
            "plan",
            "--plan-no-probe",
            path.to_str().unwrap(),
            "--tags",
            "otheros",
        ]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{root}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn tag_existence_does_not_depend_on_the_roots_shape() {
    // The same bytes under a list root and a mapping root. Before the split,
    // the tag set was a walk artifact and the descent was gated on `strict`,
    // so `otheros` was visible on the plan root and invisible on the task
    // root -- one file, two answers to a question about the file.
    for root in ["tag_scope/plan.yml", "tag_scope/task.yml"] {
        let path = fixtures().join(root);
        let out = run(&[
            "plan",
            "--plan-no-probe",
            path.to_str().unwrap(),
            "--tags",
            "handlr",
        ]);
        let text = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(EXIT_VALIDATION), "{root}: {text}");
        assert!(text.contains("`handlr` matches no step"), "{root}: {text}");
        // And the note names the whole file's tags, both subtrees included.
        assert!(text.contains("here"), "{root}: {text}");
        assert!(text.contains("otheros"), "{root}: {text}");
    }
}

#[test]
fn tags_always_is_not_exempt_from_the_unknown_tag_check() {
    // The exemption was granted to both flags; §8 only ever justified it for
    // `--skip-tags`. On `--tags` it reinstated the exact bug the check exists
    // to kill: `apply --tags always` on a plan with no `always`-tagged step
    // selected nothing, did nothing, exited 0.
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        "- name: a\n  shell: \"true\"\n  changed_when: \"false\"\n  tags: [core]\n",
    )
    .unwrap();
    let path = plan.display().to_string();

    let out = run(&["apply", "--tags", "always", &path]);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("`always` matches no step"), "{err}");

    // But a plan that DOES declare it is selected normally.
    let withit = dir.path().join("withit.yml");
    std::fs::write(
        &withit,
        "- name: a\n  shell: \"true\"\n  changed_when: \"false\"\n  tags: [always]\n",
    )
    .unwrap();
    let out = run(&["apply", "--tags", "always", &withit.display().to_string()]);
    assert_eq!(out.status.code(), Some(0));
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
// The example is a Linux workstation's plan: it asks for `sudo`, which
// Windows has not got, so `validate` refuses it there and is right to. The
// assertion is scoped to where it means something rather than weakened to
// something true everywhere — "the README's own example still validates" is
// the claim worth keeping, and it is a claim about a Unix host.
#[cfg(not(windows))]
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

// ── the `facts.` namespace (#36, spec §5) ────────────────────────────────

#[test]
fn a_vars_step_cannot_shadow_the_facts_namespace() {
    // D25's blast-radius answer is a convention: the plan declares its own
    // machine in its first step. That convention did not hold, because the
    // guard read the bare name and §3.3 lets a `vars` step outrank it -- so
    // `vars: {hostname: x1}` satisfied the guard on every machine. The
    // precedence rule is right and stays (it is how a Linux plan is
    // validated from a Mac); what was missing was a way to say "the real
    // one".
    //
    // A *probed* plan: the `plan()` helper passes `--plan-no-probe`, which
    // would skip every assert and make this test pass without evaluating a
    // single one of them.
    let path = fixtures().join("shadowed_guard.yml");
    let out = run(&["plan", path.to_str().expect("fixture paths are UTF-8")]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{text}");
    // Proof all three asserts actually ran, rather than being skipped into
    // silence: three `ok` step lines, and none reported `skipped`.
    assert_eq!(text.matches('\u{2713}').count(), 3, "{text}");
    assert!(!text.contains("skipped"), "{text}");
}

#[test]
fn a_gated_off_use_does_not_render_its_props() {
    // Found by an adversarial review of D27. `open()` was shape-aware and
    // `bind_props` two lines later was not, so a gated-off `use:` still
    // rendered and type-checked its prop values -- mooncake #191 in the one
    // place the guard was missed.
    //
    // The assertion is the second-order damage, which is the worse half:
    // `bind_props` returning Err meant the component's steps were never
    // walked, so a templated prop silently disabled the strict gate for that
    // whole component. The ungated `shell` inside MUST be the one and only
    // thing reported.
    let (code, text) = validate("gated_subtree/props.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert_eq!(text.matches("error:").count(), 1, "{text}");
    assert!(text.contains("no idempotency gate"), "{text}");
    assert!(text.contains("comp/gui.yml:8:5"), "{text}");
    // The prop value and the missing vars_file are both un-rendered and
    // un-resolved: a gated-off subtree is read for what it says, never for
    // what it would do (D27).
    assert!(!text.contains("other_os_session"), "{text}");
    assert!(!text.contains("nonexistent-vars"), "{text}");
}

#[test]
fn the_defaults_os_gate_accepts_the_unshadowable_spelling() {
    // `facts.os` is the spelling §5 makes unshadowable and the one
    // examples/x1.yml uses for its host guard. The gate asked
    // `undeclared_variables(false)`, which returns top-level names, so it saw
    // `facts` and refused -- the tool demanding the weaker form a `vars` step
    // can defeat.
    let dir = tempfile::tempdir().unwrap();
    let step = "  defaults: {domain: com.apple.dock, key: a, type: bool, value: true}\n";

    for gate in ["facts.os == \"darwin\"", "os == \"darwin\""] {
        let p = dir.path().join("g.yml");
        std::fs::write(&p, format!("- name: dock\n{step}  when: {gate}\n")).unwrap();
        let out = run(&["validate", &p.display().to_string()]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "`when: {gate}` was refused: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // A name that merely contains "os" is still not the os fact.
    let p = dir.path().join("lk.yml");
    std::fs::write(
        &p,
        format!("- vars: {{hostos: darwin}}\n- name: dock\n{step}  when: hostos == \"darwin\"\n"),
    )
    .unwrap();
    let out = run(&["validate", &p.display().to_string()]);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION));
}

// ── `pkg` version floor (D28, spec §6.5) ─────────────────────────────────

#[test]
fn a_floor_is_rejected_where_it_has_no_meaning() {
    // `absent` has no version. Caught at validate, at the key's own line,
    // rather than at apply.
    let (code, text) = validate("pkg_version_bad_state.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert!(text.contains("`version` does not apply"), "{text}");
    assert_positioned(&text);
}

#[test]
fn only_a_floor_form_is_accepted() {
    // An exact pin is a different feature: it needs the manager's own
    // syntax, which is exactly what this design avoids. The error says so by
    // naming the form rather than silently taking "24" as ">=24".
    let (code, text) = validate("pkg_version_bad_form.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert!(text.contains("is not a version floor"), "{text}");
    assert!(text.contains("`>=` and a version"), "{text}");
}

// ── a subtree this machine gates off (D27) ───────────────────────────────

#[test]
fn strict_reads_a_subtree_its_when_ruled_out_on_this_machine() {
    // Before: `validate` evaluated `when`, so a `when: os == "linux"`
    // component was invisible on a Mac and the same plan validated
    // differently on different hosts. The ungated `shell` was found by
    // whichever machine happened to match.
    let (code, text) = validate("gated_subtree/root.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert!(text.contains("no idempotency gate"), "{text}");
    assert!(text.contains("bits.yml:15:3"), "{text}");
}

#[test]
fn a_gated_off_subtree_is_read_for_shape_and_never_rendered() {
    // The whole risk of walking in, and the reason it is shape-only. That
    // fixture's other four steps each read something that does not exist on
    // the validating machine -- an undefined variable, a missing `src`, a
    // nested import named through that variable. Rendering any of them is
    // mooncake #191, the failure D7's ordering exists to prevent: a step
    // that is skipped must not fail on what it reads.
    let (code, text) = validate("gated_subtree/root.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert_eq!(
        text.matches("error:").count(),
        1,
        "exactly one fault: {text}"
    );
    assert!(!text.contains("only_on_the_other_os"), "{text}");
    assert!(!text.contains("not found"), "{text}");
}

#[test]
fn an_os_gate_on_a_false_import_still_gates_the_defaults_inside_it() {
    // The §6.10 rule asks what the plan says, not what this machine is. The
    // import is false here and its `when` still names the os, so the
    // `defaults` steps inside are gated.
    let (code, text) = validate("gated_subtree/os_gated.yml");
    assert_eq!(code, 0, "{text}");
}

#[test]
fn the_three_verbs_agree_on_a_gated_off_subtree() {
    // Spec §8: one file, one set of arguments, one verdict. The descent is
    // gated on `strict` rather than on the verb precisely so this holds --
    // restricting it to `validate` would make it refuse a file `plan`
    // accepts, with the same arguments on the same machine.
    let path = fixtures().join("gated_subtree/root.yml");
    let path = path.to_str().unwrap();
    for verb in ["validate", "plan", "apply"] {
        let out = run(&[verb, path]);
        assert_eq!(
            out.status.code(),
            Some(EXIT_VALIDATION),
            "{verb} disagreed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ── templated typed modifiers (#35, spec §3.4) ───────────────────────────

#[test]
fn the_typed_modifiers_take_a_template() {
    // `sudo`, `timeout` and both `retry` fields, driven from typed props.
    let (code, text) = validate_with("templated_mods/ok.yml", &["--prop", "root=false"]);
    assert_eq!(code, 0, "{text}");
}

#[test]
fn a_template_that_renders_to_the_wrong_type_is_still_refused() {
    // The hole this could have left: every read of these three is lenient
    // (`.ok()`, `unwrap_or`), which was safe only while parsing had already
    // proved the type. Deferring the parse-time check without adding a
    // render-time one would turn a bad `sudo` into a silent `false`.
    let (code, text) = validate("templated_mods/bad_sudo.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert!(text.contains("expected true or false"), "{text}");
    assert!(text.contains("rendered to a string"), "{text}");
    assert_positioned(&text);

    let (code, text) = validate("templated_mods/bad_timeout.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    // The rendered value is named, not the template text: `banana` is what
    // is wrong, and `{{ props.t }}` is where to look.
    assert!(text.contains("`banana` is not a duration"), "{text}");
}

#[test]
fn a_literal_typed_modifier_is_still_checked_at_parse_time() {
    // Deferring applies to templates only. A literal is proved when the file
    // is parsed, as it always was, which is almost every one.
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        "- name: x\n  shell: \"true\"\n  changed_when: \"false\"\n  timeout: nope\n",
    )
    .unwrap();
    let out = run(&["validate", &plan.display().to_string()]);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("`nope` is not a duration"), "{err}");
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

// #40c. Spec §3.1: a `when` on an `import`/`use` "applies to every imported
// step". §6.10's `defaults` gate is the only rule that reads a step's `when`
// directly, and it read only the step's own — so the shape the spec
// recommends (one os gate on the import) was refused, while the same `when`
// pasted onto every step inside it was accepted. The rule disagreed with the
// spec about what "gated" means, in the direction that punishes the tidier
// plan.
#[test]
fn an_os_gate_on_the_import_gates_the_defaults_steps_inside_it() {
    let (code, text) = validate("defaults_import/gated.yml");
    assert_eq!(code, 0, "{text}");
}

#[test]
fn the_defaults_gate_is_read_in_a_wider_scope_not_weakened() {
    // Still refused when nothing gates them: both steps, each at its own
    // position in the imported file.
    let (code, text) = validate("defaults_import/ungated.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert_eq!(text.matches("naming the os").count(), 2, "{text}");
    assert!(text.contains("macdefaults.yml:5:3"), "{text}");
    assert!(text.contains("macdefaults.yml:7:3"), "{text}");
}

#[test]
fn a_gated_import_does_not_gate_what_follows_it() {
    // The flag is saved and restored around the descent, not set: an os-gated
    // import gates the steps inside it and nothing after it. Without the
    // restore this plan would validate clean and the sibling would ship.
    let (code, text) = validate("defaults_import/sibling.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert_eq!(text.matches("naming the os").count(), 1, "{text}");
    assert!(text.contains("sibling.yml:5:3"), "{text}");
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

    // §6.10: a root write lands in root's preferences. On Windows there is no
    // `sudo` at all, so the step is refused one rule earlier — still exit 3,
    // still positioned, and the test says which answer it expects where
    // rather than asserting the host it happened to run on.
    let (code, text) = validate("defaults_sudo.yml");
    assert_eq!(code, EXIT_VALIDATION, "{text}");
    assert!(
        text.contains(if cfg!(windows) {
            "`sudo` is not supported on Windows"
        } else {
            "must not run under `sudo`"
        }),
        "{text}"
    );
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

// Spec §9.3: one fault, one diagnostic. A step's `name` is rendered twice —
// once by the field check, once for the step's own line — and a bad filter
// in it used to arrive twice and be counted as `2 problems`, while
// `validate`, which renders it once, reported the same file as `1 problem`.
#[test]
fn one_fault_in_a_steps_name_is_one_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.yml");
    std::fs::write(
        &path,
        "- name: \"B {{ nope | zzzfilter }}\"\n  shell: \"true\"\n  changed_when: \"false\"\n",
    )
    .unwrap();
    let (code, out) = plan(&[&path.display().to_string()]);
    assert_eq!(code, 3, "{out}");
    assert_eq!(
        out.matches("unknown filter").count(),
        1,
        "the same fault was reported more than once:\n{out}"
    );
    assert!(out.contains("1 problem"), "{out}");
}

// §3.2: a prop `default` is taken as written, never rendered — a legal
// string, and never a legal path. Before this, `validate` and `plan` both
// called this file clean and `apply` created a directory named `{{ home }}`,
// which inverts what the two read-only verbs are for.
#[test]
fn an_unrendered_default_reaching_a_path_is_refused_before_anything_runs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.yml");
    std::fs::write(
        &path,
        "description: a default that was never rendered\n\
         props:\n\
         \x20 dir:\n\
         \x20   type: string\n\
         \x20   default: \"{{ home }}/.cache/x\"\n\
         steps:\n\
         \x20 - name: make it\n\
         \x20   file: { path: \"{{ props.dir }}\", state: dir }\n",
    )
    .unwrap();
    let out = run(&["validate", &path.display().to_string()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION), "{text}");
    assert!(
        text.contains("still contains `{{` after rendering"),
        "{text}"
    );
    assert!(text.contains("taken as written"), "{text}");
}

// The rule it must not overturn: `{{` in a *string* is legal, which is the
// whole of §3.2, and `list` prints such a default with its braces intact.
#[test]
fn an_unrendered_default_in_a_plain_string_is_still_legal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ok.yml");
    std::fs::write(
        &path,
        "description: a default that is data, not a path\n\
         props:\n\
         \x20 greeting:\n\
         \x20   type: string\n\
         \x20   default: \"hello {{ name }}\"\n\
         steps:\n\
         \x20 - name: say it\n\
         \x20   shell: \"echo {{ props.greeting }}\"\n\
         \x20   changed_when: \"false\"\n",
    )
    .unwrap();
    let out = run(&["validate", &path.display().to_string()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{text}");
}

// #46: `$VAR` in a field provision reads itself is a literal — correct by
// the rule, and almost never meant. A `creates` that never matches degrades
// the gate to "always run" while `--strict` stays satisfied, because a
// `creates` is present. Nothing said so at either read-only verb.
#[test]
fn a_shell_variable_in_a_provision_facing_path_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cr.yml");
    std::fs::write(
        &path,
        "description: a gate that never matches\n\
         steps:\n\
         \x20 - name: gated\n\
         \x20   shell: \"true\"\n\
         \x20   creates: \"$CACHE_DIR/hit\"\n",
    )
    .unwrap();
    let out = run(&["validate", &path.display().to_string()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION), "{text}");
    assert!(text.contains("shell variable `$CACHE_DIR`"), "{text}");
    assert!(text.contains("{{ env.CACHE_DIR }}"), "{text}");
}

// The other side of the same rule, and the reason it is about which fields
// reach a shell rather than which look like paths: `unless` *is* a shell
// command, `$VAR` expands there, and it must not be flagged.
#[test]
fn a_shell_variable_in_unless_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("un.yml");
    std::fs::write(
        &path,
        "description: a gate that is a shell command\n\
         steps:\n\
         \x20 - name: gated\n\
         \x20   shell: \"true\"\n\
         \x20   unless: \"test -f $CACHE_DIR/hit\"\n",
    )
    .unwrap();
    let out = run(&["validate", &path.display().to_string()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{text}");
}

/// Write one step into a tempdir and validate it. The step body is given
/// whole, because which key the path sits under is the variable here.
fn validate_step(dir: &Path, step: &str) -> (i32, String) {
    let path = dir.join("p.yml");
    std::fs::write(
        &path,
        format!("description: one step\nsteps:\n  - name: the step\n{step}"),
    )
    .expect("writing into the test's own tempdir");
    let out = run(&["validate", &path.display().to_string()]);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

// Spec §3.2 lists four field families provision reads as paths itself, and
// two of them had no test: `dest` on all three of `template`, `download` and
// `git`, and `cwd` on any step. The check is one function over a table
// (`src/expand.rs:1688`), so a family missing from that table fails silently
// and in exactly the way the rule exists to prevent -- `$VAR` taken as a
// literal directory name, with `validate` and `plan` both calling the file
// clean.
#[test]
fn a_shell_variable_is_refused_in_every_path_field_provision_reads() {
    let dir = tempfile::tempdir().unwrap();
    for (what, step) in [
        (
            "template dest",
            "    template: { src: ./t.j2, dest: \"$OUT/rendered\" }\n",
        ),
        (
            "download dest",
            "    download: { url: \"https://example.invalid/x\", dest: \"$OUT/x\", sha256: \"0000000000000000000000000000000000000000000000000000000000000000\" }\n",
        ),
        (
            "git dest",
            "    git: { repo: \"https://example.invalid/r.git\", dest: \"$OUT/r\" }\n",
        ),
        (
            "cwd",
            "    shell: \"true\"\n    cwd: \"$OUT\"\n    changed_when: \"false\"\n",
        ),
    ] {
        let (code, text) = validate_step(dir.path(), step);
        assert_eq!(code, EXIT_VALIDATION, "{what} was accepted:\n{text}");
        assert!(
            text.contains("shell variable `$OUT`"),
            "{what} did not name the variable:\n{text}"
        );
        assert!(
            text.contains("{{ env.OUT }}"),
            "{what} did not say what to write instead:\n{text}"
        );
    }
}

// §3.2: "a `raw: true` body is not rendered at all and is exempt; `creates`
// and `cwd` are rendered whatever `raw` says, so they are checked whatever
// `raw` says." Both halves, because the exemption is the part that can
// quietly grow to cover the modifiers too -- and a `creates` silently
// exempted is a gate that never matches with `--strict` still satisfied.
#[test]
fn a_raw_body_is_exempt_and_its_modifiers_are_not() {
    let dir = tempfile::tempdir().unwrap();
    // `{{` in a raw body is the author's, not an unrendered template.
    let (code, text) = validate_step(
        dir.path(),
        "    file: { path: ./x, state: file, content: \"{{ not a template }}\" }\n    raw: true\n",
    );
    assert_eq!(
        code, 0,
        "a raw body was checked as if it were rendered:\n{text}"
    );

    // The same step's `creates` is rendered either way, so it is checked
    // either way.
    let (code, text) = validate_step(
        dir.path(),
        "    file: { path: ./x, state: file, content: \"{{ not a template }}\" }\n    raw: true\n    creates: \"$CACHE/x\"\n",
    );
    assert_eq!(code, EXIT_VALIDATION, "`raw` exempted a modifier:\n{text}");
    assert!(text.contains("shell variable `$CACHE`"), "{text}");
}

// §9.3: `via` is a field on the step event, so it is on every verb that
// emits one. It was pinned only under `plan --plan-no-probe --json` -- the
// stream nobody automates against. `apply --json` is the one a CI runner
// reads, and it is the one where telling two invocations of a component
// apart decides which call site a failure belongs to.
//
// And through `import`, not only `use`: §3.1 gives them the same call-site
// rule and the walk dispatches them at three separate sites
// (`src/expand.rs:477`, `:531`, `:585`), so "it works for `use`" is not an
// answer about `import`.
#[test]
fn via_is_on_the_apply_stream_and_names_an_import_call_site() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("leaf.yml"),
        // `import:` takes a plan -- a bare list of steps -- where `use:`
        // takes a component. The call site is named the same way for both.
        "- name: leaf step\n  shell: \"true\"\n  changed_when: \"false\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("mid.yml"),
        "description: mid\nsteps:\n  - name: inner import\n    import: ./leaf.yml\n",
    )
    .unwrap();
    let root = dir.path().join("root.yml");
    std::fs::write(
        &root,
        "description: root\nsteps:\n  - name: outer use\n    use: ./mid.yml\n  - name: at the root\n    shell: \"true\"\n    changed_when: \"false\"\n",
    )
    .unwrap();

    let out = run(&["apply", "--json", &root.display().to_string()]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    let steps: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|r| r["event"] == "step")
        .collect();
    assert_eq!(steps.len(), 2, "{stdout}");
    // One entry per call site, outermost first, whichever verb the site
    // used.
    assert_eq!(
        steps[0]["via"],
        serde_json::json!(["outer use", "inner import"]),
        "{stdout}"
    );
    // §9.3: absent at depth 0, where there is no call path to report --
    // absent, not an empty array, which a consumer would have to
    // special-case.
    assert!(
        steps[1]["via"].is_null(),
        "a root-level step carried a call path:\n{stdout}"
    );
}

// #32: a `use` step's `name:` was stored and never printed — `do_use` never
// reaches `finish`, so it only ever showed when the step was skipped. Two
// invocations of one component then emitted byte-identical `file`, `line`
// and `name`, and the component in between never appeared at all.
#[test]
fn a_call_site_name_reaches_the_output_and_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("leaf.yml"),
        "description: leaf\n\
         props:\n\
         \x20 what: {type: string, required: true}\n\
         steps:\n\
         \x20 - name: \"leaf step for {{ props.what }}\"\n\
         \x20   shell: \"true\"\n\
         \x20   changed_when: \"false\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("mid.yml"),
        "description: mid\n\
         steps:\n\
         \x20 - name: rust\n\
         \x20   use: ./leaf.yml\n\
         \x20   props: {what: rust}\n\
         \x20 - name: node\n\
         \x20   use: ./leaf.yml\n\
         \x20   props: {what: node}\n",
    )
    .unwrap();
    let root = dir.path().join("root.yml");
    std::fs::write(
        &root,
        "description: root\n\
         steps:\n\
         \x20 - name: Developer toolchains\n\
         \x20   use: ./mid.yml\n",
    )
    .unwrap();

    let (code, text) = plan(&[&root.display().to_string()]);
    assert_eq!(code, 0, "{text}");
    // Every call site appears, the intermediate one included.
    for label in ["Developer toolchains", "rust", "node"] {
        assert!(text.contains(label), "`{label}` is missing from:\n{text}");
    }

    let out = run(&[
        "plan",
        "--plan-no-probe",
        "--json",
        &root.display().to_string(),
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let vias: Vec<String> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|r| r["event"] == "step")
        .map(|r| r["via"].to_string())
        .collect();
    assert_eq!(
        vias,
        vec![
            "[\"Developer toolchains\",\"rust\"]",
            "[\"Developer toolchains\",\"node\"]"
        ],
        "two invocations of one component must be distinguishable:\n{stdout}"
    );
}

// The fallback, which is what the header always showed: an unnamed call site
// is the file it names, so a plan that never used `name:` on a `use` reads as
// it did before.
#[test]
fn an_unnamed_call_site_falls_back_to_its_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("lib.yml"),
        "description: lib\nsteps:\n  - name: inner\n    shell: \"true\"\n    changed_when: \"false\"\n",
    )
    .unwrap();
    let root = dir.path().join("root.yml");
    std::fs::write(&root, "description: root\nsteps:\n  - use: ./lib.yml\n").unwrap();
    let (code, text) = plan(&[&root.display().to_string()]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("lib.yml"), "{text}");
}

// #50: a deeper level of the call path was swallowed once a shallower one had
// diverged. The header loop compared index by index with no "already
// diverged" latch, and an unnamed `use` falls back to the file it names — so
// `A` and `B` each using `lib/common.yml` produce the same label at index 1,
// and the second one was skipped as unchanged. The step then read as if it
// came straight from `B`, which is the under-reporting of nesting §9.1's "one
// header per call site" rules out.
#[test]
fn a_repeated_label_under_a_new_parent_still_gets_its_header() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("lib")).unwrap();
    std::fs::write(
        dir.path().join("lib/common.yml"),
        "description: common\nsteps:\n  - name: inner\n    shell: \"true\"\n    changed_when: \"false\"\n",
    )
    .unwrap();
    // Both call the same component, both without a `name:`, so both produce
    // the label `lib/common.yml` at index 1.
    for f in ["a.yml", "b.yml"] {
        std::fs::write(
            dir.path().join(f),
            "description: mid\nsteps:\n  - use: ./lib/common.yml\n",
        )
        .unwrap();
    }
    let root = dir.path().join("root.yml");
    std::fs::write(
        &root,
        "description: root\nsteps:\n  - name: A\n    use: ./a.yml\n  - name: B\n    use: ./b.yml\n",
    )
    .unwrap();

    let (code, text) = plan(&[&root.display().to_string()]);
    assert_eq!(code, 0, "{text}");
    let headers = text.matches("common.yml").count();
    assert_eq!(
        headers, 2,
        "the second invocation of the component lost its header:\n{text}"
    );
}

// #38, the templating vocabulary. minijinja has no API for enumerating what
// it knows, so the note names what *provision* adds — the ones a reader
// cannot look up anywhere — and says plainly that the rest are minijinja's.
#[test]
fn an_unknown_filter_names_provisions_own() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.yml");
    std::fs::write(
        &path,
        "description: bad filter\nsteps:\n  - name: \"{{ 'a' | zzz_filter }}\"\n    shell: \"true\"\n    changed_when: \"false\"\n",
    )
    .unwrap();
    let out = run(&["validate", &path.display().to_string()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION), "{text}");
    assert!(text.contains("expanduser"), "{text}");
    assert!(text.contains("minijinja"), "{text}");
}

// #38: `retry: 3` is the obvious thing to write, and it answered with
// `expected a mapping, found an integer` — naming neither the key nor the
// shape it wanted.
#[test]
fn a_retry_that_is_not_a_mapping_names_the_key_and_the_shape() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rt.yml");
    std::fs::write(
        &path,
        "description: bad retry\nsteps:\n  - name: x\n    shell: \"true\"\n    retry: 3\n    changed_when: \"false\"\n",
    )
    .unwrap();
    let out = run(&["validate", &path.display().to_string()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION), "{text}");
    assert!(text.contains("`retry` is a mapping"), "{text}");
    assert!(text.contains("attempts"), "{text}");
}

// #38: the root-shape rule decides the `--strict` default and was stated in
// no help text and no error. The one moment it costs someone something is
// the one moment to say it.
#[test]
fn the_gate_diagnostic_explains_why_this_file_is_held_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plan.yml");
    std::fs::write(&path, "- name: x\n  shell: \"echo hi\"\n").unwrap();
    let out = run(&["validate", &path.display().to_string()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION), "{text}");
    assert!(text.contains("root is a list"), "{text}");
    assert!(
        text.contains("`description:`/`steps:` root is a task"),
        "{text}"
    );
}

// #38: §3.1's scope boundary explained itself nowhere at the point of
// failure — reading a value registered inside a `use` gave a bare
// `undefined variable` with no hint that the name exists one scope down and
// deliberately does not reach the caller.
#[test]
fn a_register_that_did_not_leak_out_of_a_use_says_so() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("prod.yml"),
        "description: produces\nsteps:\n  - name: compute\n    shell: \"echo 1.2.3\"\n    register: version\n    changed_when: \"false\"\n",
    )
    .unwrap();
    let path = dir.path().join("cons.yml");
    std::fs::write(
        &path,
        "description: consumes\nsteps:\n  - name: use it\n    use: ./prod.yml\n  - name: read it\n    shell: \"echo {{ version.stdout }}\"\n    changed_when: \"false\"\n",
    )
    .unwrap();
    let out = run(&["validate", &path.display().to_string()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(EXIT_VALIDATION), "{text}");
    assert!(text.contains("undefined variable `version`"), "{text}");
    assert!(text.contains("do not leak back"), "{text}");
}

// ── `json`'s validation surface (spec §6.11) ──────────────────────────────

/// §6.11 takes `file`'s rule, because it is the same choice: neither is an
/// error and both is an error positioned at the second.
#[test]
fn a_json_step_needs_exactly_one_of_content_and_src() {
    let dir = tempfile::tempdir().unwrap();

    let (code, out) = validate_step(dir.path(), "    json:\n      path: ./a.json\n");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("needs `content` or `src`"), "{out}");

    // `src` exists, so the check that fires is the one under test: §8's
    // existence check runs first and would otherwise be the only diagnostic.
    std::fs::write(dir.path().join("b.json"), "{}").unwrap();
    let (code, out) = validate_step(
        dir.path(),
        "    json:\n      path: ./a.json\n      content:\n        a: 1\n      src: ./b.json\n",
    );
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("mutually exclusive"), "{out}");
}

/// A merge declares keys, so a `content` with no keys is a validation error
/// rather than a step that writes a scalar over somebody's settings.
#[test]
fn a_json_content_that_is_not_a_mapping_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = validate_step(
        dir.path(),
        "    json:\n      path: ./a.json\n      content: \"just a string\"\n",
    );
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("not a mapping"), "{out}");
}

/// §3.2: `json.path` is a path provision reads itself, so it joins the set
/// where a surviving `$VAR` is an error before anything runs. The check is a
/// table (`src/expand.rs`), and a family missing from it fails silently.
#[test]
fn a_shell_variable_in_a_json_path_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = validate_step(
        dir.path(),
        "    json:\n      path: $CONFIG_DIR/a.json\n      content:\n        a: 1\n",
    );
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("$CONFIG_DIR"), "{out}");
}

/// §3: unknown keys in a step body are errors, and the table that decides
/// which are known is per action. `owner` and `group` are deliberately not
/// fields of `json`.
#[test]
fn a_json_step_refuses_a_key_it_does_not_have() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = validate_step(
        dir.path(),
        "    json:\n      path: ./a.json\n      content:\n        a: 1\n      owner: root\n",
    );
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("unknown key `owner`"), "{out}");
}

/// §6.11: a `src` that is not an object cannot declare keys. Caught at
/// validate, where §8 already checks the file exists.
#[test]
fn a_json_src_that_is_not_an_object_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("b.json"), "[1, 2]").unwrap();
    let (code, out) = validate_step(
        dir.path(),
        "    json:\n      path: ./a.json\n      src: ./b.json\n",
    );
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("not a JSON object"), "{out}");
}
