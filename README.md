# provision

A small, single-binary provisioning tool for your own machines. Rust.

One YAML file per machine. Ten actions. Sequential execution, fail-fast,
honest dry-run, readable output. No fleet, no daemon, no agent, no plugin
economy.

The same executor runs a repo's tasks and a CI job: same file format, same
verdicts, one meaning per file. See [Tasks and CI](#tasks-and-ci).

![provision apply on a real machine](docs/demo.gif)

A real, full `provision apply` — every component of a real desktop's
plan, dogfooded from a personal dotfiles repo. Recorded with
[asciinema](https://asciinema.org), converted to gif with
[agg](https://github.com/asciinema/agg).

```yaml
- name: Install zsh
  pkg: { names: [zsh] }
  sudo: true

- name: Deploy .zshrc
  template: { src: ./zsh/.zshrc.j2, dest: ~/.zshrc }

- name: Generate SSH identity
  shell: ssh-keygen -t ed25519 -C "{{ email }}" -f ~/.ssh/id_ed25519 -N ""
  creates: ~/.ssh/id_ed25519
```

```
$ provision apply x1.yml
  x1.yml
  ✓ Install zsh                                      ok  101ms
  ~ Deploy .zshrc                                    changed  0ms
    │ --- current
    │ +++ /home/aleh/.zshrc
    │ @@ -0,0 +1 @@
    │ +export EDITOR=nvim
  - Generate SSH identity                            skipped   creates exists  0ms

  x1.yml · 3 steps · 1 changed · 1 ok · 1 skipped · 134ms
```

A changed `file` or `template` step prints its diff under the line;
`--no-diff` drops them. Zero counts are left out of the summary.

## Install

No installer, no package. Three ways, in the order of least work:

**A tagged binary.** Each `v*` tag publishes
`x86_64-unknown-linux-musl`, `aarch64-apple-darwin`,
`x86_64-pc-windows-gnu` and `aarch64-linux-android` to its GitHub release,
with a `SHA256SUMS` next to them. Download the one for your machine, check
it against that file, `chmod +x`, and put it on `PATH`. A machine with no
Rust toolchain — a phone running Termux, a CI image — takes this path, and
a plan that installs provision on another machine should use a `download`
step carrying the `sha256`, which is what the checksums file is there for.

**From source**, if you have Rust:

```
$ cargo build --release --locked
$ cp target/release/provision ~/.local/bin/
```

**With provision itself**, once you have built it once:

```
$ provision apply tasks/install.yml
```

That builds and copies to `~/.local/bin/provision`; `--prop dest=…` puts
it somewhere else. It reports `ok` when the binary already matches, which `cp`
cannot.

These binaries exist for the owner's machines, not as a product: there is no
stability promise and no support (D23).

## Why this exists

This replaced [mooncake](https://github.com/alehatsman/mooncake), now
archived, for one job: provisioning a handful of personal machines (Arch laptop, two Windows
boxes with WSL, two Macs). A review of the real configs driving those
machines on 2026-09-07 found:

| Observation | Number |
|---|---|
| Actions mooncake ships | 64 |
| Actions the real configs use | 25 |
| Real steps that are plain `shell` or `cmd` | 57% |
| Uses of transactions, rollback, secrets, loops, handlers | 0 |
| Production Go LOC carrying that | ~122k |

The job is small and stable. The tool should be too.

## What it is

- **Provisioning only.** Read a YAML plan, converge one machine, report.
- **Shell is first-class.** Most real steps are shell. The tool makes shell
  steps *idempotent by declaration* (`creates`, `unless`) instead of
  pretending a typed action exists for everything.
- **Ten actions** where idempotency genuinely needs state inspection:
  `shell`, `cmd`, `file`, `template`, `pkg`, `service`, `assert`, `git`,
  `download`, `defaults` (D4 as amended). `provision list <action>` prints
  the fields any of them takes.
- **Composition** with `vars`, `vars_file`, `import`, and `use` (a component
  with declared props).
- **Jinja2 templating** (minijinja), strict undefined, the same `.j2` files
  you already have.
- **Five commands.** `validate`, `plan`, `apply`, `list`, `facts`. The first
  three take a machine plan or a component; `list <dir>/` names the components
  in a directory by their `description:`, `list <component.yml>` prints the
  props one takes, and `list <action>` prints the fields a builtin takes.
- **Output a human reads.** One line per step, live, colored on a TTY, plain
  on a pipe, unified diffs for file changes in plan mode, `--json` for
  machines.

## What it is not

- Not a fleet manager. No peers, no daemon, no remote apply. SSH in and run it.
- Not an agent runtime. No MCP, no SDK, no LLM loop.
- Not transactional. No rollback. Fail-fast and re-run; every step is
  idempotent by construction or by declared gate. When an `apply` fails
  halfway: [docs/break-glass.md](docs/break-glass.md).
- Not a task registry. A task is a component file run with `provision apply`;
  shared ones are checked out by the machine plan, never fetched by
  provision.
- Not a secrets manager. `{{ env.TOKEN }}` and file permissions.
- Not audited. No run log, no state directory.

## Tasks and CI

One executor, one file format, one meaning per file. A task is a component:
typed props are its arguments and the filesystem is the registry. `list`
answers both halves of "what can I run here, and how":

```
$ provision list tasks/install.yml
  install                   copy the release binary onto PATH

  props:
    dest                    string  default: ~/.local/bin/provision
                            Where the binary lands. `~` expands.
```

The same question about a builtin, which has no file to open:

```
$ provision list git
  git                       Ensure a checkout of a repository at a ref.

  fields:
    repo                    required            the URL to clone from
    dest                    required            the directory the checkout lives in
    ref                     optional            tag, sha, or branch; default: the remote's default branch
    depth                   optional            shallow; requires a `ref` naming a tag or a commit
```

A CI runner gets no per-step entry point. It writes the job to a file and
runs one process — `provision apply job.yml --json --deadline 30m` — and
reads every step's status, exit code, captured output, duration and
`file:line` off stdout as that step finishes. Cancel it with SIGTERM: the
step's process group dies, the summary is still printed, exit 143.
`--deadline` bounds the run, so the runner never has to SIGKILL from outside
and lose the summary it came for.

And because `validate`, `plan` and `apply` are one walk at three depths of
commitment, the first two double as a **pipeline linter** for free — a side
effect of one executor meaning one thing under every verb, not a feature
built to compete on:

```
$ provision validate job.yml   # parse, render every template, resolve every
                               # path. Run nothing.
$ provision plan job.yml       # ...and say which steps would run.
```

What stays the runner's: checkout, environment, secret delivery, scheduling,
log retention. Job facts — commit, branch, PR — arrive as `--var` and are not
facts. The line is written down in [D19](docs/decisions.md), along with what
this deliberately does not buy: parallel steps, `use` deduplication, secret
masking.

## For Ansible users

This is not a migration pitch — provision is scoped to one person's own
machines (no inventory, no fleet, no roles-as-a-package-format), and Ansible
remains the right tool past that scope. The table below is a vocabulary
bridge for a reader who already thinks in Ansible's terms and wants to know
where the same idea landed here, or where it was deliberately left out.

Same words where the idea is the same, a decision number where it is not.
The decisions are in [docs/decisions.md](docs/decisions.md).

| Ansible | provision | Note |
|---|---|---|
| playbook, `hosts:` | one plan file per machine | run where you stand; no inventory, no `delegate_to` (D2) |
| role, `include_tasks` | `use` a component with declared `props`, `import` a file | props are typed and required by declaration, not by convention (D12) |
| `vars`, `set_fact`, `vars_files` | `vars` step, `vars_file` | `--var k=v` on the command line is the top layer |
| `copy`, `template`, `file` | `file`, `template` | `file` takes `content` or `src`, `state: file/dir/link/absent` |
| `package`, `apt`, `pacman`, `homebrew` | `pkg` with `manager:` | one query for the set, one install for the missing subset; `version: ">=24"` is a floor, checked locally (D28) |
| `service`, `systemd` | `service` | `state: started/stopped/restarted/reloaded`, `enabled` |
| `command`, `shell` | `cmd`, `shell` | `shell` needs `creates`, `unless` or `changed_when` under `validate --strict` (D3) |
| `assert`, `fail`, `wait_for` | `assert` with `retry` | plan runs asserts once; `retry` belongs to apply |
| `get_url`, `git` | `download`, `git` | spec §6.8–6.9 |
| `osx_defaults` | `defaults` | spec §6.10; four scalar types |
| `register`, `when`, `changed_when`, `failed_when` | the same | `result.rc`, `result.stdout`, `result.changed`, `result.skipped` |
| `until`, `retries`, `delay` | `retry: {attempts, delay}` | timeout is per attempt |
| `ignore_errors: true` | `failed_when: false` | |
| `creates`, `removes` on `command` | `creates`, `unless` on any command step | `unless` is a command, exit 0 skips |
| `--check`, `--diff` | `plan` | best effort, says `unknown` when it cannot tell (D11); exit 2 when anything would change |
| `tags`, `--tags`, `--skip-tags`, `always` | the same | Ansible semantics (D7), with one divergence: a filter value no step carries is exit 3, not an empty run |
| `become`, `become_user` | `sudo: true` | root or you, nothing between (D8) |
| `loop`, `with_items` | none | deferred; copy the step (plan.md "Deferred") |
| `notify`, handlers | `register` + `when: x.changed` on the restart step | no handlers (D5) |
| `block`, `rescue`, `always` | none | no rollback (D5); apply stops at the first failure, `--keep-going` walks on |
| `lineinfile`, `blockinfile` | none | own the whole file with `template`; deferred otherwise |
| `unarchive` | `download` then a `creates`-gated `shell` | deferred |
| `user`, `group`, `cron`, `sysctl`, `mount` | `shell` with `unless` | server modules, zero use on a workstation fleet (D4) |
| `debug` | none | `--verbose` shows every command's output; `facts` prints the facts |
| `ansible-vault` | none | decrypt with age or sops outside the plan and read the result with `vars_file` |
| `ansible-galaxy`, collections | a git checkout at a pinned tag, `use`d by path | no registry, no lockfile (D17) |
| `ansible-playbook` output | one line per step, live; `--json` one object per line | exit 0/1/2/3: converged, failed, would change, usage; 124 deadline, 130/143 stopped |

## Development

provision provisions itself. The tasks live in `tasks/` and are components
run with `provision apply` — the same executor, the same verb, the same
rules the machine plans use (D17).

```
$ provision list tasks/
  build                     build the release binary for this machine
  ci                        full pre-push gate — fmt, clippy, test, rustdoc, deny, machete, lint drift
  ci-fast                   fast pre-commit gate — lockfile drift, fmt, clippy, ai-lint, soft caps
  ...
```

Once, to check out the quality gate and install the three tools it runs
(cargo-nextest, cargo-deny, cargo-machete — from source, so it is slow):

```
$ provision apply tasks/tools.yml
```

Then `provision apply tasks/ci-fast.yml` before a commit and
`provision apply tasks/ci.yml` before a push.

The gate itself is [rust-quality](https://github.com/alehatsman/rust-quality),
pinned in `tasks/tools.yml` and checked out under `~/.cache/provision/tools/`.
Its presets are components, so each task here is one `use:` line:

```yaml
steps:
  - name: full gate
    use: ~/.cache/provision/tools/rust-quality/ci.yml
```

Nothing fetches at gate time. The checkout is a step in `tasks/tools.yml`,
`creates`-gated like any other, so a version bump is one line there and
offline works. rust-quality ships config as well as presets:
`clippy.toml`, `rustfmt.toml`, `deny.toml` and `.cargo/config.toml` in this
repo are its files, put here by `provision apply tasks/sync-config.yml` and
re-checkable with it -- it reports `ok` on all four when they already match.
The `[workspace.lints]` block in `Cargo.toml` is its canonical lint block;
cargo has no include mechanism for manifests, so `tasks/lints-check.yml`
reports drift instead of copying.

The same gate runs on GitHub Actions, as one job in one process — which is
the shape a CI runner is supposed to use it in (D19, D24):

```
$ provision apply tasks/tools.yml                      # setup, once per runner
$ provision apply tasks/ci.yml --json --deadline 40m   # the job
```

Two invocations, and the first one is why: `use` paths resolve during
validation, before any step runs, so one file cannot both create the
rust-quality checkout and `use` a preset from it — it would validate green
only on a machine that already had the checkout, which is every machine
except a fresh runner. A combined file was written, passed locally, and
failed on CI's first cold run; the split is what the Development section
above already told a reader to do, and [#27](https://github.com/alehatsman/provision/issues/27)
is why it is not a one-liner. CI runs the same `tasks/ci.yml` a push runs, so
neither restates the other. The workflow redirects the
stream to a file and reads it back with `.github/check-stream.py`, which
asserts what spec §9.3 promises: one object per step as it finishes, a
summary that says whether the run was stopped, and counts that agree with the
steps that produced them. A second job exercises the rest of the contract —
SIGTERM gives exit 143, `--deadline` gives 124, and both still print the
summary.

Windows runs `tasks/tests.yml` — the suite with no tool but cargo — natively.
`tests/apply.rs` is `#![cfg(unix)]`, because most of it is modes, `chown`,
`stat` and sudo, which are Unix behaviour rather than assertions that are
merely awkward to write portably. The engine underneath it is not: the four
verdicts, stop-on-failure, `--keep-going`, a register read by a later step,
the `--json` shape, `--deadline`, a `file` step converging, and tag
selection run on both, in `tests/apply_cross_platform.rs`, with every
command spelled so that `bash` and `powershell` both accept it. With
`cli.rs`, `component.rs`, `list.rs` and the unit tests, what a Windows host
now covers is parsing, validation, `list`, components, the CLI, every exit
code they reach, and the apply engine's own contract.

What stays untested there is `pkg` against winget, `service` beyond its
refusal, and Ctrl-C. The first two would have to install or start something
on the machine running them; the third needs a console control event rather
than a signal, which is a different mechanism from the one the Unix test
drives.

That is evidence about **a** runner, not about moongit. The `mgitci.yml` and
the Rust CI image still wait on moongit's runner switching to `provision apply
job.yml --json`, and D19's reopen check reads this as partial credit rather
than settled.

Tagged binaries come from the same place: `.github/workflows/release.yml`
builds `x86_64-unknown-linux-musl`, `aarch64-apple-darwin`,
`x86_64-pc-windows-gnu` and `aarch64-linux-android` on a `v*` tag and
publishes them with a `SHA256SUMS`, so a machine with no Rust toolchain —
a phone running Termux, a CI image — can install one with
`tasks/install.yml` or a `download` step carrying the checksum. They exist
for the owner's machines, not as a product: there is no stability promise
and no support (D23).

## Documents

| File | What |
|---|---|
| [docs/spec.md](docs/spec.md) | The contract: config model, actions, execution, output, edge cases |
| [docs/plan.md](docs/plan.md) | Build plan: phases, gates, crates, what is deferred |
| [docs/decisions.md](docs/decisions.md) | Why Rust, why no rollback, why shell-first, tag semantics |
| [docs/migration.md](docs/migration.md) | Moving the existing dotfiles off mooncake |
| [docs/audit.md](docs/audit.md) | The spec §11 gate: every construct in the real configs, mapped |
| [docs/break-glass.md](docs/break-glass.md) | `apply` failed halfway and the machine has to work again now |
| [examples/](examples/) | A machine plan and a component |
| [tasks/](tasks/) | This repo's own tasks, run with `provision apply` |

## Status

Version 0.9.2. Phases 0 through 7 are code complete ([docs/plan.md](docs/plan.md)).
`validate`, `plan`, `apply`, `list` and `facts` all work, and all ten
actions — `shell`, `cmd`, `assert`, `file`, `template`, `pkg`, `service`,
`git`, `download`, `defaults` — run for real, with `unless`, `creates`,
`timeout`, `retry`, `env`, `cwd`, `register`, `changed_when`, `failed_when`,
tags, sudo, `--deadline`, Ctrl-C, SIGTERM and `--json`. Components take typed
`props`, from a `use:` or from `--prop` on the command line, and this repo's
own tasks and the shared quality gate run through them. Windows
cross-compiles to a self-contained `.exe` and has been run natively.

The one action this machine cannot exercise is `defaults`: its compare is
unit tested against captured `defaults read` output, and a probed run on a
mac is the owner's.

The dotfiles are migrated: all five machine plans validate under `--strict`,
and the probed plan for this machine reads

```
$ provision plan main_pc.yml
  ...
  →     Build moongit CI container images (refresh … would run  200ms
  ✓     Ensure ~/.ssh exists with the permissions s… ok  0ms
  ✓     Deploy authorized_keys from the declared fl… ok  0ms
  ✓     Verify the agentd is listening               ok  101ms

  main_pc.yml · 166 steps · 1 would change · 9 would run · 6 would run (unprobed)
              · 90 ok · 60 skipped · 12.6s
```

No `unknown` in that line, and that is the point of `validate --strict`:
every step on this machine declares what it manages. The six unprobed ones are
gated on a register whose step has not run, which plan reports rather than
guesses (D14). What is left is the owner's: the first real apply on each
machine.

Since 2026-09-12 an ungated `shell` step reports `ok` on exit 0 rather than
`unknown` — its exit code is its whole contract (spec §6.1, D20), which is what
makes `shell: cargo test` a complete step in a task or CI file. `--strict` is
the guard that keeps a *convergence* plan from quietly carrying one, and — as
of the same day — is on by default for a plan file, so an operator does not
have to remember the flag on the file that most needs it (D21). It runs under
`plan` and `apply` as well as `validate`, so the guard fires on the one verb
that touches the machine (D26); a task or component file keeps the old
default, off. `unknown` is plan-only and comes
from `pkg latest`, a `git` branch ref, and `defaults` off macOS.

`plan` exits 0 when there is nothing to do, 2 when there is, and 1 when a step
failed. Anything that is not `ok` or `skipped` counts as something to do,
`unknown` and `would run (unprobed)` included: a step provision cannot judge is
not a step it may call converged. `--plan-no-probe` inspects nothing, so it claims nothing, and
exits 0. A run stopped from outside exits `128 + signal` — 130 for Ctrl-C, 143
for SIGTERM — and one that runs out of its `--deadline` exits 124.

### A plan that refuses the wrong machine

A plan is written for one machine and can say so. Make the first step an
`assert` tagged `always`, so no `--tags` selection can skip it:

```yaml
- name: Refuse to run on the wrong machine
  assert:
    expr: facts.hostname not in ["mainpc", "minipc", "mac"]
    msg: "hostname {{ facts.hostname }} belongs to another machine"
  tags: [always]
```

`expr:` is evaluated, not run, so the guard costs no process and holds under
`plan` as well as `apply`.

**`facts.hostname`, not `hostname`.** The bare name follows §3.3's
precedence, so `vars: {hostname: mainpc}` anywhere above this step satisfies
the guard on every machine — which is the one thing it exists not to do.
`facts.*` is the fact itself and nothing shadows it. The bare name stays
overridable on purpose: that is how a Linux plan gets validated from a Mac.

`examples/x1.yml` opens with exactly that, so on one of the other machines it
refuses before touching anything:

```
$ provision apply examples/x1.yml        # run on mainpc
  examples/x1.yml
  ✗ Refuse to run on the wrong machine               FAILED  0ms
    │ (exit 1 · hostname mainpc belongs to another machine)

  examples/x1.yml · 1 step · 1 failed · 1ms
```

This is the whole of provision's answer to "pointed at the wrong file", and
it is deliberately a convention rather than a checked field — a checked field
is a noun, and a plan that names its own machine in its first step is
something a reader of the plan can see (D25). It matters more than it used to:
when something other than a person is deciding which plan to apply, this
assert is the step that was not skimmed.

That example is an Arch laptop's, it illustrates every construct in the spec
once, and it is written to be **read rather than applied** — its second step
edits `/etc/pacman.conf`. The two commands that exercise it on any machine
without touching one are the ones that run nothing:

```
$ provision validate examples/x1.yml
  ok  examples/x1.yml

$ provision plan --plan-no-probe examples/x1.yml
  ...
  examples/x1.yml · 19 steps · 16 would run (unprobed) · 3 skipped · 3ms
```
