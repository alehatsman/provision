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

**Amended by the owner 2026-10-12. See D24.** Both dropped targets come back,
joined by `aarch64-linux-android`, because the condition this paragraph set is
met: a phone running Termux and a CI image both install a binary and neither
should carry a Rust toolchain to get one. The machines that have toolchains
still build their own; the releases are for the ones that do not.

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
without a gate is rejected by `validate --strict`.

**Why.** 57% of real steps are shell today, and the real configs already
gate them with `unless_command` (39 uses) and `creates`. Pretending 64 typed
actions cover the world produced 19 actions with no diff, no reverse, no
permissions, and a `changed: true` lie on every Windows step. Making the
gate explicit is honest; auto-detecting change from shell is not possible.

**Amended 2026-09-12 (D20).** An ungated step used to *report* `unknown` as
well as being rejected by `--strict`. The verdict is gone; the rejection
stays. D3's claim was that idempotency must be declared rather than guessed,
and that is untouched — provision still never infers change from a shell
step. What changed is the answer for a step that declares no state at all,
where `unknown` was answering a question the step had not asked.

**Overturned by.** Nothing. The core idea — declared, never guessed — stands.

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

**Amended by the owner 2026-09-08, phase 6: ten, not seven.** Three reopen
conditions fired or were judged fired on the migrated fleet, and the owner
named the three actions he wanted back from mooncake's catalogue after
review held every family against the rule above:

- `git`: four shell clones in dotfiles plus the sixty-line clone-and-pin
  block copied into two `tasks/tools.yml` files (provision, teleport) — six
  against a threshold of three. The state is "HEAD is this ref", and the
  tag/branch/annotated-tag distinctions that block got wrong twice in one
  afternoon belong in one place.
- `download`: twelve `curl -o` steps, every one gated by `creates` and none
  by content. The shell reads "the file exists" well and "the file is the
  right one" never; a checksum is state inspection.
- `defaults`: twenty `defaults write` lines in one ungated shell step on the
  mac plans, `unknown` on every apply. Reading the current value back is the
  whole verdict.

The rule stands unchanged: state inspection the shell does badly. Every
other mooncake family was held against it with the same fleet counts and
stays out: the `os_*` server actions (zero to one use), text replacement
(five `sed -i`, under threshold), containers (`docker image inspect` is a
one-line `unless`), the observe/wait family (D2), the data actions (a
`register` and a one-liner). Spec §6.8–6.10.

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

**Overturned by.** minijinja being abandoned. Then vendor it — but priced,
not assumed.

**The exit ramp, measured 2026-10-15** (issue #5 asked for this to be a tested
claim rather than a sentence). It is not D13's situation and should not be
read as it. `minijinja` is named in **15 files**, and the reason is
`minijinja::Value`: it is this project's value type by adoption. Scope entries,
facts, props, register results and every action's rendered field are all
`Value`, so "vendor minijinja" is not a swap behind one module — it is
vendoring a template engine *and* owning the type system the whole codebase
passes around.

What the ramp actually is, in order: a `Value` newtype of our own at the scope
boundary first, which is the change that makes the engine replaceable at all,
and only then a vendored or substituted engine behind it. That is a real piece
of work and nobody should discover its size on the day minijinja is archived.
It is not being done now — minijinja is maintained, 2.x, and widely used, and
doing it today would spend the week to buy nothing. Writing down what it costs
is the part that was missing.

**Why the smaller fence was not built instead.** One engine for values and
conditions is this decision, and the single shared `Value` is what makes it
one engine. A newtype today would add a conversion at every field render to
protect against an event that has not happened.

**Amended 2026-10-15, on where the variable's name comes from.** §3.4
promises the name of the undefined variable, and minijinja cannot supply it:
every undefined is `Error::from(ErrorKind::UndefinedError)` with no detail
and a fixed "undefined value" display. The name is recovered instead by
static analysis -- asking the compiled template or expression which names it
reads, then walking each dotted path against the context and reporting the
shortest prefix that does not resolve. `env.MYCACHE` when `env` is bound,
plain `palette` when `palette` is not, because naming a child of something
that does not exist invents a level the plan never had.

Two limits worth writing down so they are not filed as bugs twice. **A
subscript cannot be named**: `{{ t['nope'] }}` leaves the analysis holding
`t`, which is defined, so the message stays minijinja's. That is the correct
answer under D11 and D14, and there is a test pinning it. And **strict
undefined fires only when a value is used** -- string-converted, iterated, or
looked up another level -- so a field that is one expression and nothing else
used to return a successful `undefined`. `render_field` now raises the error
minijinja would have raised had the value been used, which is what makes a
bad `props:` value name the typo instead of the prop.

## D7 — Ansible tag semantics

**Decision.** `--tags x` runs only steps carrying `x`, plus steps tagged
`always`. Untagged steps do not run. `--skip-tags` excludes. Filtering
happens after import/use expansion and before rendering.

**Why.** Mooncake's "positive filter runs everything except other-tagged
steps" (#196) surprised its own author, and filtering after platform
compilation (#191) made `plan -t` fail on excluded steps. Ansible's rule is
known and predictable; excluded steps are never rendered, so they cannot
fail.

**Amended 2026-10-15, on an unknown filter value.** Ansible accepts a tag no
step carries and runs nothing. Provision does not: a `--tags` or `--skip-tags`
value that **no step in the file declares** is exit 3, named per value, with
the plan's tags in the note. A value that is declared but selects nothing on
this host is *not* an error.

That distinction is the whole of the rule and the first version did not have
it. It asked "did this match anything in the run", which is a question about
the machine, and so refused `--tags pacman` on a Mac where every pacman step
is correctly gated to Linux — the ordinary state of a cross-platform plan.
Worse, the tag set was collected by the walk, so the answer depended on the
root's shape: visible on a plan root, invisible on a task root, for the same
bytes. Whether a tag exists is a property of the file and must have one
answer. The walk now descends into gated-off subtrees for that purpose (D27),
and stands down entirely when it could not finish looking — a deadline, an
interrupt, or an import path it could not render — because a false refusal of
a tag that is really there is worse than the silence this check replaced. The amendment is not a style preference — `apply drift.yml
--tags handlr` reported `2 steps · 2 skipped`, exit 0, which is an apply that
touched nothing reporting success, and the inverse typo ran the whole plan.
Neither is recoverable by reading the output, and `break-glass.md` §3b's
second recovery move is an operator typing `--skip-tags <tag>` from memory
while something is broken. D7 adopted Ansible's semantics because they are
*predictable*; silently accepting a typo is the one place they are not.

The note carries the plan's tag set because there is no other way to ask for
it — `list` on a plan refuses (D17), and adding a verb to enumerate tags would
be a new noun for a question that only ever gets asked at the moment the
answer is already needed.

**What this costs, stated plainly.** A pipeline passing a tag that a plan has
since dropped now fails instead of running nothing — louder, and in CI that is
the point, but it is a new way for a green job to go red. `always` is exempt on
`--skip-tags` only, so `--skip-tags always` is still accepted on a plan with
no `always` step; on `--tags` it is checked like any other value, because
selecting a keyword no step carries can only be a mistake. And the check stands down when the walk already found
problems: a typo'd tag on a plan that also fails to load is not reported, on
the grounds that "no step is tagged `x`" would then be a guess.

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

**Amended 2026-10-15.** Saying what the audit trail *is*, because the
question kept coming back from careful readers who took "no run log" to mean
the answer was unavailable rather than unwritten. `provision apply plan.yml
--json > run.jsonl` **is** the run log: every step's status, exit code,
stdout, stderr, duration and `file:line`, plus the summary, in a schema that
is stable by construction because the words come from `Status::key` and never
from the terminal wording (§9.3). It is a better record than a built-in one
would be — the operator chooses when to keep it, tests cannot corrupt a file
the tool never opens, and retention is the runner's by D19. The condition
above reopens only for a question the stream does not already answer. A
stateless reader re-asking it every session is not that: it is a consumer
that did not keep the file, and the fix is to keep the file.

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

**Confinement verified 2026-10-15** (issue #5). The claim above holds as
written: `saphyr` is named in `src/yaml.rs` and in one line of a `template.rs`
test, nowhere else, and the surface is four items — `LoadableYamlNode`,
`MarkedYaml`, `Scalar`, `YamlData`. Nothing downstream of `yaml.rs` knows the
parser exists; the walk receives typed accessors and spans. This ramp is cheap
and stays cheap as long as that stays true, which is worth re-checking the day
a second file imports `saphyr`. Contrast D6, where the same sentence costs
something very different.

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

**Amended 2026-10-12 (#16), owner's call.** The placeholder only made
sense to a `when`. A field that transforms a register, such as
`{{ crc.stdout | trim | int }}`, fails on the empty string. That failed
`validate`, `plan` and `apply`'s pre-walk, so a plan that applies fine
could not be applied at all. The rule now covers every template, not just
`when`. Under `validate` and `plan`, a template that reads a register in
reach cannot fail, and `plan` reports its step unprobed. A `vars` value
computed from a register joins the register set, so it propagates. The
other option was a typed placeholder, `0` where a number is wanted. It was
rejected because it is magic, and because it puts back the false verdict
this decision exists to prevent: `when: port > 0` would answer for a port
nobody computed.

## D15 — `unknown` counts as a change for `plan`'s exit code

**Decision.** `plan` exits 2 for any step that is not `ok` or `skipped` —
`would change`, `would run`, `unknown` and `would run (unprobed)` alike.
`--plan-no-probe` probes nothing, claims nothing, and exits 0 unless
validation failed.

**Why.** Spec §8 said "0 if nothing would change, 2 if something would" and
never said which side `unknown` falls on. It has to be 2. A step provision
cannot judge is not a step it may call converged, so counting it as 0 would
make a converged machine and a machine full of unjudgeable steps report
identically — and `plan` stops being a drift check on exactly the steps most
likely to drift.

**Amended 2026-09-12 (D20).** The original argument was carried by the
ungated `shell` step, which was `unknown` forever, and it read "driving the
unknown count to zero is what `--strict` is for". An ungated shell step now
reports `would run` under plan, which is in the same exit-2 bucket, so no
exit code moves. What changes is the *source* of `unknown`: it is now
plan-only and comes from `pkg` `latest`, a `git` branch ref, and `defaults`
off macOS — three cases where plan declines a network call or a mutation and
apply answers by doing the work. `--strict` is still what drives ungated
shell steps to zero; it just does it as a validation error rather than
through an exit code.

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

## D17 — One executor: tasks are components, distribution is provisioning's job

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
rust-quality's own presets, `use`d by file path from a checkout `tasks/tools.yml`
clones and pins. That is the whole of the distribution story working as
written: no registry, no fetch at gate time, a version bump is one line, and
a preset is a component like any other. It took a change on the rust-quality
side too -- its presets carried `name:` and `version:` root keys, which a
component may not have, and the tag is the version anyway.

Dogfooding it found four things the machine plans never would have: a
component had no way to name its own directory, a task inherited the wrong
working directory, an `apt-get install` that reports success can install
nothing, and `component_dir` was empty for a component named by a bare
filename.

**Amended by the owner 2026-09-08, phase 5b: no `run` verb.** Building
phase 5 and then dogfooding it showed that `run` made the same file mean
different things depending on the verb — its working directory, its ungated
verdict, whether `plan` applied to it. The owner's reading: apply and run
should be one thing. So: `validate`, `plan` and `apply` each take a plan or a
component as the root, with `--prop` for the component; the working
directory is one rule for every command (§4, measured safe against the
fleet); an ungated step is `unknown` under every command, and a task step
whose exit code is its whole contract says `changed_when: false` — the
declaration a plan would make, not a verb's side effect; the directory
listing is its own verb, `list`, which does nothing else; and `--step` is
gone, because provision already has the per-step event stream a CI runner
needs — the runner writes the job to a file and runs `provision apply
job.yml --json`, one process per job, with `rc`, `stdout` and `stderr` on
every step event. Of the three paragraphs above, the first and third stand
with `apply` read for `run` and `list` for the listing; the second is
replaced by this one. "One executor" now also means one meaning per file.

**Overturned by.** A second consumer that needs something a
component cannot say — task dependencies, positional arguments, a registry.
Any of those is the road back to mooncake, and the answer is still no.

## D18 — a `file` src after a command is judged when the step runs

**Decision.** A `file` step's `src` is checked for existence before anything
runs, as §8 says — unless a `shell` or `cmd` step comes earlier in the same
walk. Then the check belongs to the walk that executes: `validate` says
nothing about it, `plan` reports the step `would run (unprobed)`, and `apply`
reaches it after the command has run and either copies the source or fails
naming it. Nothing else defers: `import`, `use`, `vars_file` and a `template`
step's `src` are read by the walk itself and must be there, and a `src` with
no command before it is missing now and missing later.

**Why.** Build-then-install cannot run from a clean checkout otherwise
(issue #3). `shell: cargo build --release` followed by a `file` step copying
the binary out of `target/` is rejected on step two for a path step one
produces — and it is invisible in any repo that has been built once, which is
every repo where such a task was written, including this one's own
`tasks/install.yml`. The rule draws the line where the tool's knowledge
actually ends: every other step declares what it writes, and `shell` and `cmd`
are exactly the two that do not, so after one of them provision has no basis
for calling a path missing. The alternative rules were worse. Asking whether
some earlier step "could plausibly" create this path is a guess dressed as an
answer. A per-step escape hatch puts the burden on every author of a shape
this common. Dropping the check entirely gives up the fail-fast that makes a
typo in `src` cheap, which is the half of the behaviour worth keeping.

**The cost, stated.** A typo'd `src` in a plan that builds something is now
caught when the step is reached rather than before the run — after the build
has already happened. That is the price of not guessing, and the step still
fails before it writes anything.

**Overturned by.** Steps declaring their outputs well enough to match a `src`
against them — then the rule can name the producing step instead of any
command at all. Not worth building for `shell`, which would still declare
nothing.

## D19 — provision owns one job's clock and its own death; the runner owns the rest

**Decision.** As a CI execution engine provision takes on exactly three
obligations beyond what it already had: die cleanly when told to (SIGTERM,
exit 143), bound the whole walk when asked to (`--deadline`, exit 124), and
say what a component takes (`list <component.yml>`). Nothing else moves in
from the runner's side of the line.

The line, written down so it can be pointed at:

| moongit owns | provision owns |
|---|---|
| checkout, environment, secret delivery | one job, one process, one machine |
| scheduling, queueing, concurrency across jobs | ordering, fail-fast, timeouts, the interrupt |
| log storage, retention, UI | the `--json` stream, one object per step |
| cancellation policy | dying cleanly when cancelled |
| CI facts — commit, branch, PR — passed as `--var` | machine facts only (§5) |

**Why.** D17 settled that provisioning, tasks and CI share one executor, and
reading provision as moongit's runner then found three things the fleet never
would. SIGTERM was unhandled — `exec/process.rs` caught SIGINT only, so a
cancelled job killed provision where it stood and orphaned the step's process
group, defeating the whole reason every child gets its own group. `timeout`
bounds a step and nothing bounded a run, so a twenty-step job had a
three-hour worst case and the runner's only recourse was a SIGKILL from
outside, which loses the summary event that is the thing it came for. And
`list <dir>/` said what could be run without saying what any of it took,
which is fine for the author and useless for a caller.

Each is a flag or a signal handler. None is a new noun. That is the test this
decision exists to impose: **the noun budget is closed** — plan, component,
step, fact, and no fifth. A CI request that cannot be spelled as a step
modifier, an action, or a flag on the existing five verbs belongs in moongit.
`job`, `pipeline`, `stage`, `matrix`, `artifact`, `cache` and `needs:` are
all fifth nouns, and each one is the road back to mooncake.

**What this deliberately does not buy.** Parallel steps: sequential execution
is a semantic guarantee — `register` chaining and "step N+1 sees step N's
effects" both rest on it — and a runner that wants concurrency splits the job
into two `provision apply` invocations. `use` deduplication: running a
component twice is a performance question, not a correctness one, the fix
would be a graph, and a graph is `needs:`. Secret masking: provision prints
what a step printed, and a promise to redact that cannot survive a step
base64-ing a token is worse than no promise — the runner masks at the
log-write boundary, where it knows what the secrets are. Spec §6.3's
"secrets are the operator's problem" is unchanged and is still the position.

**Overturned by.** Parallelism, when a real gate's wall time is dominated by
independent steps *and* splitting it runner-side is measured worse. Masking,
when a real secret reaches a real log through provision and the runner could
not have caught it. Neither has happened.

**Reopen check: 2026-12-12.** Everything above was built against a runner
that has not yet run it. moongit's runner switching to `provision apply
job.yml --json` — reading the stream, cancelling with SIGTERM, passing
`--deadline` — is the owner's, and was open-ended. On
2026-12-12, look: if no moongit job has run through that contract in
production, this decision and D17's CI-runner half are **unvalidated**, not
settled, and are read that way — the next CI request is argued from scratch
rather than against the line drawn here. D17's task half is not in question;
this repo's own `tasks/` exercise it on every gate run. Set 2026-09-13 from
issue #5.

**Partial credit from 2026-10-12, D24.** This repo's own gate runs as
`provision apply job.yml --json --deadline <N>m` on GitHub Actions, with a
cancellation case, so the contract is exercised by *a* runner before the
check fires. That is evidence that the stream, the deadline and the signal
work against something which is not a test harness. It is not evidence about
moongit, the consumer this decision's line was drawn for, so the December
reading is "partial" and not "settled".

## D20 — a bare `shell` step's exit code is its whole contract

**Decision.** A `shell` or `cmd` step with no `unless`, `creates` or
`changed_when` reports `ok` on exit 0 and `failed` otherwise, and under `plan`
reports `would run`. It no longer reports `unknown`. `validate --strict` still
rejects it (D3 as amended). `changed_when: false` stays legal and unchanged.

**Why.** The word was a lie, and the spec had said so since phase 5b: a build,
a test or a lint wrote `changed_when: false` in order to read `ok`, and on
`cargo build --release` that is literally false — the build rewrites `target/`
every run. What the author meant was "do not ask me about change", a different
proposition wearing the first one's words. The fix was deferred behind a
reopen condition: a real job file where the boilerplate lands on more than
three steps.

It arrived, and it arrived as evidence against the fix that had been sketched.
`mgitci.yml` in the dotfiles repo is ten steps, each a bare command — and it is
written in **moongit's own schema**, not provision's, because provision's
surface could not carry that shape without ten lines of ceremony. Against
that, the sketched `verdict: exit_code` modifier is the same ceremony under a
truer name, and the sketched file-level default is a second place a reader
must look to learn what a step means. Neither gets `- shell: cargo test` down
to one line, which is the thing the runner actually wanted.

The census that settled it also priced what `unknown` was protecting.
`validate --strict` across all five machine plans found **one** ungated shell
step in the entire fleet: a `killall cfprefsd Finder SystemUIServer` whose own
comment says it is a no-op to gate on. That step reads *better* as `ok` than as
`unknown`. `--strict` had already done its teaching; the fleet gates its
convergence steps, and the default was collecting a toll from the task case to
guard a door nobody was walking through.

**What this does not overturn.** Phase 5b withdrew a `run` verb that made an
ungated step `ok` *by virtue of the verb*, on the rule that the same file must
mean the same thing under every verb. That rule is intact and was the binding
constraint on the design: there is no mode, flag or verb here that makes a bare
step mean the other thing. A file means one thing, and one default verdict
moved in the language for every reader of it. D3's "declared, never guessed" is
also intact — provision still infers nothing about change from a shell step;
it stopped *asking* about change on a step that declares no state.

**The cost, stated plainly.** Nothing now stops a convergence plan from
carrying an ungated `sed -i /etc/pacman.conf` that rewrites the file on every
apply while `plan` reports nothing to do. That is a real regression and the
guard is `validate --strict`, which a plan that converges a machine runs in its
own CI. The trade is deliberate: a task or CI job file, where every step is an
exit code and there is no state to declare, should not pay for a discipline it
does not need.

**Overturned by.** A convergence plan shipping a silent drift bug that
`--strict` would have caught and nobody ran `--strict` on. The fix then is not
this default coming back — it is `--strict` becoming the default for a plan,
which is a different and smaller change.

**Triggered 2026-09-12** (structured multi-persona review, issue #5, before any
plan shipped one). See D21.

## D21 — `validate --strict` defaults on for a plan

**Decision.** `--strict` (D3, D20) is on by default when the root file is a
plan — a list root, the same shape test `is_component` already uses for D17.
A task or component root keeps the old default, off. The flag itself only
ever turns strict *on*; there is no way to turn it off for a plan, because
there is no case for wanting D20's cost without the guard D20 named for it.

**Why.** D20's own "Overturned by" clause named this exactly: nothing stops a
convergence plan from carrying an ungated `sed -i /etc/pacman.conf` that
rewrites a file on every `apply` while `plan` reports nothing to do, and the
only guard was a flag an operator had to remember to type. A review pass
caught the gap before a plan shipped the bug it describes, rather than after —
the fix D20 already specified, applied on schedule instead of waited out.

**What this does not overturn.** D20 itself: a bare `shell` step's exit code
is still its whole contract, `ok` on exit 0, and a task or CI file still pays
nothing for a discipline it has no state to need. Only the plan case's
default moved, exactly as D20 said it would.

## D22 — `register.timed_out` names who killed the step

**Decision.** `register` binds a sixth field, `timed_out: bool`, alongside
`rc`, `stdout`, `stderr`, `changed`, `skipped`. It is `true` exactly when the
step's own process was killed — by its own `timeout`, or by `--deadline` —
rather than exited on its own. `rc` for a killed step stays `124`, unchanged
from before this decision. `timed_out` is the discriminator; `rc` is not.

**Why.** `rc: 124` already meant two different things, and nothing in
`register` said which. A step whose own command wraps something in
`timeout(1)` and legitimately exits 124 reads identically, in
`failed_when`/`register`, to a step provision itself killed for exceeding its
`timeout` or the run's `--deadline` — both report `rc: 124`, because both
*are* `timeout(1)`'s convention (spec, `--deadline`), and that convention is
not being overturned here: a killed child's `rc` staying `124` is still the
right answer to "what does a shell reader expect a killed process to
report." The gap was narrower than "fix the exit code" — it was "give the one
consumer who needs to tell the two apart, `register`, a field for it,"
without touching the process-level exit code (`main.rs`'s `124` on
`--deadline`, spec `--deadline` entry) or `timeout(1)`'s own convention at
all.

**What this does not overturn.** The spec's `--deadline` entry, unchanged: the
run's own exit code is still `124`, still `timeout(1)`'s code, still what a
killed child already reports. Nothing about `rc`'s value changed for any
step, killed or not — a reader who already wrote `failed_when: result.rc ==
124` keeps reading the same thing. `timed_out` is additive: an old plan that
never reads it behaves exactly as before.

**The cost, stated plainly.** A seventh word in a fixed five-field shape that
spec §4 called fixed. The field earns it: the alternative — making `rc` carry
`None`/absent for a killed step — would change what every existing
`failed_when: result.rc == 124` plan reads, silently, for the case D19 exists
to guard.

**Triggered 2026-09-12**, issue #5's "Later" queue, item "namespace exit
codes so a step's own internal `timeout` call is distinguishable from
provision's own deadline-kill."

## D23 — one person's tool, published

**Decided by the owner 2026-10-12**, when the question "who is this for"
turned out to be unanswered after the repo had been public for a month.

**Decision.** provision is the owner's tool for the owner's machines. It is
public so it can be linked, read and copied, not because it is looking for
users. There is no stability promise, no support, no triage obligation. The
issue tracker is the owner's own friction log, which strangers may read and
may file into; nothing in it is a commitment. A tag is a tag, not a product
release, and `1.0` — if it ever happens — will mean the spec stopped moving,
not that anyone is being asked to adopt this.

**Why.** Every decision in this file is argued from a fleet of five machines
and one person's dotfiles. That is the whole source of the tool's
discipline: the non-goals hold because there is no second user whose case
reopens them. A published repo with an Ansible vocabulary table and release
binaries can be misread as a pitch, and a pitch would be the first real
pressure on D19's noun budget — every adopter arrives with a fifth noun and a
reason. Saying the scope out loud costs one paragraph and keeps the argument
honest.

**What this does not mean.** Not abandonment, not a closed tracker, and not
an excuse for bad documentation — the README is written for a reader because
a tool one person maintains for years has one reader who forgets, and that
reader is the owner. Issue #5's review found the positioning was the main
thing reviewers split on while nobody objected to the engineering; this is
the positioning being written down rather than inferred.

**Consequences.** The release artifacts D24 adds exist to get the binary onto
the owner's machines — a phone that cannot comfortably build Rust, a CI image
that should not have to — and the release notes say so. The Ansible table
stays a vocabulary bridge, as issue #5 already reframed it. A feature request
from a stranger is evidence about the world, read with interest, and is still
answered by the deferred list's reopen conditions like any other.

**Overturned by.** The owner deciding to want users, which is a different
project and should be named as one.

## D24 — a second runner: released binaries and the repo's own gate

**Decided by the owner 2026-10-12.** Two gaps closed by one piece of
infrastructure, and each would have been hard to justify alone.

**Decision.** GitHub Actions builds release binaries on a tag for four
targets — `x86_64-unknown-linux-musl`, `aarch64-apple-darwin`,
`x86_64-pc-windows-gnu`, `aarch64-linux-android` — and runs this repo's
quality gate on Linux and Windows on every push. The gate is written as a
component and run as `provision apply job.yml --json --deadline <N>m`, with a
cancellation case, so the contract D19 specified is exercised by a runner
that is not moongit.

**Why, half one: the binary has to reach the machine.** D1's amendment
dropped musl and `aarch64-apple-darwin` "until something consumes them",
which was correct when every machine in the fleet had a Rust toolchain. Two
things now consume them. A phone running Termux (issue #9) can only get
provision by `cargo build` inside Termux, which is the kind of install nobody
does twice. A CI image that runs `provision apply job.yml` should not carry a
Rust toolchain to get one 3 MB binary. D1's test was "a cross-built artifact
nobody installs is a build we would maintain for nothing"; the artifacts now
have installers, so the test passes and the targets come back.

**Why, half two: D19 was built against a runner that never ran it.** Phase 7
shipped SIGTERM handling, `--deadline` and `list <component.yml>` for a
contract moongit has not yet used, and D19's own reopen check — 2026-12-12 —
says that if nothing has exercised it by then, D17's CI half and D19 are
**unvalidated**, not settled. A gate running the stream, the deadline and a
cancel on every push is the evidence that check is asking for, available
before the date rather than after it.

**What this costs, stated plainly.** A second runner exists, and D19's table
says moongit owns scheduling, log retention and cancellation policy. That
line is unchanged: GitHub Actions is a consumer of the same contract, not a
new owner of anything, and it schedules and retains for this one repo exactly
as moongit will for its own. What it must not become is a place where CI
features accrete that provision then grows nouns to serve — the gate is one
job file and one process, and if it ever needs a second, the second belongs
in the workflow and not in the language. And the evidence it produces is
evidence about *a* runner, not about moongit: a moongit job still has to run
before D17's CI half is settled on its own terms. The December check reads
this decision as partial credit, not a pass.

**Setup is a separate invocation.** The gate is `tasks/ci.yml`; the tools it
needs are `tasks/tools.yml`, run first. That is not tidiness — a single file
doing both cannot work, because `use` paths resolve during validation and a
checkout made by step 1 does not exist when step 2's `use` is resolved
(#27). The combined file was written first, passed on a warm machine, and
failed on the first cold runner, which is the useful half of having a cold
runner at all.

**Bootstrapping.** The gate builds provision from source and then runs the
job with the binary it just built. There is no released-binary dependency in
the gate, and no gate dependency in the release: a tag that fails to build
fails loudly, and the gate never runs a provision older than the commit it is
gating.

**The matrix is Linux and Windows.** Linux is where the fleet lives. Windows
is the platform README Status names as cross-compiled and natively untested —
`pkg` against winget, `service`, and Ctrl-C — which is the standing item from
issue #5's "Later" queue. macOS is covered by the owner running the gate on a
Mac, which happens, and `defaults` stays unit-tested against captured
`defaults read` output until a probed run on a mac says otherwise.

**Overturned by.** GitHub Actions becoming a thing the language has to serve.
Then the gate goes back to local and D19's check reads unvalidated, which is
a worse answer honestly stated rather than a better one bought with nouns.

## D25 — an agent reads the output and writes the plan; it is not a runtime

**Decided by the owner 2026-10-15**, when a review asked who the `--json`
stream is for and found the answer had been written down once, for a
consumer that is not the only one.

**Decision.** An agent — a model writing plan files and reading what
provision says back — is a named consumer of the output, the way D19 names
the CI runner. Being *legible to* one is in scope. *Being* one is not, and
nothing in README's "Not an agent runtime. No MCP, no SDK, no LLM loop"
moves.

Concretely, one rule about shape: **verbs that walk stream, verbs that answer
return a document.** `validate`, `plan` and `apply` emit one JSON object per
line as the walk proceeds; `list` and `facts` answer one question and return
one document. `facts --json` already returns a document, so a single-stream
rule would be a breaking change to a contract that exists, bought for
symmetry nobody asked for.

**Why an agent is not a user.** D23 says this is one person's tool and there
are no users. That still holds: an agent is not a second machine owner whose
case reopens a non-goal. It is a reader and a writer — it reads diagnostics
and verdicts, and it writes YAML. Neither of those wants inventory, a
daemon, a registry, a plugin economy or a fifth noun. The thing it wants is
the thing the README already claims to be good at, which is saying what
happened in a form something else can act on.

**The precedent is already in this repo and was never generalised.**
`tasks/findings.yml` exists to "write every gate finding to
`.gate/findings.jsonl` for an agent to read", with fingerprints for dedup.
The decision that a machine reader deserves a structured answer was made for
the quality gate's output. provision's own diagnostics never got it: a
config error emits prose on stderr under every verb, including `--json`, and
`apply` emits nothing at all because its preflight runs before the sink is
built.

**What this does not buy.** The no-list, in D19's spirit, because the whole
value of naming a consumer is naming what it does not get to ask for:

- No MCP server, no SDK, no LLM loop, no long-lived process.
- No `mcp:` action or other agent-host typed action. An agent host is a set
  of files, and `file`, `template`, `git` and `download` already own files.
  An action earns its place by needing state inspection (D4), not by naming
  a popular category.
- No run log and no state directory. D9 holds, and the redirect is the log.
- **No merged-scope dump**, and this one is refused on D19's own reasoning
  rather than on taste. The scope holds `{{ env.TOKEN }}` in clear, and D19
  already declined to promise masking because a promise a step can defeat by
  base64-ing a token is worse than no promise. A dump of the whole scope is
  that same undefendable promise wearing a debugging hat. If the gap ever
  has to close, it closes on a named variable, not a dump.
- No structured before/after on diffs. The unified diff is what a reader
  wants, and a second representation is a second thing to keep true.
- No `json_version` field and no schema negotiation. The binary's
  `--version` is the version and spec §9.3 is the contract; D19 added no
  such field and did not need one.
- Parallel steps, `use` deduplication, secret masking: D19's list, restated.
  A third consumer reopens none of them.

**Blast radius, and why it stays a convention.** An agent holding the trigger
raises the cost of pointing `apply` at the wrong plan: nobody eyeballed the
file first. The answer is the one `examples/x1.yml` already shows — the plan
declares its own machine in its first step, an `assert` tagged `always`, and
refuses before touching anything. That stays a convention and does not become
a checked field. A checked field is a noun, the convention already works, and
the failure it guards against is one a reader of the plan can see. What
changed is that the pattern is now written down as a pattern rather than left
to be inferred from an example.

**Amended 2026-10-15: the convention needed a `facts.` to lean on.** As
written above, the guard did not hold. `assert: {expr: hostname == "x1"}`
reads the bare name, which §3.3 lets a `vars` step outrank — so
`vars: {hostname: x1}` anywhere above it satisfied the guard on every
machine, and `validate --strict` reported clean. A decision whose stated
answer to blast radius is a convention cannot have that convention be
silently defeatable from inside the file it is meant to protect.

The fix is a namespace, not a prohibition. Refusing to let a plan shadow a
fact would have broken `--vars-file`, which is how a Linux plan is validated
from a Mac and is the whole off-host fleet workflow — the precedence rule is
right, and the gap was that a guard had no way to say "the real one". It does
now: `facts.hostname` is the fact and nothing shadows it, the same way `env`
is already unshadowable. `examples/x1.yml` and the README pattern both read
it through `facts.` as of this amendment. The convention stands; it now has
something to stand on.

**Overturned by.** Wanting the tool to *be* an agent runtime, which is a
different project and should be named as one.

## D26 — the gate fires under every verb that walks the plan

**Decided by the owner 2026-10-15**, after a review found a one-step plan
that `validate` refused and `apply` ran.

**Decision.** The idempotency-gate check (D3, D20, D21) runs under `plan` and
`apply` as well as `validate`, on the same root-shape test: a list root is a
convergence plan and the check is on; a mapping root is a task or component
and it stays off unless `validate --strict` asks. Under `apply` it runs in
the preflight walk (spec §7), so a plan that fails it has run nothing.
`--strict` stays a `validate`-only flag: it only ever turns the check on, and
under `plan` and `apply` there is nothing left for it to turn on.

**Why.** D20 named its own cost in writing — nothing stops a convergence plan
carrying an ungated `sed -i /etc/pacman.conf` — and D21 installed
`--strict`-on-by-default as the guard D20 asked for. But the guard only fired
if the operator independently typed `validate`. A one-step list-root file
with an ungated `shell` was exit 3 from `validate` and exit 0 from both
`plan` and `apply`, so the regression D20 named was live on the only verb
that touches the machine. D21 was the right decision applied in one of the
three places it had to hold.

**Why the preflight and not the walk.** This is forced, not stylistic. The
walk re-reads its diagnostics at the top of each step, so a gate error raised
during the walk that executes arrives *after* the ungated command has already
run. Apply's preflight already existed for the sudo scan (§7) and already
lands before `Sudo::preflight`, before the interrupt handler and before the
executing expander exists. `plan` needs no preflight: it executes nothing, so
the diagnostic is non-fatal mid-walk and the report after it turns the walk
into exit 3, which is the path every other plan-time diagnostic already
takes. A preflight there would buy an ordering nicety on a verb that cannot
touch anything, and cost a second full render of every step, every template
and every `vars_file` on every invocation.

**What this costs, stated plainly.** A file that applied yesterday can exit 3
today, and there is no flag to make it apply again — the fix is to declare
the gate, or to say the file is a task by giving it a `description:` and a
`steps:` root. Nine fixtures in this repo paid exactly that, and every one of
them was a task wearing a plan's shape. And the check answers for the steps
the run would run: `apply --tags db` does not report an ungated step in
`web`, because the whole-file question is `validate`'s, which takes no
filter. The alternative — checking steps the run excluded — would make a
tag-filtered apply refuse over work it will not do, and the invariant D20
names is pointwise over executed commands anyway: a violation `--skip-tags`
hides from the operator is hidden from the machine too.

**What this does not overturn.** D20: a bare `shell` step's exit code is
still its whole contract, `ok` on exit 0, under every verb, and a task or CI
job file still pays nothing. D21: the default and the root-shape test are
unchanged. Only the set of verbs that read it moved. Spec §8's claim that a
generated CI job file is "one line per command" was stale and is corrected
rather than honoured — the shape it describes is a mapping root, which
`tasks/ci.yml` already is.

**Overturned by.** A convergence plan whose CI cannot ship because a
legitimately ungated step has no honest gate to declare and no task root to
move to. The fix then is a per-step `verdict:` escape hatch, which D20
already weighed and closed.

## D27 — strict reads a subtree this machine gates off, shape only

**Decision.** When `strict` is on, the walk descends into a structural step
whose `when` is false and checks the steps inside it for shape: the
idempotency gate (D3, D21) and §6.10's `defaults` os-gate. It renders
nothing there, records nothing, and reports no step. The step itself is still
reported `skipped   when: false`, exactly as before.

**Why.** `validate` evaluates `when`, so a subtree gated off on *this*
machine was invisible to it. A `when: os == "linux"` component carried no
weight on a Mac and a `when: os == "darwin"` one carried none on Linux — so
the same plan validated differently on different hosts, and an ungated
`shell` inside a cross-platform component was found by whichever machine
happened to match. That is the opposite of what a linter is for, and it fails
on exactly the plans that most need checking: os-gated components are what a
cross-platform plan is made of. D20's named regression — a convergence plan
carrying an ungated `sed -i /etc/pacman.conf` — was reachable by putting it
behind an os gate.

**Why shape only.** A gated-off subtree may read variables that do not exist
on this machine; that is usually *why* it is gated. Rendering it would fail
on an undefined variable, a missing `src`, or an import path named through
one of those variables — which is mooncake #191, the failure D7's ordering
exists to prevent: a step that is skipped must not fail on what it reads. So
the descent evaluates no condition, renders no field and binds no variable.
The one thing it must render is the path of the next file, because there is
no other way to reach it, and a failure there just ends the descent.

`vars` and `vars_file` are not run inside a false branch. They compute
values, and values are the one thing a gated-off subtree has no business
producing: they would land in the live scope and be read by the steps after
it.

**Why it is gated on `strict` and not on the verb.** Two reasons that agree.
The strict gate is the only thing a shape-only walk checks, so with strict
off there is nothing to go in for. And strict is a property of the file
(D26), so all three verbs that walk a plan root descend and reach the same
verdict — which is what §8 promises: one file, one set of arguments, one
verdict. Restricting this to the `validate` verb would have made it refuse a
file `plan` accepts, with the same arguments on the same machine, and that is
the single thing §8 says cannot happen.

**What this costs, stated plainly.** A plan that validated yesterday can exit
3 today, under all three verbs, because a fault that was hidden behind an os
gate is now visible — and the fix is the same as D26's: declare the gate. A
machine can now be told about a fault in a component it will never run, which
is the point and is also mildly annoying. The `defaults` os-gate rule reads
the plan rather than the machine, so a `defaults` step inside an
`os == "darwin"` import is gated when validated on Linux, where the import is
false and the steps will never run; that is correct — the rule asks what the
plan says, not what this host is. The gate reads `os` and `facts.os`
alike: `facts.os` is the spelling §5 makes unshadowable and the one the
repo's own host-guard pattern uses, and a rule that accepted only the bare
name was the tool demanding the weaker form.

**What stays un-checked, on purpose.** Everything that needs a value: types,
undefined variables, missing files, prop arity. **A `use:`'s `props:` are not
rendered either** — this was written as a claim before it was true: `open()`
honoured `shape_only` and `bind_props` one line later did not, so a gated-off
`use` still rendered and type-checked every prop value, which is mooncake
#191 in the one place the guard was missed. Worse, a prop that failed to
render meant the component's steps were never walked, so a templated prop
silently disabled the strict gate for that whole component — the descent
reporting nothing precisely where it was most needed. Found by an adversarial
review the same day and fixed; the subtree is now walked with empty props,
which is safe because nothing inside is rendered. A gated-off subtree is
checked for what it *says*, never for what it would *do*. Nobody should read
a clean `validate` as proof that the other OS's half of a plan will run.

**Overturned by.** A shape check that cannot be made without a value, for a
fault worth catching across the gate. Then the honest move is a second verb
or a flag that takes a target os and renders against it — not relaxing this
one into rendering a branch the machine cannot evaluate.

## D28 — `pkg` takes a `>=` version floor, checked locally

**Decision.** `pkg` accepts `version: ">=24"` on `state: present`. It is a
floor and nothing else: no exact pin, no `<=`, no ranges. It changes the
question the step asks — "installed, and at least 24" instead of "installed"
— and never the call the step makes.

**Why.** `present` and `latest` were both wrong for a package whose distro
version is too old, which is the ordinary case for a plan that registers a
third-party repository. Ubuntu noble ships `nodejs` 18.19.1; with NodeSource
registered, `state: present` reported `ok` in 104ms having changed nothing,
because the question it asked was "is `nodejs` installed" and the honest
answer was yes. `latest` would have installed 24 that day and 25 on some
unrelated future apply — upgrade-class behaviour, which in this fleet is a
deliberate act (`just upgrade`) and explicitly not something an `apply` does.
So the choice was between a constraint too weak and one unbounded, and the
thing actually wanted was not expressible. The working pattern was to drop
out of `pkg` into a `shell` step with a hand-written `unless` that parses
`node --version` — a second way to say what the `pkg` step already said,
which is the shape D5 and §6.5's `adopt` note both argue against.

**Why it is checked locally and not expressed to the manager.** This is the
part that makes it affordable. The queries already carry versions, because
`latest` has nothing to compare without them (§6.5), so the floor costs no
new probe and no new per-manager code. Expressing it instead would have
meant four different syntaxes and one dead end: apt takes `name=version`,
brew has versioned formulae rather than constraints, winget's `--version` is
exact rather than a range, and **pacman has no floor syntax at all**. A
feature that could not work on pacman is not one this fleet can have.

**What this costs, stated plainly.**

- **The comparison is coarse, by choice.** A leading `epoch:` dropped, split
  on `.` and `-`, each piece contributing its leading digits, compared left
  to right until a piece has no digit at all. Amended the day it shipped: it
  first required the *whole* piece to be a number, which read brew's
  `1.5.7_1` as `[1,5]` and reported it below `>=1.5.7` — wrong on 22 formulae
  and 2 casks on one ordinary machine, and red on every apply forever, since
  the upgrade no-ops and the post-check re-reads the same string. The
  coarseness that remains is the price: a pre-release written `2.0~rc1` reads
  as `2.0` and clears `>=2.0`, where Debian ordering puts it before. It is not Debian ordering and not
  rpm ordering. Those two disagree with each other, and implementing either
  badly would be worse than naming a simple rule and holding to it. The rule
  is in spec §6.5 and pinned by unit tests including the apt-epoch case,
  where getting it wrong would make every epoch-carrying package clear every
  floor.
- **A version with no leading number fails the step** rather than being
  guessed at. D11 and D14's house answer: say so, do not invent.
- **An unreachable floor fails the step**, and that is a new way for an
  apply to go red. It is the honest answer — the plan asked for something
  this machine's sources cannot provide, usually a repository that was never
  registered — and the message names the version it got and the floor it
  wanted, because those two numbers are the diagnosis.
- `plan` answers a floor from the local query rather than reporting
  `unknown` the way `latest` does, because whether the installed version
  clears the floor *is* a question the local database answers. Whether the
  upgrade can reach the floor is a network question and plan claims nothing
  about it — the same way it claims nothing about an install succeeding.

**What this does not open.** Not a constraint language. `>=` is one
operator, chosen because it is the only one a converge step can act on: an
exact pin is a different feature needing the manager's own syntax, and `<=`
describes a state a plan cannot reach by installing. Repositories, taps and
PPAs remain outside `pkg` (§6.5) — this makes the *floor* expressible, not
the source that satisfies it, and a plan still registers NodeSource itself.

**Overturned by.** A plan that needs an exact pin, which is where the
per-manager syntax differences become unavoidable and this design stops
paying. Or a manager whose version strings the numeric rule cannot read, at
which point the rule needs naming per manager rather than globally.
