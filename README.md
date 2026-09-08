# provision

A small, single-binary provisioning tool for your own machines. Rust.

One YAML file per machine. Seven actions. Sequential execution, fail-fast,
honest dry-run, readable output. No fleet, no daemon, no agent, no plugin
economy.

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

This replaces [mooncake](https://github.com/alehatsman/mooncake) for one
job: provisioning a handful of personal machines (Arch laptop, two Windows
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
- **Seven typed actions** where idempotency genuinely needs state inspection:
  `shell`, `cmd`, `file`, `template`, `pkg`, `service`, `assert`.
- **Composition** with `vars`, `vars_file`, `import`, and `use` (a component
  with declared props).
- **Jinja2 templating** (minijinja), strict undefined, the same `.j2` files
  you already have.
- **Three commands.** `validate`, `plan`, `apply`.
- **Output a human reads.** One line per step, live, colored on a TTY, plain
  on a pipe, unified diffs for file changes in plan mode, `--json` for
  machines.

## What it is not

- Not a fleet manager. No peers, no daemon, no remote apply. SSH in and run it.
- Not an agent runtime. No MCP, no SDK, no LLM loop.
- Not transactional. No rollback. Fail-fast and re-run; every step is
  idempotent by construction or by declared gate.
- Not a task runner. Use `just` or a Makefile for repo tasks.
- Not a secrets manager. `{{ env.TOKEN }}` and file permissions.
- Not audited. No run log, no state directory.

## Documents

| File | What |
|---|---|
| [docs/spec.md](docs/spec.md) | The contract: config model, actions, execution, output, edge cases |
| [docs/plan.md](docs/plan.md) | Build plan: phases, gates, crates, LOC budget |
| [docs/decisions.md](docs/decisions.md) | Why Rust, why no rollback, why shell-first, tag semantics |
| [docs/migration.md](docs/migration.md) | Moving the existing dotfiles off mooncake |
| [docs/audit.md](docs/audit.md) | The spec §11 gate: every construct in the real configs, mapped |
| [examples/](examples/) | A machine plan and a component |

## Status

Phases 0 through 4 are code complete. `validate`, `plan`, `apply` and `facts`
all work, and all seven actions — `shell`, `cmd`, `assert`, `file`,
`template`, `pkg`, `service` — run for real, with `unless`, `creates`,
`timeout`, `retry`, `env`, `cwd`, `register`, `changed_when`, `failed_when`,
tags, sudo, Ctrl-C and `--json`. Windows cross-compiles to a self-contained
2.6 MB `.exe`.

What is left is the owner's: applying to each machine, a Windows box to
validate on, and the release itself.

The spec §11 gate is closed — all 371 steps in the real configs walk against
the spec with zero unmapped constructs ([docs/audit.md](docs/audit.md)) — and
so is the phase 0 gate: all five `~/dotfiles` machine plans validate and
render (migration.md §7 step 1).

```
$ provision plan main_pc.yml
  ...
  -     Install and start the mooncake agentd        skipped   unless  101ms
  ✓     Verify the agentd is listening               ok  100ms

  main_pc.yml · 160 steps · 6 would change · 7 would run · 6 would run (unprobed)
              · 82 ok · 59 skipped · 12.2s
```

No `unknown` in that line, and that is the point of `validate --strict`:
every step on this machine can say what it did. The six unprobed ones are
gated on a register whose step has not run, which plan reports rather than
guesses (D14).

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
