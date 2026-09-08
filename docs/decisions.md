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

**Amended and confirmed by the owner 2026-09-08.** Windows ships as
`x86_64-pc-windows-gnu`, not `-msvc`, and no cross-compilation happens for
Linux or macOS at all. Every machine in the fleet builds its own native
binary; only Windows, which has no toolchain in this fleet, is
cross-compiled — from Linux, with the mingw-w64 that is already installed.
`-msvc` needs `xwin` and raises a question about redistributing Microsoft's
SDK headers that nothing here is asking us to answer. The resulting `.exe`
is 2.6 MB and imports only system DLLs — `kernel32`, `ntdll`, `msvcrt`,
`combase`, `shell32`, `userenv`, `bcryptprimitives` — so it is as
self-contained as the musl target would have been. musl and
`aarch64-apple-darwin` are dropped until something consumes them: the Linux
and macOS machines each have a Rust toolchain, and a cross-built artifact
nobody installs is a build we would maintain for nothing.

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

**Amended by the owner 2026-09-08. See D17.** The reason above was about
the registry, the lockfile and the marketplace — a distribution problem —
and it was written as if reusing the step executor had caused them. It had
not. The step core is one thing whether the steps declare state or run a
command, and provision already ran a tagged step list as a task before
anyone called it one. What D10 keeps out is the registry: no remote `use`,
no fetch, no cache, no lockfile. What it no longer keeps out is running a
component as a task, which D17 specifies.

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

## D16 — the phase 2 action semantics

**Decided by review 2026-09-08, confirmed by the owner the same day.**
Everything under this heading was settled by the reviewing session while the
owner was away; it is gathered here rather than scattered so the whole block
could be ruled on in one place, which it was. It stands. Three of the original decisions were amended
before any code was written, each because a measurement said so; those are
marked where they appear.

**Reading state obeys sudo.** A `file` step with `sudo: true` probes the
target as root. The alternative — probing as the user and guessing on failure
— rewrites files nobody could compare, and `/etc/sudoers.d/*` at `0440
root:root` is not a corner case: all 18 sudo+`file` steps in the fleet write
into root-owned directories. Without sudo an unreadable target fails the step
and says `sudo: true` is the fix. Under plan with no reachable root it is
unprobed, exactly like a gate.

**No touch semantics.** `state: file` needs `content` or `src`, and exactly
one of them. Checked against the fleet first: zero steps rely on `state: file`
alone, so nothing breaks. An empty file is `content: ""`, said out loud.

**Links carry no mode.** `src` is stored as written, never canonicalised.
Replacing a regular file or directory at `path` needs `force: true`. Setting
`mode`, `owner` or `group` on a link is a validation error — checked first:
both `link` steps in the fleet set none of them.

**The sudo write path uses the user's temp directory** (amended). The brief
said "temp file next to dest"; `/etc/sudoers.d` is `drwxr-xr-x root root`, so
the user cannot create it there, and being unable to write there is the whole
reason the step said sudo. The same-directory rule exists so the *non-sudo*
path can rename atomically within one filesystem; the sudo path uses
`install`, which copies, so it is free to stage anywhere the user can write.

**Modes are deterministic.** A new file with no `mode` is `0644` and a new
directory `0755`, umask ignored. An existing target keeps what it has. A
converged machine should not depend on the shell that launched provision.

**Package queries carry versions** (amended). The brief's queries returned
names only, which makes `latest`'s "changed when the version differs"
unanswerable. They now return version and status, and apt's is filtered to
`install ok installed` — plain `dpkg-query -W` lists removed-but-config
packages, which would read as present and make `present` a silent no-op on a
package that is not there.

**Default manager order** is pacman, apt, brew, winget. `yay` is never
default: reaching the AUR is a decision a plan should have to write down.
`sudo: true` with brew or with yay is a validation error; both refuse to run
as root.

**`update_cache` never runs under plan.** A dry run that mutates the package
database is not a dry run.

**`would change` is its own verdict**, sharing `changed`'s glyph and color,
with the diff underneath. `--no-diff` exists on `apply` too, where it does
nothing, so a script can pass the same arguments to both.

**Overturned by.** The owner, in one pass over this section.

## D17 — One executor: tasks are components, `run` judges by exit code, distribution is provisioning's job

**Decided by the owner 2026-09-08**, after review showed that provisioning,
repo tasks and moongit CI steps share everything but three things: how a run
is entered, what a step's verdict means, and how a shared step list reaches
the machine. Each gets the smallest answer that closes it.

**A task is a component file.** A component already has typed props with
defaults, `required` and `description`, validated before anything runs, and
a step list. That is a task with its arguments declared. `provision run
tasks/deploy.yml --prop web_dir=~/x` runs it as the root, the CLI filling
its props; `provision run tasks/` lists each file with its `description`.
One file per task and the filesystem is the registry — no `tasks:` mapping,
so "a plan is a list, a component is a mapping" still holds. The only format
change is an optional `description:` at the component root. The same
subcommand takes `--step '<yaml>'` and runs one step from a string, which is
the contract moongit's runner needs so its CI image can drop mooncake.

**Under `run`, an ungated `shell` or `cmd` that exits 0 is `ok`.** In a task
the contract of a step is its exit code, and there is no state for provision
to be unsure about. `unknown` keeps its meaning under `apply` and `plan`
exactly as D15 says. Every gate and modifier keeps working under `run` —
`creates` skips a build whose artifact exists, `register` and `when` chain
steps — so what changes is one default verdict, not the model. There is no
`plan` for `run`: a task list has nothing to probe.

**Shared step lists are checked out by the machine plan, not fetched by
provision.** `use` takes a path, renders templates and expands `~`, so a task
file says `use: "{{ tools_dir }}/go-quality/ci.yml"` and the dotfiles
component that owns `tools_dir` clones the repo and checks out a pinned tag,
`creates`-gated and `unless`-gated like any other step. The version pin is a
dotfiles variable, one place for the fleet; a bump is a provisioning change;
offline works because the checkout is on disk. This is the module system
replaced by one component and one variable. A repo that needs a different
version than the fleet clones its own and passes `--var tools_dir=`.

**Why.** The alternative was to keep D10 as written and send nine repos and
the fleet to `just`, adding a dependency to every machine to run step lists
provision could already run. The executor was never the thing that grew
mooncake; the fetch-and-pin layer was, and that layer stays out.

**Consequences.** The 2026-09-08 ruling that `just` goes in the three
package lists was made under D10, and D17 removes its reason; the lists
already carry it (dotfiles `f68a784`), and whether it stays once the
justfile goes is the owner's call, not a gate. The dotfiles `justfile` is
interim and becomes `tasks/` once `run` lands. Spec §2, §3.2, §6.1 and §8;
plan.md phase 5; migration.md §6.

**First consumer.** provision's own repo, 2026-09-08: the `justfile` is
gone, `tasks/` holds one component per task, and the quality gate is
rust-quality's, `use`d by path from a pinned checkout. Dogfooding it found
the two things the machine plans never would have — a component had no way to
name its own directory, and a task inherited the wrong working directory.

**Overturned by.** A second consumer of `run` that needs something a
component cannot say — task dependencies, positional arguments, a registry.
Any of those is the road back to mooncake, and the answer is still no.
