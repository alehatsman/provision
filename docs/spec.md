# provision — specification

Status: v0.7 · 2026-09-08 · owner: aleh

This is the contract. Code that disagrees with it is wrong, or this file is.
Fix one.

## 1. Goal

Converge one machine to a declared state from one YAML plan, from a single
static binary, on Linux, macOS, and Windows, with output a person can read
and a dry-run a person can trust.

## 2. Scope

**In**

- Parse and validate a plan. Render templates. Evaluate conditions.
- Execute steps sequentially on the local machine. Stop at the first failure.
- Ten actions: `shell`, `cmd`, `file`, `template`, `pkg`, `service`, `assert`,
  `git`, `download`, `defaults` (the last three from phase 6, D4 as amended).
- Four structural keywords: `vars`, `vars_file`, `import`, `use`.
- Step modifiers: `name`, `when`, `unless`, `creates`, `sudo`, `timeout`,
  `retry`, `env`, `cwd`, `tags`, `register`, `changed_when`, `failed_when`.
- Facts about the local machine.
- Five commands: `validate`, `plan`, `apply`, `list`, `facts`. The first
  three take a plan or a component as the root; a component's props come from
  `--prop` (§8, D17). `list` names the components in a directory, or
  describes one component's props. `facts` prints what this machine reports
  about itself and walks nothing.
- TTY and non-TTY output, `--json` event stream.

**Out** (see README non-goals and decisions.md)

- Remote execution, fleet, daemon, agent, MCP, SDK.
- Transactions, rollback, handlers/notify.
- Loops (`loop`, `with_items`).
- Secrets providers. Run log. State directory. Plugins. Modules registry.
- Typed Windows actions (registry, firewall, scheduled tasks). Windows is
  supported through `shell` with the PowerShell interpreter.
- A task registry. `use` takes a path: no remote source, no fetch, no cache,
  no lockfile. Shared components reach a machine by being provisioned onto
  it (D17).

## 3. Plan file

A plan is a YAML **list of steps**. A step is a mapping with exactly one
action key or one structural key, plus optional modifiers.

```yaml
- name: Install base packages       # modifier
  pkg:                              # action key
    names: [git, curl]
  sudo: true                        # modifier
  tags: [core]                      # modifier
```

Rules:

- Exactly one of the action or structural keys per step. Zero or two is a
  validation error naming the step and the file:line.
- Unknown top-level keys in a step are errors. No silent typos.
- Files are resolved relative to the file that names them, never the cwd.
- `~` expands in every path field the tool owns (`dest`, `path`, `src`,
  `creates`, `cwd`, `vars_file`, `import`, `use`). It does **not** expand
  inside shell scripts; the shell does that.

### 3.1 Structural keywords

| Key | Form | Meaning |
|---|---|---|
| `vars` | mapping | Set variables in the current scope. Later wins. |
| `vars_file` | path or list of paths | Load a YAML mapping into the current scope. Path may contain templates (`./vars/{{ os }}.yml`). Missing file is an error unless `optional: true` is set on the step. |
| `import` | path | Inline the steps of another plan file into this scope. `when` and `tags` on the import apply to every imported step (tags are unioned). |
| `use` | path | Run a **component** in a child scope. See §3.2. |

Scope: a plan file has one scope. `import` shares it. `use` creates a child
that sees the parent's variables read-only plus its own `props`, and whose
`vars` do not leak back.

### 3.2 Components

A component file is a mapping, not a list:

```yaml
description: render the colour palette   # optional; shown by `list <dir>/`
props:
  variant:
    type: string          # string | bool | int | list
    default: dark
    description: palette variant
  notes_dir:
    type: string
    required: true

steps:
  - name: Render palette
    template: { src: ./{{ props.variant }}.j2, dest: ~/.config/palette }
```

Call site:

```yaml
- name: Palette
  use: ../components/palette/index.yml
  props: { variant: "{{ palette_variant }}" }
  tags: [core, palette]
```

- Unknown prop at the call site: error. Missing required prop: error.
  Wrong type after template rendering: error. All three at `validate`
  time, not at apply time.
- Inside the component, props are reachable as `props.<name>`.
- Inside the component, **`component_dir`** is the absolute directory of the
  component file. A path in `file`, `template`, `use` or `cwd` resolves
  against the file that names it, but a path inside a shell string does not,
  and a shared component that ships scripts has no other way to reach them:
  `bash "{{ component_dir }}/scripts/gate.sh"`. `import` does not get it —
  an imported file shares the caller's scope and is not a component.
  Decided by review, confirmed by the owner 2026-09-08.
- A prop's `default` is taken **as written**, not rendered: `default: "{{ home }}"`
  is the literal seven characters, not a path. Defaults are data in the
  component file, and the value a call site passes is the one that gets
  rendered. A default that has to be computed is a `vars` step instead.
- `description` is optional, a plain string, and only read by
  `provision list <dir>/`. Any other root key is an error.
- A component is also a **task**: `provision apply <component.yml>` runs it
  as the root scope with props supplied by `--prop`, and `plan` and
  `validate` take it the same way (§8). A task step whose exit code is its
  whole contract says `changed_when: false` and reads `ok` (§6.1).

### 3.3 Variables and precedence

Lowest to highest:

1. Facts (§5).
2. `vars` / `vars_file` in file order, later wins.
3. Command line `--var key=value` and `--vars-file path` (repeatable, later wins).
4. Inside a component, `props.*` (separate namespace, no collision), and
   `component_dir` (§3.2).

`env.*` exposes the process environment read-only.

**A `vars` step is rendered; a `vars_file` is not.** Values in a `vars:`
step are templates evaluated against the scope at that point, so
`rq_dir: "{{ home }}/.cache/x"` is a path. Values loaded by `vars_file` are
data taken as written: those files hold shell snippets and config text where
`{{` must survive, and there is no second render pass when a variable is
later substituted into a field, so a `{{ home }}` inside one stays the
literal seven characters. A file-loaded value that has to be computed is a
`vars` step beside the `vars_file`, or uses `~`, which every path field
expands (§3). Written into the spec 2026-09-08 after a consumer copied
mooncake's rendered-vars shape and created a directory named `{{ home }}`;
the rule itself dates from phase 0.

### 3.4 Templating

Jinja2 via minijinja. Applies to every string value in a step except the
body of `shell` **when** `raw: true` is set on that step. Templates in
`.j2` files rendered by `template` use the same engine and scope.

- Undefined variable: error at plan time, with file:line and the variable name.
- Filters available: minijinja built-ins plus `expanduser`, `basename`,
  `dirname`, `quote` (shell-quote), `to_yaml`, `to_json`.
- Tests available: minijinja built-ins plus `exists` (path).
- `{% raw %}` blocks are honored; needed for shell scripts with `{{`. This is
  Jinja2's tag. `{% verbatim %}` is Twig's and is **not** supported.

**A field that is one expression keeps its type.** When a string field is
exactly one `{{ … }}` and nothing else, it is evaluated rather than rendered,
and the result keeps the type the expression produced:

```yaml
vars:
  apps: [git, curl]

pkg:
  names: "{{ apps }}"        # the list, not its textual rendering
file:
  path: "{{ home }}/.zshrc"  # a string: there is text around the expression
```

Without this, `names: "{{ apps }}"` would be the string `['git', 'curl']`, and
§3.2's "wrong type after template rendering is an error" could never pass. The
rule is exactly "one expression, no surrounding text"; two expressions, or an
expression with text beside it, render to a string.

Booleans render Python-style, as Jinja2 does: `{{ is_wsl }}` is `True`. Use
`{{ is_wsl | to_json }}` where a config file needs `true`.

### 3.5 Conditions

`when` and `changed_when` / `failed_when` are minijinja expressions
evaluated to a boolean. Non-boolean results are an error, not truthy.

```yaml
when: os == "linux" and not is_wsl
when: pacman_available
failed_when: result.rc not in [0, 2]
changed_when: result.rc == 0
```

## 4. Step modifiers

| Modifier | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | action key + first arg | Shown in output. Templated. |
| `when` | expr | true | Skip the step when false. Evaluated before `unless`/`creates`. |
| `unless` | string | — | A shell command, run through the interpreter `shell` defaults to (bash on unix, powershell on Windows), and with the step's own `sudo`, `env` and `cwd` — a root step's gate has to see root's view. Exit 0 means "already done", step skipped. Non-zero means run, **except** a gate that could not run at all: a spawn failure, `126`/`127` from the interpreter, or the gate hitting the step's timeout. Those fail the step (§10). |
| `creates` | path | — | Skip when the path exists. |
| `sudo` | bool | false | Run the action as root (§7). Windows: error; use `shell` from an elevated prompt. |
| `timeout` | duration | 10m | Kill the step and fail. `30s`, `5m`, `1h`. |
| `retry` | `{attempts, delay}` | none | Re-run on failure. `delay` is a duration. Output shows attempt N/M. |
| `env` | mapping | {} | Extra environment for `shell`, `cmd`, `unless`. Templated. |
| `cwd` | path | the invocation directory | Working directory for `shell`, `cmd`, `unless`, under every command and for every step, including one inside a `use`d component. A command runs where the operator is standing; a shared gate checked out elsewhere must not gate its own checkout. An explicit `cwd:` resolves against the step's own file, like every other path a plan names, and `component_dir` (§3.2) names a component's own directory for a path inside a shell string. Phase 5b, 2026-09-08: this used to be the step's file directory under `apply` and the invocation directory under `run`. Measured against the fleet, no step depended on the old default — zero `cwd:` modifiers, zero relative paths in shell strings — and one rule beats two. |
| `tags` | list | [] | For `--tags` selection (§8). |
| `register` | identifier | — | Store the step result as a variable: `{rc, stdout, stderr, changed, skipped}`. |
| `changed_when` | expr | action-defined | Override the changed verdict. `result` is in scope. |
| `failed_when` | expr | `result.rc != 0` | Override the failure verdict. `result` is in scope. |
| `raw` | bool | false | Do not template the action body. |

Order of evaluation: `when` → `unless` → `creates` → action → `failed_when`
→ `changed_when` → `register`.

## 5. Facts

Computed once at startup, lazily where expensive, read-only.

| Fact | Values |
|---|---|
| `os` | `linux`, `darwin`, `windows` |
| `arch` | `x86_64`, `aarch64` |
| `hostname` | short hostname as the kernel reports it |
| `username`, `home` | current user |
| `distro`, `distro_version` | from `/etc/os-release` (`arch`, `ubuntu`, …), empty elsewhere |
| `is_wsl` | `/proc/version` contains `microsoft` |
| `apt_available`, `pacman_available`, `brew_available`, `winget_available`, `yay_available` | binary on PATH |
| `systemd_available`, `launchd_available` | init detection |

No user-declared facts. Anything else is a variable.

## 6. Actions

Every action defines: fields, what "changed" means, what `plan` reports,
and its idempotency guarantee. `changed` is a verdict, not a guess: an
action that cannot tell reports `unknown`, shown distinctly in output.

### 6.1 `shell`

Run a script through an interpreter.

```yaml
shell: |                       # short form: script string
  set -euo pipefail
  hostnamectl set-hostname x1

shell:                         # long form
  script: ...
  interpreter: bash            # bash (default on unix) | sh | zsh | powershell (default on windows) | pwsh
  login: false
```

- Idempotency: **declared**, via `unless`, `creates`, or `changed_when`. A
  bare `shell` step with none of these is reported `changed: unknown`.
  `validate --strict` fails on it.
- The verdict of a gated step is the gate's own meaning: `unless` exited
  non-zero, or `creates` did not exist, so the work was needed — a successful
  run is `changed`. With no gate the verdict is `unknown`: the step ran, and
  nothing here can say what it did. `changed_when` overrides either.
- Plan: a gated step prints `skip` or `would run` — plan evaluates `unless`
  or `creates` and the gate's answer is a verdict. An ungated step prints
  `unknown`, the same verdict apply gives it and for the same reason: it
  will run, and nothing here can say what it would do. (This used to promise "plus the first
  line of the script". The renderer never printed one and nothing has wanted
  it: the step's name is what carries the meaning, and a script's first line
  is usually `set -euo pipefail`.)
  `--plan-no-probe` skips `unless` evaluation (for CI without the target).
- A step whose exit code is its whole contract — a test run, a build, a
  lint — says `changed_when: false` and is `ok` on exit 0. That is the task
  case (D17), and it is the same declaration a plan makes: there is no
  command under which an undeclared step reads anything but `unknown`.
  (Phase 5 had `run` report such a step `ok` by virtue of the verb; phase
  5b withdrew that. The same file means the same thing under every verb.)
- Streaming: stdout/stderr captured; shown in full on failure, on
  `--verbose`, or streamed live with `--stream`.
- `set -euo pipefail` is **not** injected. Explicit > magic.

### 6.2 `cmd`

Run an argv without a shell.

```yaml
cmd: [mv, "{{ home }}/Projects", "{{ home }}/projects"]
```

Same semantics as `shell` for idempotency, plan, output. Use when arguments
contain spaces or come from variables.

### 6.3 `file`

Ensure a filesystem entry.

```yaml
file:
  path: ~/.config/zsh
  state: dir                   # file | dir | absent | link
  content: "..."               # state=file only; mutually exclusive with src
  src: ./files/gitconfig       # state=file: copy; state=link: link target
  mode: "0644"
  owner: root                  # requires sudo
  group: wheel                 # requires sudo
```

- Changed when content, mode, owner, group, or type differs. Compared by
  bytes, not mtime.

**The four states.**

- `file` needs exactly one of `content` or `src`. Neither is a validation
  error, and both together is one, positioned at the second. There is no
  touch: an empty file is `content: ""`, said out loud.
- `dir` creates the directory and its parents. On one that already exists it
  still applies `mode`, `owner` and `group` where they differ, and reports
  `changed` when it does.
- `link` points `path` at `src`, stored exactly as written — no
  canonicalisation, so a relative link stays relative. Changed when `path` is
  not a symlink at all, or is one pointing somewhere else. Replacing an
  existing regular file or directory at `path` requires `force: true`;
  without it the step fails naming the path. `mode`, `owner` and `group` do
  not apply to a symlink, and setting them is a validation error.
- `absent` removes a file, a symlink, or an empty directory; a non-empty
  directory requires `force: true`. A path that is already gone is `ok`, not
  `changed`.

**Modes are deterministic, not inherited from the umask.** A new file with no
`mode` gets `0644` and a new directory `0755`. A target that already exists
keeps the mode it has unless `mode` says otherwise.

`mode` applies to the target, and directories created on the way to it get
`0755`. That is what GNU `install -d -m` already does — intermediates are
`0755` whatever the umask, and the mode lands on the last component — so the
non-sudo path matches it rather than inventing a third rule. A parent that
already exists is not touched.

A relative `path` resolves against the process's working directory, not the
plan file's. §3's "resolved relative to the file that names them" governs the
files provision *reads out of the plan tree* — `src`, `import`, `use`,
`vars_file` — because those live beside the plan. A `path` names a location on
the machine being converged, and resolving one into the dotfiles repo would
be a surprise nobody asked for.

**A `src` the plan itself builds is judged when the step runs.** `src` must
exist, and §8 checks that before anything runs — except after a `shell` or
`cmd` step, which can create any path and has not created it yet on a walk
that runs nothing. There the check moves to the walk that executes, which
reaches the `file` step after that command has run: `validate` says nothing
about the source, `plan` reports the step `would run (unprobed)`, and `apply`
copies it or fails naming it. Build-then-install is the shape this is for — a
`shell: cargo build --release` followed by a `file` step copying the binary
out of `target/` cannot pass a check made on a clean checkout (D18).

`mode` is a **quoted** string. Unquoted, `0644` is a number, and which number
depends on whether the reader believes it is YAML 1.1 (octal, 420) or YAML
1.2 (decimal, 644). Neither is what was meant, so an unquoted mode is an
error that says to quote it rather than guessing which dialect was intended.

- `owner` and `group` are independent; either may be set alone. Both require
  `sudo: true` and are a validation error without it.

**Reading state obeys `sudo` too.** With `sudo: true` every probe of the
target — its bytes, its mode, its owner — runs as root, because a target the
step needs root to write is usually one it needs root to read:
`/etc/sudoers.d/*` is `0440 root:root`. Without `sudo: true` a target that
cannot be read fails the step, naming the path and saying that `sudo: true`
is how to read it. Under `plan` with no reachable root the step is
`would run (unprobed)`, exactly as a gate is (§7).

**Writing.** Without sudo: a temp file in the destination's own directory,
mode `0600`, then rename — the same directory because rename is only atomic
within one filesystem. With `sudo` the temp file goes in the user's own temp
directory instead, because the reason the step said `sudo` is usually that it
cannot create a file next to the destination at all; it is then placed with
`install -m MODE [-o OWNER] [-g GROUP] TMP DEST` and the temp removed. The
same-directory rule does not apply there: `install` copies, so nothing depends
on the two paths sharing a filesystem.

- Directories under sudo are `install -d -m MODE`, links are `ln -sfn`, and
  removal is `rm -r`. Without sudo the same four operations go through `std`.
- Plan: unified diff for content changes (`--no-diff` to suppress; secrets
  are the operator's problem), `mode 0644 → 0600` and `group staff → wheel`
  for metadata.

### 6.4 `template`

Render a `.j2` file and ensure `dest` matches, with `file` semantics.

```yaml
template:
  src: ./templates/.zshrc.j2
  dest: ~/.zshrc
  mode: "0644"
```

- Rendered in the step's scope. Undefined variable is an error at plan time.
- Plan: diff of rendered output against current file.
- Takes the same `mode`, `owner`, and `group` fields as `file` (§6.3), with
  the same meaning and the same sudo requirement.

**Directory mode.** When `src` names a directory, every file under it is
rendered into `dest`, keeping relative paths. Directories are created as
needed with mode `0755` (or `mode` masked to its directory form).

```yaml
template:
  src: ./templates/            # a directory
  dest: ~/.config/nvim/        # rendered tree root
  mode: "0644"                 # applied to every rendered file
```

- One step, one output line. `changed` when any file changed; the line
  reports the count (`changed  3 of 14`).
- Plan prints one diff per changed file, each headed by its relative path.
- `dest` must be a directory or absent. An existing regular file at `dest` is
  a step failure, not a tree silently flattened into one file.
- The source tree is walked in sorted order, depth first. A symlink anywhere
  in it is a validation error, positioned at `src`: the tool does not follow
  them and will not guess.
- A source file that is not valid UTF-8 is copied byte for byte rather than
  rendered. The validate walk already steps over it; apply still has to place
  it.
- Files are not required to end in `.j2`; every file is rendered. A `.j2`
  suffix on a source file is stripped from the destination name.
- Nothing under `dest` is removed. This is not a sync; a file deleted from
  the source tree stays on disk. Delete it with `file: {state: absent}`.
- This is the only iteration in the language, and it is not a loop
  construct: no `item`, no scope per file, no user-supplied collection.

### 6.5 `pkg`

Ensure packages are present or absent through one manager.

```yaml
pkg:
  names: [git, curl]           # or name: git
  state: present               # present | absent | latest
  manager: apt                 # apt | pacman | yay | brew | winget; default: first available
  cask: false                  # brew only
  update_cache: false          # apt update / pacman -Sy / brew update before install
```

- Changed when the manager's own query says the package was missing before
  and present after. One query for the whole set, one install call for the
  missing subset.
- The "and present after" half is **checked**, not assumed: `apply` re-runs
  the query after the install or remove call, and a name that is still
  missing — or, under `absent`, still present — **fails** the step naming it.
  An install call that exits 0 having done nothing is real: on Debian
  `apt-get install -y yarn` succeeds and installs nothing, because `yarn` is
  a virtual package `cmdtest` provides, and `dpkg-query` still reports the
  name absent. Without the check that step reports `changed` on every apply,
  forever. `plan` does not re-query: it installed nothing.
- **Default manager**, when `manager` is absent: the first of `pacman`,
  `apt`, `brew`, `winget` found on PATH. `yay` is never chosen by default — a
  plan that wants the AUR says `manager: yay` and means it.
- The queries carry **versions**, because `latest` has nothing to compare
  without them:

| Manager | Query |
|---|---|
| apt | `dpkg-query -W -f '${Package}\t${Version}\t${Status}\n'`, keeping only `install ok installed`. Plain `-W` also lists removed-but-config packages, which would read as present |
| pacman, yay | `pacman -Q`. Both read the same database, so `yay` queries with `pacman` |
| brew | `brew list --versions`, plus `brew list --cask --versions` when `cask: true` |
| winget | `winget list`, matched by id |

- `latest` upgrades; changed when the version from that query differs before
  and after. A named package that is **not installed** is installed first:
  `apt-get install --only-upgrade` silently skips one that is absent, and brew
  and winget error on it, so upgrading the whole list would report `ok` with
  the package still missing.
- Under `plan`, `latest` on an installed package is **`unknown`**: whether a
  newer version exists is not a question the local database answers, and plan
  does not go to the network to find out. A missing package is knowable, and
  is reported as a change.
- `latest` compares against the package lists the machine already has, unless
  `update_cache: true` says to refresh them first.
- A package may be named as its manager accepts it — brew takes
  `hashicorp/tap/packer` — and the query is matched by the last path segment,
  because brew then lists it as plain `packer`. The install call still gets
  the name as written.
- `update_cache: true` refreshes before the install call, and only when there
  is something to install. **Never under `plan`**: a dry run that mutates the
  package database is not a dry run.
- `absent` mirrors `present` with the manager's remove command.
- Plan: lists the packages that would be installed, by actual query.
- `sudo` is not implied. apt and pacman need `sudo: true`. brew and yay must
  **not** have it — both refuse to run as root — and both combinations are
  validation errors. apt, pacman or yay with `sudo: false` is not a validation
  error; the manager will say so itself.
- **`plan` answers even with no reachable root.** The query reads a local
  database and never escalates, plan never installs, and `update_cache` never
  runs under plan, so there is nothing for root to protect. Only `apply` needs
  the preflight. This is unlike `file`, where reading the target itself can
  require root.
- A missing manager binary is an error naming it.
- Repositories, taps, PPAs, AUR helpers: **not** part of `pkg`. They are
  `shell` steps with `unless`. See migration.md for recipes.

### 6.6 `service`

Ensure a service state under systemd (system or user) or launchd.

```yaml
service:
  name: tailscaled
  state: started               # started | stopped | restarted | reloaded
  enabled: true                # optional
  scope: system                # system | user (systemd only)
```

- Probe is `systemctl is-active` and `systemctl is-enabled`; changed when
  either differs before and after.
- `scope: user` runs `systemctl --user`, and must not carry `sudo: true`. The
  two mean opposite things, and the combination is a validation error.
- `restarted` and `reloaded` are always `changed`, and always `would change`
  under plan. Neither has a before-state to compare against.
- **Probes never escalate.** `systemctl is-active` and `is-enabled` answer
  for anyone, and asking for root to read a world-readable state is how
  `plan` stops working on a machine with a cold sudo credential. Only the
  mutations take the step's `sudo`.
- launchd: `launchctl print gui/$UID/<name>` for `scope: user` and
  `launchctl print system/<name>` for system asks whether it is loaded;
  `launchctl print-disabled <domain>` asks whether it is enabled, and is the
  probe `enabled` uses. `kickstart -k` is `restarted`, `bootout` is
  `stopped`.
- `enabled` is `launchctl enable`/`disable`, **not** `bootstrap`/`bootout`.
  `bootstrap` needs the path to a plist and a `service` step does not carry
  one — the unit was placed by a `file` or `template` step that knows where
  it went. `enable` takes a service target and no path.
- For the same reason a launchd unit must **already be bootstrapped** before
  `started` can reach it: `kickstart` cannot load a plist that was never
  loaded. The step that places the plist bootstraps it, the way a systemd
  unit file is followed by `cmd: [systemctl, daemon-reload]`.
- A launchd path on a machine that is not macOS is a validation error naming
  the fact that decided it. Windows is a validation error too: use `shell`
  with PowerShell.

### 6.7 `assert`

Fail the run if a condition does not hold. Never changes anything.

```yaml
assert:
  command: test -f ~/.ssh/id_ed25519      # exit 0 passes
  # or
  expr: hostname in ["x1", "mainpc"]
  msg: "wrong machine"
```

- Always reports `ok` or `failed`. Plan runs asserts unless `--plan-no-probe`.
- **A failing assert does not stop a `plan` walk.** Apply stops at the first
  failure; plan never does. Plan has not done the work, so an assert about
  work not yet done — "verify the key is present", before the step that
  writes it has run — is information, not a reason to hide the other 154
  steps. The run still exits 1, so a wrong-machine guard still fails loudly;
  it just prints the rest of the plan underneath itself.
- `retry` (§4) composes with `assert`, and is how a readiness gate is
  written: the assert is re-run on failure until it passes or attempts run
  out. `retry: {attempts: 30, delay: 2s}` is a 60-second wait-for-ready.
- **`retry` belongs to `apply`. `plan` evaluates an assert exactly once.**
  That same 60-second wait would otherwise be spent, on every plan, waiting
  for work plan has not done and is not about to do.

### 6.8 `git`

Ensure a checkout of a repository at a ref. Phase 6, D4 as amended
2026-09-08: the fleet had four shell clones and two copies of a sixty-line
clone-and-pin block before this existed.

```yaml
git:
  repo: https://github.com/tmux-plugins/tpm
  dest: ~/.config/tmux/plugins/tpm
  ref: v3.1.0                  # tag, sha, or branch; default: the remote's default branch
```

- **State is "HEAD of `dest` is `ref`."** `dest` missing: clone, check out
  `ref`, `changed`. `dest` present: compare and converge.
- A **tag or sha** is immutable, so once it is on disk the answer needs no
  network: HEAD equals `ref^{commit}` reads `ok` without a fetch, and an
  offline machine converges. A tag that is not on disk yet is fetched first.
- A **branch** is mutable, so `apply` fetches and fast-forwards to
  `origin/<ref>`: behind is `changed`, level is `ok`. Under `plan` a branch
  ref on an existing checkout is **`unknown`**, for the reason `pkg`'s
  `latest` is (§6.5): whether the remote moved is not a question the local
  clone answers, and plan does not go to the network to ask. No `ref` means
  the remote's default branch and is treated as a branch. A tag or sha
  that is **not on disk yet** is `unknown` under plan for the same reason:
  whether HEAD already sits on that commit cannot be known without
  resolving the ref, and resolving it needs the network.
- Compared by `git rev-parse`, never by branch name: the ref name alone
  cannot tell a tag from a branch, and an annotated tag's ref resolves to the
  tag object, not the commit HEAD sits on. Tags are told apart with
  `rev-parse --verify refs/tags/<ref>`.
- **Fails**, never resets: a `dest` that is not a git repository, one whose
  `origin` is a different URL, or one with a dirty working tree. Provision
  does not throw away work it did not do. Detached HEAD at the right commit
  is fine and is what a tag checkout leaves behind.
- Full clones, no `depth`: a shallow clone breaks `describe` and tag
  comparison, and the fleet's repositories are small. A plan that needs
  shallow says so in shell.
- Plan: `clone <repo> at <ref>` when missing; `<sha> → <sha>` when a tag or
  sha differs; `unknown` for a branch as above.
- Runs `git` from PATH and fails with a clear message when it is absent.
  `sudo` is not implied and rarely wanted: the checkout belongs to the user
  running the plan.
- `retry` (§4) applies to the clone and fetch calls: the network is the one
  thing here that fails and then works.
- **Dirty** means tracked modifications or staged changes (`git status
  --porcelain --untracked-files=no` non-empty); untracked files are
  ignored, because a checkout cannot destroy one, and if one collides with
  the target ref git refuses the checkout itself and its message is the
  better error.
- Parents of `dest` are created the way `file` creates them (§6.3); `dest`
  itself must be absent or a checkout of the same `repo`.
- A repository that **rewrites its own tracked files** at runtime (zplug
  turns its tracked `init.zsh` into a symlink on first run) reads as dirty
  forever, and the guard cannot tell that from an edit. Such a checkout
  stays a `shell` clone with `creates`; the guard is not loosened for it.

### 6.9 `download`

Ensure a file fetched from a URL is at `dest`, and that it is the right
file. Phase 6, D4 as amended 2026-09-08: the fleet had twelve `curl -o`
steps, each hand-gated with `creates`, none of which would notice a wrong or
truncated download.

```yaml
download:
  url: https://github.com/tree-sitter/tree-sitter/releases/download/v0.26.8/tree-sitter-linux-x64.gz
  dest: ~/.cache/provision/downloads/tree-sitter-linux-x64.gz
  sha256: 9f2c…                # optional, strongly recommended
  mode: "0644"
```

- **State is "`dest` exists and matches `sha256`."** With `sha256`: `dest`
  missing or its hash differing is `changed`; matching is `ok`, with no
  network. Without `sha256`: `dest` present is `ok` and is never fetched
  again, because a URL is not state and provision will not re-download on
  every apply to find out. `sha256` is how a plan says the content matters.
- The fetch goes to a temporary file beside `dest`, is hashed, and is moved
  into place only when the hash matches; a mismatch **fails** the step
  naming both hashes, removes the temporary file and leaves `dest` as it
  was. A non-2xx response fails the same way. Redirects are followed.
- `mode` as `file` takes it (§6.3), quoted; parents are created like `file`
  does; `sudo: true` writes the way `file` writes, for the apt keyrings.
- **`sha256` is for immutable bytes at a stable URL**: a versioned release
  asset. A file that is meant to rotate — a vendor's apt signing key — is
  not that: pinning its digest fails every machine the day the vendor
  rotates, and without a digest `download` is the `creates`-gated curl it
  replaced, in more YAML. The apt keyrings stay in shell; their trust
  anchor, when one is added, is the key fingerprint, not a file digest.
- **Single files only.** An archive is downloaded by this action and
  unpacked by a `creates`-gated shell step after it; a `sha256` on the
  archive is what makes that pair sound. Unarchiving is deferred with its
  own reopen condition (plan.md).
- Plan: never fetches. `would change` when `dest` is missing or the hash
  differs; nothing else is knowable without the network and nothing else is
  claimed.
- **Transport is `curl`**, executed from PATH, with `-fsSL --retry 0`; the
  step's own `retry` (§4) repeats the whole fetch-and-hash. Every fleet OS
  ships curl (Windows since 1803) and a static binary that carries its own
  TLS stack and certificate handling is the wrong trade for a fetch the
  shell already does well. What the action adds is the hash and the
  verdict, not the transport. Absent curl fails with a clear message.
  Decided by review, owner to confirm.
- The step's `timeout` bounds the fetch.

### 6.10 `defaults`

Ensure a macOS preference key holds a value. Phase 6, D4 as amended
2026-09-08: the mac plans carried twenty `defaults write` lines in one
ungated shell step, which reported `unknown` on every apply and could not
say which key it had changed.

```yaml
defaults:
  domain: com.apple.finder     # or NSGlobalDomain
  key: AppleShowAllFiles
  type: bool                   # bool | int | float | string
  value: true
  current_host: false          # `defaults -currentHost`
```

- **State is "`defaults read domain key` parses as `value` under `type`."**
  bool reads as `1`/`0`, int and float as numbers compared numerically,
  string byte-for-byte. Key missing or differing is `changed` and writes
  with `defaults write domain key -<type> value`; equal is `ok`.
- `type` is required: `defaults` stores and prints values by type, and a
  `true` written as a string is a different key from one written as a bool.
- **Only these four types.** `array` and `dict` are deferred with a reopen
  condition (plan.md); the one array in the fleet stays in shell.
- Not macOS: **fails** at apply, `unknown` at plan, and under
  `validate --strict` a `defaults` step with no `when` naming the os is an
  **error**, the way an ungated `shell` is (D3). Strict has one severity;
  a second one is a bigger idea than this rule pays for. The action does
  not skip itself; a plan says where it runs.
- The write takes effect for the running user. An app that caches its
  preferences (Finder, Dock, SystemUIServer) needs a restart, which is a
  shell step gated on the register of the `defaults` steps before it, not
  something this action does behind the plan's back.
- Plan: reads only, reports `<old> → <new>` per key.
- `sudo` is not implied and is wrong here: a root write lands in root's
  preferences.

## 7. Privilege escalation

- `sudo: true` runs the action through `sudo -n` (non-interactive). If sudo
  needs a password, the run fails **before the first step** with a clear
  message, unless `--ask-sudo-pass` was given, in which case the password is
  read once from the terminal and fed via `sudo -S` for every escalated step.
- Environment is passed with `sudo --preserve-env=<the step's env keys>`.
- No `become_user`. Root or the current user. That is the whole matrix.
- The preflight can run "before the first step" because `apply` walks the
  plan twice: once exactly as `validate` does — rendering every field,
  resolving every file, touching nothing — and then again for real. The first
  walk is what finds the sudo steps, and it is why a plan with a typo in its
  last step fails before its first step runs.
- That first walk cannot know a `when` that reads a `register`, so it
  over-approximates: a sudo step the real walk will skip still arms the
  preflight. Asking for a password that goes unused beats failing halfway in.
- `plan` runs gates too (§4), so it also needs to know whether root is
  reachable — but it **asks rather than insists**. `plan` is the read-only
  command and must still work on a machine with a cold sudo credential, so a
  root gate it cannot run leaves its step `would run (unprobed)` rather than
  failing the run. D11 already has the word for what plan cannot see.

## 8. Commands

```
provision validate <file.yml> [--strict] [--prop k=v]... [--var k=v]... [--vars-file f]...
provision plan     <file.yml> [--prop k=v]... [--tags t,u] [--skip-tags t] [--var k=v]... [--vars-file f]... [--plan-no-probe] [--no-diff] [--json] [--hide-skipped] [--color when] [--deadline 30m]
provision apply    <file.yml> [same as plan] [--ask-sudo-pass] [--verbose] [--stream] [--keep-going]
provision list     <dir>/ | <component.yml>
provision facts    [--json]
provision --version
```

`--hide-skipped`, `--color` and `--deadline` are shared by `plan` and `apply`,
not `apply`-only: a plan is the output most worth quieting, and a plan runs
gates, so it has a wall clock worth bounding too. There is no
`provision version` subcommand — the flag is the whole of it.

- `validate`: parse, schema, template syntax, prop schemas, file existence
  for `import`/`use`/`vars_file`/`src`, **and the body of every action** — a
  `pkg` that names no package, a `file` with neither `content` nor `src`, a
  `mode` that is not a quoted octal string. No commands run. `--strict` also
  rejects `shell`/`cmd` steps with no idempotency gate.

  One exception, and only one: a `file` step's `src` after a `shell` or `cmd`
  step in the same walk. That command can create any path and nothing has run,
  so a source that is not there yet is unresolved rather than missing, and the
  check moves to the walk that executes (§6.3, D18). Every other path — and a
  `src` with no command before it — is checked here.

  `validate` is the subset of `plan` that runs nothing, not a weaker check.
  Anything `plan` would reject before touching the machine, `validate`
  rejects too.

  It is therefore evaluated against **this host's facts and PATH**. Resolving
  a `pkg` step's default manager asks what is on PATH, and a `service` step's
  backend follows the `os` fact, so a plan written for another machine can be
  rejected here for a reason that is true only here: a `service` step
  validated with `os` reading `windows`, or a `pkg` step naming no `manager`
  on a box that has none. This is the host-dependence a `when` gate has always
  had, now reaching one step further. Name the `manager` on a plan meant to
  validate anywhere.
- `plan`: everything `validate` does, plus render templates, evaluate `when`,
  probe `unless`/`creates`/`pkg`/`service`/`file` state, print what would
  change. Runs nothing that mutates. Exit 0 if nothing would change, 2 if
  something would.
- `apply`: plan, then execute. Stops at the first failed step. Exit 1 on
  failure.
- `--keep-going` (`apply` only): carry on past a failed step instead of
  stopping, so one run reports everything that is broken rather than the
  first thing. Every later step still runs, is reported, and counts in the
  summary; the exit code is 1 all the same. It does **not** apply to an
  interrupt — Ctrl-C is the operator saying stop, not a step saying it could
  not do its work — and it does not apply to the validation pass that runs
  before any step: a plan that will not render is not a plan to keep going
  with.

  It is off by default because a failed step usually invalidates what
  follows: a package that did not install makes every step configuring it
  fail too, and six failures are harder to read than the one that caused
  them. It earns its keep on a bare machine, where the point of the first
  run is the list.

- `--deadline <duration>` (`plan` and `apply`): a wall clock for the whole
  walk. `timeout` (§4) bounds one step, and twenty steps of ten minutes is
  three hours; a CI runner needs a bound it can state in advance on the run
  itself (D19).

  Two effects, and no third. Before each step, a clock that has run out stops
  the walk — the step does not run, and neither does anything after it, the
  same stop the first failure makes. And each step's effective `timeout` is
  the lesser of its own and the time left, so no step can run past the
  deadline it was admitted under. A step killed that way fails with
  `deadline exceeded after N`, naming the run's clock rather than a step
  timeout that is not in the file: the whole point of §4's per-step number is
  that a reader can find it, and a message pointing at one that was never
  written would send them hunting.

  The summary prints `deadline exceeded` and the run exits **124**, which is
  `timeout(1)`'s code and already what a killed child reports internally.
  `--keep-going` does not override it, for the reason it does not override
  Ctrl-C: the clock running out is not a step saying it could not do its
  work. `validate` takes no deadline — it runs nothing.

- **The root file** (D17, phase 5b): every one of `validate`, `plan` and
  `apply` takes a plan or a component. A file whose root is a sequence is a
  plan; one whose root is a mapping is a component — the distinction both
  parsers already make in their own error messages, so no command needs a
  flag saying which it got. A component as the root gets its props from
  `--prop k=v`; an unknown prop, a missing required prop or a wrong type is
  a validation error, the same three `validate` reports at a `use` site,
  and `validate` applies them too: a required prop with no value is an
  error at `validate`, not a placeholder.

  A `--prop` value is rendered as a template, then **read as the type its
  declaration gives it**: `string` as written, `int` and `bool` as YAML
  scalars, `list` as a YAML flow sequence. So `--prop count=3` is the int 3
  when `count` is declared `int`, and `--prop name=3` is the string `"3"`
  when `name` is declared `string`. "Wrong type" means the rendered text
  does not read as the declared one, and the error says what that type
  takes. This is deliberately **not** §3.4's sole-expression rule: that rule
  reads a type off the source text, and a command line has no way to carry
  one — the declaration is the only place it can come from. `--var` is
  unaffected and stays untyped strings.
  Decided by review and confirmed by the owner 2026-09-08.

  A `--prop` aimed at a **plan** is a usage error, not a value quietly
  dropped: a plan declares no props, and the caller who typed one believes
  it took effect. A **directory** given to any of the three is a usage error
  naming `list`, for the same reason — it is the listing's argument and
  nothing else's.

  Nothing else changes with the root's shape. The working directory is §4's
  one rule, the verdicts are §6's, the exit codes are the command's. A
  component run as the root means exactly what it means when a plan `use`s
  it. (Phase 5 had a `run` verb that changed three of those by itself —
  working directory, the ungated verdict, and whether `plan` applied. Phase
  5b withdrew it: the same file must mean the same thing under every verb.)

- `list <dir>/`: every `*.yml` in the directory, sorted by stem, one line
  each: the stem, then its `description` or nothing. Nothing below the root
  keys is parsed and nothing runs, so a directory holding one broken file
  still lists — a file that will not load, or whose root is not a component,
  prints `(not a component)` where a description would go rather than being
  silently dropped. Exit 0, or 3 if the directory is not there. The trailing
  separator is optional — the verb already says what the argument is; it was
  required only while the listing shared a verb with something else. This is
  the task runner's "what can I run here", and the only thing `list` does.

- `list <component.yml>`: the other half of that question — what one takes.
  The component's `description`, then its props, one line each: the name,
  the type, and either `required` or the default **as written** (§3.2: a
  default is data in the file and is never rendered, so the listing prints
  the four braces if that is what is there). A prop's own `description`
  follows on its own indented line, because descriptions are sentences. A
  component with no props says so.

  Nothing below the root keys is parsed and nothing runs, exactly as the
  directory form promises. Exit 0; exit 3 when the path is not there, when
  the file will not load, or when its root is a **plan** — a plan declares
  no props, `--prop` on one is already a usage error below, and asking one
  for its props is that same mistake and gets that same answer.

  This is `list` doing one thing at two granularities, not a second verb: it
  answers "what can I run, and how", and the directory form answered only
  the first half. Before it, the only way to learn a task's arguments was to
  open the file — which is fine for the author and useless for everyone
  else, including a runner.

- **A CI runner** wanting one result per step does not get a per-step entry
  point. It writes the job's steps to a file and runs one process,
  `provision apply job.yml --json`: the stream carries every step's status,
  exit code, captured output, duration and file:line as each step finishes
  (§9.3), and provision owns ordering, fail-fast, timeouts and the
  interrupt. Steps a runner generates from plain command lines carry
  `changed_when: false` so they read `ok` (§6.1).

  The rest of that contract is three things and is fixed by D19. **Cancel**
  by sending SIGTERM to the process: the current step's process group dies,
  the summary is still printed, and the exit code is `143`. **Bound** the
  job with `--deadline`, so the runner does not have to police a clock from
  outside with a SIGKILL that would lose the summary. And **check the job
  before spending a machine on it**: `provision validate job.yml` parses it,
  renders every template and resolves every path while running nothing, and
  `provision plan job.yml` says which steps would run — the same walk at
  three depths of commitment (§2), which is a pipeline linter no other step
  runner in this family ships.

  Everything else stays the runner's: what to check out, what to put in the
  environment, when to cancel, where the log goes, how long it is kept. Job
  facts — commit, branch, ref — arrive as `--var`, and are **not** facts
  (§5): facts are what the machine reports about itself, and a CI variable
  is not that.

Tag selection (Ansible semantics, deliberately):

- `--tags a,b`: run steps carrying `a` or `b`. Untagged steps are skipped.
  Steps tagged `always` run regardless.
- `--skip-tags a`: run everything except steps carrying `a`. It wins over
  `--tags`, `always` included.
- Tag filtering happens **after** `import`/`use` expansion, so a tag on an
  import applies to each imported step. `plan --tags x` must not fail on a
  step the tag excludes; excluded steps are not rendered.
- Filtering therefore runs **before** the step modifiers of §4: a step the
  tags exclude never has its `when` evaluated, never has its fields rendered,
  and so cannot fail on a variable it was never meant to read. The full order
  is: tags → `when` → `unless` → `creates` → action.
- **Structural steps are never filtered.** `vars`, `vars_file`, `import` and
  `use` build the scope every later step reads; skipping them by tag would
  silently unset variables the selected steps depend on. A tag on an `import`
  or `use` still propagates to the steps inside it, which is the point.

Exit codes: `0` ok / nothing to do · `1` failure · `2` plan found changes ·
`3` usage or validation error · `124` deadline exceeded · `130` interrupted
by Ctrl-C · `143` terminated by SIGTERM.

The last three are settled by **precedence**, because more than one can be
true of a single run: a stop from outside (`130`, `143`) beats the clock
(`124`), which beats a failed step (`1`), which beats found changes (`2`).
Read down the list, the rule is "the reason the run ended", and a signal is
always a better answer than whatever the walk was doing when it arrived.
`130` and `143` are the shell's `128 + signal`, which is where `130` already
came from.

`plan` exits 2 when any step is reported `would change`, `would run`,
`unknown`, or `would run (unprobed)` — anything that is not `ok` or `skipped`.
**`unknown` counts, and so does `unprobed`.** Both are provision saying it does
not know, which is not the same as nothing to do; a step provision cannot judge
is not a step it may call converged, and driving that count to zero is
precisely what `--strict` is for. `--plan-no-probe` is the one exception: it
inspects nothing, so it claims nothing, and exits 0 unless validation failed.

A malformed command line — an unknown flag, a missing argument, an unknown
subcommand — is exit `3`, like every other usage error, with the message and
usage text on stderr. `--help` and `--version` are exit `0` on stdout.

`apply` exits 1 on the first failed step, 0 otherwise. It does not exit 2;
having done the work, "changes were found" is not news. `plan` also exits 1
when a step failed — only an `assert` can, and §6.7 says why it keeps walking.

## 9. Output

### 9.1 TTY

One line per step, updated in place while running (spinner), then frozen:

```
  ✓ Install zsh                                    ok        0.4s
  ~ Deploy .zshrc                                  changed   0.0s
  - Generate SSH identity                          skipped   creates exists
  ? Add neovim PPA                                 unknown   1.2s
  ✗ Enable multilib                                FAILED    0.1s
    │ sed: can't read /etc/pacman.conf: No such file or directory
    │ (exit 2 · stderr, last 20 lines · --verbose for all)

  x1.yml · 5 steps · 1 changed · 1 ok · 1 skipped · 1 unknown · 1 failed · 1.7s
```

- Glyphs and colors: `✓` green ok, `~` yellow changed, `-` dim skipped, `?`
  magenta unknown, `✗` red failed, `→` yellow would run. `would change` is
  the plan-time twin of `changed` and carries the same `~` and the same
  yellow. `NO_COLOR` and `--color=never` honored.
- Nested `import`/`use` shown as a dim header line with the file name;
  steps indented one level. Depth capped at display; execution is flat.
- Skipped steps collapse to one dim line each; `--hide-skipped` drops them
  and the summary still counts them.
- Plan mode uses `would change` / `would run` in place of `changed`, and
  prints diffs under the line, indented, colored: unified, three lines of
  context. `--no-diff` suppresses them. The flag exists on `apply` too, where
  it does nothing, so that a script can pass the same arguments to both.
- The summary counts `would change` before `would run`.
- Retries render as `attempt 2/3` on the finished line. The spinner
  carries the step's name only; the attempt count is not known until the
  attempt that succeeded has finished.

### 9.2 Non-TTY

Same lines, no spinner, no color, no in-place updates. Failure output is
printed the same way. Suitable for CI logs.

### 9.3 `--json`

One JSON object per line on stdout, human output goes to stderr. `status` is
one of `ok`, `changed`, `unknown`, `skipped`, `failed`, `would_change`,
`would_run`, `would_run_unprobed`. `diff` is present only when there is one.
`rc`, `stdout` and `stderr` are the step's own command's and are present
exactly when the step ran one: `shell`, `cmd` and `assert`, whatever their
status, in full, not truncated. A typed action ran no command and carries
none; a skipped step ran none and carries none. A gate's (`unless`,
`creates`) result is never reported: it decides whether the step runs, it
is not the step's result (phase 5b; before it `rc` and `stderr` came only
with a failure). That is the whole of what a CI runner reads per step (§8),
so it is emitted as each step finishes, never batched:

```json
{"event":"step","index":3,"name":"Deploy .zshrc","status":"changed","duration_ms":12,"file":"components/zsh/index.yml","line":41,"diff":"..."}
{"event":"summary","changed":1,"ok":1,"skipped":1,"unknown":0,"failed":0,"duration_ms":1700}
```

The summary also carries `interrupted` and `deadline_exceeded`, both always
present and both booleans. A runner reading only the summary has to be able
to tell a job that finished from one that was stopped, and an absent key is
not that answer.

## 10. Edge cases

| Case | Behavior |
|---|---|
| Step has two action keys | Validation error with file:line |
| `import` cycle | Error naming the cycle |
| Template renders to empty `name` | Falls back to action + first arg |
| `unless` command not found | Error, not "run the step". `127` (not found) and `126` (not executable) from the interpreter fail the step, naming the gate |
| `unless` hits the step's timeout | Same rule, other cause: the step fails with `` `unless` timed out after 2.0s: <first line> ``. A gate that never finished told us nothing, and "the check did not answer" is not "the work is not done". Confirmed by the owner 2026-09-08: it turns a silent re-run into a failure, and that is the point |
| `creates` path contains `~` or template | Expanded, rendered, then checked |
| `sudo: true` on macOS with `pkg: brew` | Validation error |
| `retry` on a step that changed on attempt 2 | Reported changed, `attempt 2/3` in output |
| Timeout kills a `shell` with children | Process group killed, not just the shell |
| Plan probes with `--plan-no-probe` | `unless`/`assert`/`pkg` queries skipped, reported `would run (unprobed)` |
| `when` reads a `register` during `plan` | The step that registers it has not run, so the name holds a placeholder (`rc: 0`, empty output, `changed: false`) and the reader is reported `would run (unprobed)` rather than skipped. Plan does not claim a verdict it cannot have (D11) |
| Non-UTF-8 in stdout | Lossy in both the display and the `register`. Exact bytes were the earlier rule and were dropped: a `register` holding bytes breaks `result.stdout == "yes"`, which is the only thing a register is for |
| A step is skipped by `when` or by tags | Its `register`, if it declares one, still binds — with `skipped: true`. That field exists precisely so a later `when` can read it rather than fail on an undefined name |
| Ctrl-C mid-step | Current step killed, summary printed with `interrupted`, exit 130 |
| SIGTERM mid-step | The same path and the same word, exit **143**. A CI runner cancelling a job sends TERM, and so does systemd stopping a unit; with no handler provision died where it stood and left the step's process group running, which is the one thing the process-group discipline of §10's timeout row exists to prevent. `--keep-going` does not apply, for the reason it does not apply to Ctrl-C (D19) |
| SIGTERM on Windows | There is none. The handler is installed on unix only, and `130` stays the code for a stop that has no signal number to report |
| `--deadline` runs out between steps | The walk stops before the next step, which does not run and is not reported. Summary says `deadline exceeded`, exit 124 |
| `--deadline` runs out during a step | That step's `timeout` was already clamped to the time left, so it is killed on the run's clock and fails with `deadline exceeded after N` rather than naming a step timeout the file does not contain |
| `--deadline` shorter than a step's own `timeout` | The deadline wins; that is the clamp. The reverse — a step whose `timeout` expires first — is an ordinary step timeout and reads as one |
| `--deadline` and a step that already failed | Exit 124 only if the clock is why the run ended. A failure with `--keep-going` off stopped the walk first, so that run is exit 1 and the clock never ran out |
| Windows path in `dest` | Accepted; `~` expands to `%USERPROFILE%` |
| Two `vars_file` set the same key | Later wins. `--explain-var k` would show the chain — **spec'd, not built**: nothing in the fleet has needed it, and the owner ruled 2026-09-08 to leave it written down rather than build it or cut it |
| `timeout` and `retry` on one step | The timeout applies per attempt, not to the step |
| `retry` and `register` on one step | The register holds the last attempt, and its `rc` is that attempt's |
| `--stream` and `register` on one step | Output is teed: streamed live *and* captured |
| `assert` fails | `rc: 1` in `register`; the run stops like any other failure |
| `--keep-going` and a step whose `register` a later step reads | The register holds the failed step's own result — `rc`, output, and `failed: true` — so a `when` reading it sees what happened. Nothing is invented to stand in for work that did not happen |
| `--keep-going` and Ctrl-C | The run stops anyway, exit 130. The flag is about a step failing, not about the operator stopping |
| `--keep-going` on `plan` or `validate` | Not accepted: neither ever stopped at a failure to begin with |
| A step's `env` names a key `sudo` must preserve | `sudo --preserve-env` is given exactly the step's own `env` keys, nothing more |
| `--ask-sudo-pass` and a step that reads stdin | The wrapped command is `sudo -k -S`, so the timestamp is invalidated and sudo consumes the password line before the child is started. The child sees EOF, never the password |
| A `sudo: true` step's `unless` under `plan`, with no sudo | The gate is not run and the step is `would run (unprobed)`. Under `apply` this cannot arise: the preflight already failed |
| `file` target cannot be read and the step has no `sudo` | Step fails naming the path, and says `sudo: true` is how to read it. Guessing "it must differ" would rewrite a file nobody could compare |
| `file` target cannot be read under `plan` with no reachable root | `would run (unprobed)`, like a gate |
| `template` in directory mode with a regular file at `dest` | Step failure. A tree is not flattened into one file |
| `pkg` with `update_cache: true` under `plan` | The cache is not refreshed. A dry run that mutates the package database is not a dry run |
| `service` with `scope: user` and `sudo: true` | Validation error. The two mean opposite things |
| `service` on a launchd path on a machine that is not macOS | Validation error naming the fact that decided it |
| `service` with `enabled: false` on a static, indirect, generated or alias systemd unit | `is-enabled` exits 0 for all of those, so the step would change forever and `disable` is a no-op. Use `state:` alone on units that have no enablement to change |
| A typed action's command hangs | Killed on the step's `timeout` and reported `timed out after N`, the same as a `shell` step. Probes and mutations take the same path: a hung package manager hangs the query as readily as the install |

## 11. Validation of this spec

Before code: walk every step type in `~/dotfiles` (64 files, ~950 steps)
against §3–§6 and record each construct that has no mapping. The migration
document must list all of them. Zero unmapped constructs, or an explicit
decision to rewrite each, is the gate for starting Phase 0.

During build: each action ships with an integration test that runs it
twice on a scratch filesystem or container and asserts `changed` on the
first run and `ok` on the second. That test is the definition of
"idempotent" for this project.

After build: `provision plan` on all five machine plans reports zero
changes on a machine mooncake has already converged, except for steps the
migration document predicts.
