# provision — decisions

Short records. Each has a decision, the reason, and what would overturn it.

## D1 — Rust, single static binary

**Decision.** Rust, one crate, `cargo build --release` per target, no
runtime dependencies. Cross-compile for `x86_64-unknown-linux-musl`,
`aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.

**Why.** The machines include two Windows hosts that need a bootstrap run
from PowerShell before WSL exists. Anything needing Python, a shell, or a
runtime on the target is disqualified. Rust gives the static binary, the
process control (process groups, timeouts) and the strict types that keep
a config parser honest. Go would also do; Rust is the owner's choice for
the next stack and this is a small enough project to be a good first one.

**Overturned by.** Nothing foreseeable.

## D2 — No fleet, no daemon, no remote

**Decision.** The tool runs on the machine it converges. Period.

**Why.** The mooncake fleet layer (agentd + fleet + peers, ~25k LOC) served
three hosts. `ssh host 'cd dotfiles && provision apply x1.yml'` serves
three hosts. Remote apply is the shell's job.

**Overturned by.** Owning more machines than fit in a for-loop. Not before
ten.

## D3 — Shell is first-class, idempotency is declared

**Decision.** `shell` and `cmd` are primary actions. Their idempotency comes
from `unless`, `creates`, or `changed_when` on the step. A shell step
without a gate reports `unknown`, and `validate --strict` rejects it.

**Why.** 57% of real steps are shell today, and the real configs already
gate them with `unless_command` (39 uses) and `creates`. Pretending 64 typed
actions cover the world produced 19 actions with no diff, no reverse, no
permissions, and a `changed: true` lie on every Windows step. Making the
gate explicit and visible in output ("`unknown`") is honest; auto-detecting
change from shell is not possible.

**Overturned by.** Nothing. This is the core idea.

## D4 — Seven typed actions, chosen by state inspection need

**Decision.** `file`, `template`, `pkg`, `service` exist because a correct
`changed` verdict requires reading system state (bytes, modes, package
database, unit status), which shell-plus-gate does badly. `assert` exists
because "verify" deserves a distinct glyph. `shell` and `cmd` are the
escape hatch. Nothing else.

**Why.** The 2026-09-07 count of what the dotfiles use. Everything dropped
(`text.replace`, `git.clone`, `pkg.repo`, `wait.http`, `container.image`,
the Windows trio) had 1–5 uses and a shell-with-gate equivalent of under
five lines. See plan.md "Deferred" for the reopen conditions.

**Overturned by.** The reopen conditions, individually.

## D5 — No transactions, no rollback, no handlers

**Decision.** Fail-fast. Re-run. That is the recovery model.

**Why.** Zero uses in real configs. A rollback that cannot reverse a shell
step, which is most steps, is theater. Idempotent steps make re-run the
correct fix. `on_change` handlers (1 use) are replaced by the next step
using `register` and `when: result.changed`.

**Overturned by.** A real incident where fail-fast plus re-run destroyed
something, documented.

## D6 — Jinja2 via minijinja, strict undefined, one engine for values and conditions

**Decision.** minijinja renders string fields and `.j2` files, and evaluates
`when`/`changed_when`/`failed_when` expressions. An undefined variable is
an error at plan time.

**Why.** The 27 existing templates are Jinja2. The `when` expressions
already use Jinja syntax (`not in [0, 2]`, `and`, `==`). One engine, no
second expression language. Strict undefined because a silently empty
`{{ palette.bg }}` in a config file is the worst kind of bug.

**Overturned by.** minijinja being abandoned. Then vendor it.

## D7 — Ansible tag semantics

**Decision.** `--tags x` runs only steps carrying `x`, plus steps tagged
`always`. Untagged steps do not run. `--skip-tags` excludes. Filtering
happens after import/use expansion and before rendering.

**Why.** Mooncake's "positive filter runs everything except other-tagged
steps" (#196) surprised its own author, and filtering after platform
compilation (#191) made `plan -t` fail on excluded steps. Ansible's rule is
known and predictable; excluded steps are never rendered, so they cannot
fail.

**Overturned by.** Nothing.

## D8 — `sudo: true` only; no `become_user`, no run-as-admin

**Decision.** Root or current user. On Windows, `sudo` is a validation
error; run the tool from an elevated prompt.

**Why.** `as_user: root` is the only form in use (77 of 77). Windows
elevation from an unelevated process is a UAC dance not worth owning; the
existing bootstrap already documents "run from an Administrator PowerShell".

**Overturned by.** A step that genuinely must run as a third user.

## D9 — No state directory, no run log

**Decision.** The tool writes nothing outside what the plan declares.

**Why.** Mooncake's run log was 25k lines, 1,144 of them from its own test
suite writing into the live file. A log nobody reads that tests can corrupt
is negative value. `--json` to a file gives an audit trail on demand.

**Overturned by.** Asking "what changed last time" twice and having no
answer.

## D10 — No task runner

**Decision.** `provision` does not run project tasks. The nine repos using
`mooncake task` move to `just` or a Makefile, and the `go-quality` presets
become a shared `justfile` include or a small shell script.

**Why.** Different job, different lifecycle, different users. Coupling
them is why mooncake grew a preset registry, a module lockfile, and a
marketplace plan.

**Overturned by.** Nothing. If a task runner is wanted, it is a separate
20-line justfile, not a feature here.

## D11 — Plan is best-effort and says so

**Decision.** `plan` probes real state where an action can (`file`,
`template`, `pkg`, `service`, `unless`, `creates`, `assert`) and reports
`would run` for the rest. `--plan-no-probe` disables probing for CI. The
output distinguishes `would change`, `would run`, and `would run (unprobed)`.

**Why.** A dry-run that claims certainty it does not have trains the
operator to ignore it. Three distinct words cost nothing.

**Overturned by.** Nothing.

## D12 — Components with declared props, not includes with globals

**Decision.** Keep `use` + `props` from mooncake. A component declares its
inputs with type, default, and required. Globals are visible read-only;
`props.*` is a separate namespace.

**Why.** Three components already declare props and 21 call sites use them.
The alternative, components reading undeclared globals, is the "two
never-merged namespaces" confusion mooncake filed against itself (#183).
Declaring inputs is the fix.

**Overturned by.** Nothing.

## D13 — saphyr for YAML, not serde_yaml

**Decision.** Parse plan files with `saphyr`'s `MarkedYaml` into a spanned
tree, and extract typed values from it with explicit accessors. No
`serde_yaml`, no `#[derive(Deserialize)]` for the config model.

**Why.** The Phase 0 gate is "100% of validation errors carry file:line".
`serde_yaml::Value` carries no positions, so every error raised after
deserialization — two action keys in one step, an unknown prop, a `when` that
is not a boolean, an undefined variable in a field — would have no line to
point at. `MarkedYaml` gives every node a span, map keys included, so the
error can name the exact key that is wrong and suggest the correction.

Explicit accessors also beat derive here for a second reason: fields cannot be
typed at parse time. `names: "{{ apps }}"` is a string until it is rendered,
and only then a list. A derive-based model would have to type it twice.

plan.md already listed `yaml-rust2`/`serde-saphyr` as the sanctioned fallback
if serde_yaml did not hold; this is that fallback being taken, for a concrete
gate rather than a hypothetical.

**Cost.** `saphyr` is pre-1.0 (0.0.12) where `serde_yaml` 0.9 is archived but
frozen. Pre-1.0 churn is the risk taken. The surface used is small — load a
document, walk nodes, read spans — and it is confined to `src/yaml.rs`, which
is the only file that would change if it has to be swapped or vendored.

**Overturned by.** saphyr breaking its API twice in a way that costs more than
vendoring `src/yaml.rs`'s dependency surface. Then vendor it.

## D14 — plan binds registers to a placeholder and says "unprobed"

**Decision.** At plan time, `register: r` binds `r` to a real-shaped result
(`rc: 0`, empty stdout/stderr, `changed: false`, `skipped: false`). A later
step whose `when` reads `r` is reported `would run (unprobed)`, never
`skipped`.

**Why.** Strict undefined (D6) and `plan` evaluating `when` (§8) collide on
the first `when: r.changed` in a plan: the registering step has not run, so
the name does not exist, so plan fails on a config that applies fine. The
alternatives are worse — leaving the name undefined breaks the plan, and
evaluating the placeholder as truth would report a step `skipped` that apply
will actually run. D11 already says plan is best-effort and must say so;
`unprobed` is the word it already has for exactly this.

**Overturned by.** Nothing. It is D11 applied to one more case.

## D15 — `unknown` counts as a change for `plan`'s exit code

**Decision.** `plan` exits 2 for any step that is not `ok` or `skipped` —
`would change`, `would run`, `unknown` and `would run (unprobed)` alike.
`--plan-no-probe` probes nothing, claims nothing, and exits 0 unless
validation failed.

**Why.** Spec §8 said "0 if nothing would change, 2 if something would" and
never said which side `unknown` falls on. It has to be 2. An ungated `shell`
step is `unknown` forever, so counting it as 0 makes a converged machine and
a machine full of ungated shell steps report identically — and `plan` stops
being a drift check on exactly the steps most likely to drift. Counting it as
2 gives `--strict` (D3) something to be *for*: driving the unknown count to
zero is what makes exit 0 mean something. plan.md already measures it.

`unprobed` counts for the same reason `unknown` does. In a probed plan it
means an action whose runner does not exist yet, or a root gate this run could
not reach — both are provision admitting it does not know, and neither is
"nothing to do".

`--plan-no-probe` is the exception because it is the one mode that inspects
nothing. Exiting 2 there would be a claim it did not earn, and dotfiles CI
runs it on every machine plan from a machine that is none of them.

**Overturned by.** Nothing. If exit 2 proves noisy, the fix is `--strict` on
the plan, not a quieter exit code.
