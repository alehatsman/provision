# provision

A small, single-binary provisioning tool for your own machines. Rust.

One YAML file per machine. Ten actions. Sequential execution, fail-fast,
honest dry-run, readable output. No fleet, no daemon, no agent, no plugin
economy.

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
  `shell`, `cmd`, `file`, `template`, `pkg`, `service`, `assert`, and from
  phase 6 `git`, `download`, `defaults` (D4 as amended).
- **Composition** with `vars`, `vars_file`, `import`, and `use` (a component
  with declared props).
- **Jinja2 templating** (minijinja), strict undefined, the same `.j2` files
  you already have.
- **Five commands.** `validate`, `plan`, `apply`, `list`, `facts`. The first
  three take a machine plan or a component; `list <dir>/` names the components
  in a directory by their `description:`, and `list <component.yml>` prints
  the props one takes.
- **Output a human reads.** One line per step, live, colored on a TTY, plain
  on a pipe, unified diffs for file changes in plan mode, `--json` for
  machines.

## What it is not

- Not a fleet manager. No peers, no daemon, no remote apply. SSH in and run it.
- Not an agent runtime. No MCP, no SDK, no LLM loop.
- Not transactional. No rollback. Fail-fast and re-run; every step is
  idempotent by construction or by declared gate.
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

A CI runner gets no per-step entry point. It writes the job to a file and
runs one process — `provision apply job.yml --json --deadline 30m` — and
reads every step's status, exit code, captured output, duration and
`file:line` off stdout as that step finishes. Cancel it with SIGTERM: the
step's process group dies, the summary is still printed, exit 143.
`--deadline` bounds the run, so the runner never has to SIGKILL from outside
and lose the summary it came for.

And because `validate`, `plan` and `apply` are one walk at three depths of
commitment, the first two are a **pipeline linter** — the thing no other step
runner in this family ships:

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

Same words where the idea is the same, a decision number where it is not.
The decisions are in [docs/decisions.md](docs/decisions.md).

| Ansible | provision | Note |
|---|---|---|
| playbook, `hosts:` | one plan file per machine | run where you stand; no inventory, no `delegate_to` (D2) |
| role, `include_tasks` | `use` a component with declared `props`, `import` a file | props are typed and required by declaration, not by convention (D12) |
| `vars`, `set_fact`, `vars_files` | `vars` step, `vars_file` | `--var k=v` on the command line is the top layer |
| `copy`, `template`, `file` | `file`, `template` | `file` takes `content` or `src`, `state: file/dir/link/absent` |
| `package`, `apt`, `pacman`, `homebrew` | `pkg` with `manager:` | one query for the set, one install for the missing subset |
| `service`, `systemd` | `service` | `state: started/stopped/restarted/reloaded`, `enabled` |
| `command`, `shell` | `cmd`, `shell` | `shell` needs `creates`, `unless` or `changed_when` under `validate --strict` (D3) |
| `assert`, `fail`, `wait_for` | `assert` with `retry` | plan runs asserts once; `retry` belongs to apply |
| `get_url`, `git` | `download`, `git` | phase 6, spec §6.8–6.9 |
| `osx_defaults` | `defaults` | phase 6, spec §6.10; four scalar types |
| `register`, `when`, `changed_when`, `failed_when` | the same | `result.rc`, `result.stdout`, `result.changed`, `result.skipped` |
| `until`, `retries`, `delay` | `retry: {attempts, delay}` | timeout is per attempt |
| `ignore_errors: true` | `failed_when: false` | |
| `creates`, `removes` on `command` | `creates`, `unless` on any command step | `unless` is a command, exit 0 skips |
| `--check`, `--diff` | `plan` | best effort, says `unknown` when it cannot tell (D11); exit 2 when anything would change |
| `tags`, `--tags`, `--skip-tags`, `always` | the same | Ansible semantics verbatim (D7) |
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

**There is no CI for this repo yet.** No `mgitci.yml`, no Rust CI image; both
wait on moongit's runner running a job as `provision apply job.yml --json`.
Until then the gate is a local one, and running it before a push is the whole
of it.

## Documents

| File | What |
|---|---|
| [docs/spec.md](docs/spec.md) | The contract: config model, actions, execution, output, edge cases |
| [docs/plan.md](docs/plan.md) | Build plan: phases, gates, crates, LOC budget |
| [docs/decisions.md](docs/decisions.md) | Why Rust, why no rollback, why shell-first, tag semantics |
| [docs/migration.md](docs/migration.md) | Moving the existing dotfiles off mooncake |
| [docs/audit.md](docs/audit.md) | The spec §11 gate: every construct in the real configs, mapped |
| [examples/](examples/) | A machine plan and a component |
| [tasks/](tasks/) | This repo's own tasks, run with `provision apply` |

## Status

Version 0.9.1. Phases 0 through 5b are code complete ([docs/plan.md](docs/plan.md)).
`validate`, `plan`, `apply`, `list` and `facts` all work, and all ten
actions — `shell`, `cmd`, `assert`, `file`, `template`, `pkg`, `service`,
`git`, `download`, `defaults` — run for real, with `unless`, `creates`,
`timeout`, `retry`, `env`, `cwd`, `register`, `changed_when`, `failed_when`,
tags, sudo, Ctrl-C and `--json`. Components take typed `props`, from a `use:`
or from `--prop` on the command line, and this repo's own tasks and the
shared quality gate run through them. Windows cross-compiles to a
self-contained `.exe` and has been run natively.

Phase 6 — `git`, `download` and `defaults` (spec §6.8–6.10) — is in. The one
of the three this machine cannot exercise is `defaults`: its compare is unit
tested against captured `defaults read` output, and a probed run on a mac is
the owner's.

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
every step on this machine can say what it did. The six unprobed ones are
gated on a register whose step has not run, which plan reports rather than
guesses (D14). What is left is the owner's: the first real apply on each
machine.

`plan` exits 0 when there is nothing to do, 2 when there is, and 1 when a step
failed. Anything that is not `ok` or `skipped` counts as something to do,
`unknown` and `would run (unprobed)` included: a step provision cannot judge is
not a step it may call converged, and `validate --strict` is how that count is
driven to zero. `--plan-no-probe` inspects nothing, so it claims nothing, and
exits 0.

```
$ provision apply examples/x1.yml
  examples/x1.yml
  ✗ Refuse to run on the wrong machine               FAILED  0ms
    │ (exit 1 · hostname mainpc belongs to another machine)

  examples/x1.yml · 1 step · 1 failed · 1ms
```
