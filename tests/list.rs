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

// Spec §8: the third granularity — what a *builtin* takes. A plan's author
// asking which keys `git` accepts has no file to open, and used to have two
// options: read the source, or write a key and let `validate` reject it
// (issue #7).
#[test]
fn a_builtin_action_lists_its_fields() {
    let (code, out) = run(&["list", "git"]);
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("action", out);
}

// An action whose body is not a mapping has no fields to list, and an empty
// listing would read as "it takes nothing". `cmd` takes an argv.
#[test]
fn an_action_with_no_body_mapping_says_what_its_body_is() {
    let (code, out) = run(&["list", "cmd"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("a list of arguments"), "{out}");
    assert!(!out.contains("fields:"), "{out}");
}

// Spec §8: a path that exists wins, so a file named like an action is that
// file. Only a bare name matching nothing is looked up as a builtin.
#[test]
fn a_path_that_exists_wins_over_an_action_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("git");
    std::fs::write(&path, "description: not the builtin\nsteps: []\n").unwrap();
    let (code, out) = run(&["list", path.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("not the builtin"), "{out}");
    assert!(!out.contains("the URL to clone from"), "{out}");
}

// The error has to name all three forms, because the argument that got here
// could have been meant as any of them.
#[test]
fn a_name_that_is_neither_a_path_nor_an_action_names_both_forms() {
    let (code, out) = run(&["list", "gti"]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("builtin action"), "{out}");
}

// The claim §8 makes about this listing: the names it prints are the names
// `validate` accepts, because both read one table. A key added to an action's
// parse and not to the table is a key the listing hides; the reverse is a
// help text that lies.
#[test]
fn every_action_is_listed_and_every_listing_is_an_action() {
    // The action keys, as `validate`'s own error prints them on an unknown
    // step key — the one place the set is visible from outside the crate.
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("p.yml");
    std::fs::write(&plan, "- name: A step\n  nosuchaction: x\n").unwrap();
    let (code, out) = run(&["validate", plan.to_str().unwrap()]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");

    for action in [
        "shell", "cmd", "file", "template", "pkg", "service", "assert", "git", "defaults",
        "download",
    ] {
        assert!(
            out.contains(action),
            "`{action}` is not among the keys validate knows:\n{out}"
        );
        let (code, listed) = run(&["list", action]);
        assert_eq!(code, 0, "`list {action}` failed:\n{listed}");
        assert!(
            listed.contains(action),
            "`list {action}` named nothing:\n{listed}"
        );
    }
}

// Both layouts in one directory. `flat.yml` is a component named by its
// stem; `palette/index.yml` is a component named by its *directory*, which
// is the layout a component shipping its own `templates/` has to use.
//
// The two negatives are the point of reading one level and matching
// `index.yml` by name rather than sweeping: `palette/vars/data.yml` is a
// component's data and `no_index/` holds no component at all, and a sweep
// would name both `(not a component)`.
#[test]
fn a_directory_lists_a_name_index_component_by_its_directory() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/list_nested");
    let (code, out) = run(&["list", &dir.display().to_string()]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("flat"), "{out}");
    assert!(out.contains("palette"), "{out}");
    assert!(
        out.contains("a component that ships its own templates"),
        "the directory's `index.yml` supplies the description: {out}"
    );
    assert!(
        !out.contains("index"),
        "named by the directory, not the stem: {out}"
    );
    assert!(
        !out.contains("data"),
        "a component's data is not a component: {out}"
    );
    assert!(
        !out.contains("no_index"),
        "a directory with no index.yml is not one: {out}"
    );
}

// D25: verbs that answer return a document. One call gives a writer of plan
// files the whole vocabulary -- the ten actions *and* the step envelope,
// which is the half nothing described before: `list <action>` covered the
// actions, and the sixteen keys every step may carry were a bare name list
// inside the parser.
#[test]
fn list_json_with_no_path_dumps_the_whole_vocabulary() {
    let (code, out) = run(&["list", "--json"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(&out).expect("one document");

    let actions = doc["actions"].as_array().expect("actions");
    assert_eq!(actions.len(), 10, "spec §6 has ten actions");
    let git = actions
        .iter()
        .find(|a| a["name"] == "git")
        .expect("git is a builtin");
    let fields: Vec<&str> = git["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["repo", "dest", "ref", "depth"]);
    assert_eq!(git["fields"][0]["need"], "required");

    // The envelope, which is the point of the issue.
    let mods = doc["modifiers"].as_array().expect("modifiers");
    let names: Vec<&str> = mods.iter().map(|m| m["name"].as_str().unwrap()).collect();
    for k in [
        "when",
        "register",
        "creates",
        "unless",
        "changed_when",
        "tags",
    ] {
        assert!(names.contains(&k), "{k} missing from {names:?}");
    }
    // `applies` is the word a reader needs most: four of these are legal on
    // one kind of step only.
    let props = mods.iter().find(|m| m["name"] == "props").unwrap();
    assert_eq!(props["applies"], "`use` only");

    assert_eq!(
        doc["structural"].as_array().unwrap().len(),
        4,
        "vars, vars_file, import, use"
    );
    assert!(doc["version"].is_string());
}

// A single action as a document, the same table the text form prints.
#[test]
fn list_json_for_one_action_is_that_action() {
    let (code, out) = run(&["list", "git", "--json"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(&out).expect("one document");
    assert_eq!(doc["name"], "git");
    assert!(
        doc["summary"].as_str().unwrap().contains("checkout"),
        "{doc}"
    );
}

// `list` with neither a path nor `--json` has nothing to answer about.
#[test]
fn list_with_no_argument_at_all_is_a_usage_error() {
    let (code, out) = run(&["list"]);
    assert_eq!(code, EXIT_VALIDATION, "{out}");
    assert!(out.contains("needs a path, or `--json`"), "{out}");
}

// The listing and the validator read one table (`modifier_keys` is
// `MODIFIERS`), so this cannot fail by construction on the *names* — it is a
// smoke test that the table is wired to the parser at all, not evidence that
// the two agree. It does not assert "the other direction"; an earlier comment
// here claimed it did, which is how the table's prose drifted for months
// without a failing test.
//
// The claims that can be wrong are in `applies` and `note`, and those are
// what `the_dumps_prose_about_a_modifier_is_true` below actually checks.
#[test]
fn every_modifier_the_dump_names_is_one_the_parser_knows() {
    let (_, out) = run(&["list", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    let dir = tempfile::tempdir().unwrap();

    for m in doc["modifiers"].as_array().unwrap() {
        let name = m["name"].as_str().unwrap();
        let plan = dir.path().join(format!("{name}.yml"));
        // A body that is wrong for most of them — the point is only that the
        // key itself is recognised, not that the value type fits.
        std::fs::write(
            &plan,
            format!("- name: a step\n  shell: \"true\"\n  changed_when: \"false\"\n  {name}: x\n"),
        )
        .unwrap();
        let (_, out) = run(&["validate", &plan.display().to_string()]);
        assert!(
            !out.contains("unknown key"),
            "`{name}` is in `list --json` but the parser does not know it: {out}"
        );
    }
}

// D25 made `list --json` a contract an agent reads, and the contract drifted
// within hours: the `sudo` row said "not templated; true or false as a
// literal" on the day templating landed, and nothing failed. Prose nothing
// can falsify is worse than no prose, because a consumer cannot tell which
// fields are load-bearing.
//
// So this asserts the dump against the binary, for the row that drifted.
#[test]
fn the_dumps_prose_about_a_modifier_is_true() {
    let (_, out) = run(&["list", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    let sudo = doc["modifiers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "sudo")
        .expect("sudo is a documented modifier");
    let note = sudo["note"].as_str().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, body: &str| {
        let p = dir.path().join(name);
        std::fs::write(&p, body).unwrap();
        p.display().to_string()
    };

    // Claim 1: "templated".
    assert!(
        note.contains("templated"),
        "the note no longer says: {note}"
    );
    let templated = write(
        "t.yml",
        "- vars: {root: false}\n- name: a\n  shell: \"true\"\n  changed_when: \"false\"\n  sudo: \"{{ root }}\"\n",
    );
    let (code, text) = run(&["validate", &templated]);
    assert_eq!(code, 0, "the note claims templated, but: {text}");

    // Claim 2: "a sole expression must yield a bool".
    let wrongtype = write(
        "w.yml",
        "- vars: {root: \"yes\"}\n- name: a\n  shell: \"true\"\n  changed_when: \"false\"\n  sudo: \"{{ root }}\"\n",
    );
    let (code, text) = run(&["validate", &wrongtype]);
    assert_eq!(
        code, EXIT_VALIDATION,
        "a string was accepted as sudo: {text}"
    );

    // Claim 3: "it cannot read a register".
    assert!(note.contains("cannot read a register"), "{note}");
    let reg = write(
        "r.yml",
        "- {name: a, shell: \"true\", changed_when: \"false\", register: r}\n\
         - {name: b, shell: \"true\", changed_when: \"false\", sudo: \"{{ r.changed }}\"}\n",
    );
    let (code, text) = run(&["validate", &reg]);
    assert_eq!(
        code, EXIT_VALIDATION,
        "a register-derived sudo was accepted: {text}"
    );
    assert!(text.contains("cannot read a register"), "{text}");
}
