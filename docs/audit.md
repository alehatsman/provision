# provision — dotfiles construct audit

Status: v1.0 · 2026-09-07 · the spec §11 pre-Phase-0 gate

Every step in `~/dotfiles` walked against spec §3–§6. This is the artifact
migration.md §4 says it is waiting on: "The Phase 0 gate (spec §11) will
extend this list; nothing is done until it is complete."

Measured at `~/dotfiles` commit `9548bbd` (after the dex drop). Method:
parse all YAML, classify every top-level step key as action, structural, or
modifier. Script: `scripts/census.py` reproduction in this doc's §5.

## 1. Census

**58 YAML files · 371 steps · 27 `.j2` templates.**

| Key | Uses | Files | Spec mapping |
|---|---|---|---|
| `import` | 66 | 16 | §3.1 `import` — unchanged |
| `shell` | 62 | 26 | §6.1 — unchanged |
| `file.write` | 52 | 15 | §6.3 `file` — rename, `directory`→`dir` |
| `pkg` | 34 | 16 | §6.5 — unchanged |
| `file.template` | 32 | 10 | §6.4 `template` — rename |
| `vars.load` | 26 | 14 | §3.1 `vars_file` — rename |
| `assert` | 24 | 13 | §6.7 — flatten `command.cmd` → `command` |
| `use` | 17 | 5 | §3.2 — unchanged |
| `os.service` | 10 | 3 | §6.6 `service` — **1 site needs `daemon_reload`** |
| `os.systemd` | 5 | 2 | **UNMAPPED — writes unit files** |
| `pkg.repo` | 4 | 4 | migration §3 recipe |
| `windows.scheduled_task` | 4 | 1 | migration §3 PowerShell recipe |
| `text.replace` | 3 | 2 | migration §3 recipe |
| `file.copy` | 3 | 2 | §6.3 `file` with `src` |
| `windows.firewall_rule` | 3 | 1 | migration §3 recipe |
| `windows.hyperv_firewall_rule` | 3 | 1 | migration §3 recipe |
| `stat` | 2 | 1 | **UNMAPPED** |
| `pip` | 2 | 1 | **UNMAPPED** |
| `git.clone` | 2 | 2 | `shell` + `creates` |
| `log` | 2 | 2 | **UNMAPPED** |
| `os.user` | 2 | 2 | **UNMAPPED** |
| `for_each` | 2 | 1 | **UNMAPPED — loops are a non-goal (§2)** |
| `for_each_file` | 1 | 1 | **UNMAPPED — loops are a non-goal (§2)** |
| `file.download` | 1 | 1 | `shell: curl` + `creates` |
| `wait.http` | 1 | 1 | delete with fleet-peer — but see §3.7 |
| `text.line` | 1 | 1 | migration §3 recipe |
| `pkg.upgrade` | 1 | 1 | drop (migration §4) |
| `cmd` | 1 | 1 | §6.2 — flatten |

Modifiers: `name` 272 · `tags` 272 · `as_user` 77 · `when` 57 ·
`unless_command` 39 · `timeout` 33 · `retry` 18 · `props` 10 · `vars` 8 ·
`as` 3 · `changed_when` 3 · `creates` 2 · `failed_when` 1.

All 77 `as_user` are `root` — D8 holds. All 26 `vars.load` and all 66
`import` are scalar paths. All 24 `assert` are `{command: {cmd: …}}`. All 8
step-level `vars` are bare structural steps, which spec §3.1 already allows.

## 2. Corrections to migration.md §2

Six counts in migration.md were measured before the dex drop or by hand.
Actual:

| migration.md says | Actual | Effect |
|---|---|---|
| `wait.http` (5) | 1 | one site, in `moongit`, not `fleet-peer` — see §3.7 |
| `container.image` (2) | 0 | row can be deleted; dex took them |
| `on_change:` (1 site) | 0 | row can be deleted; nothing to hand-edit |
| `file.copy` (4) | 3 | — |
| `git.clone` (3) | 2 | — |
| `os.service`/`os.systemd`/`service` (20) | 15 | 10 + 5 |

## 3. Unmapped constructs

Each needs a decision before Phase 0 opens. Recommendation given; the ones
marked **DECISION** change the spec if accepted.

### 3.1 `os.systemd` writes unit files — 5 sites — **DECISION**

`machines/x1/thermal.yml:155,174,215,231`, `components/moongit/index.yml:176`

```yaml
os.systemd:
  name: intel-rapl-cap.service
  unit:    { Description: "…", After: multi-user.target }
  service: { Type: oneshot, ExecStart: /usr/local/bin/intel-rapl-cap }
  install: { WantedBy: multi-user.target }
  enabled: true
  started: true
```

The action *generates* `/etc/systemd/system/<name>` (or
`~/.config/systemd/user/<name>` at `scope: user`), daemon-reloads, enables,
starts. Spec §6.6 `service` does none of that — it only takes
`{name, state, enabled, scope}` against a unit that already exists.

`components/moongit/index.yml:197` also uses `reload_on_change: true`
(restart the unit when the unit file changed), which needs the
write-then-act ordering in one step or a `register` chain.

**Recommendation: rewrite, do not extend the spec.** A unit file is a config
file; `template` already writes config files and already reports a diff.
Each site becomes three steps:

```yaml
- name: intel-rapl-cap unit
  template: { src: ./units/intel-rapl-cap.service.j2, dest: /etc/systemd/system/intel-rapl-cap.service, mode: "0644" }
  sudo: true
  register: rapl_unit

- name: Reload systemd
  cmd: [systemctl, daemon-reload]
  when: rapl_unit.changed
  sudo: true

- name: intel-rapl-cap
  service: { name: intel-rapl-cap.service, state: started, enabled: true }
  sudo: true
```

Cost: 5 sites → 15 steps + 5 small `.j2` files. Gain: the unit file gets a
real diff in `plan`, which `os.systemd` never showed, and `service` stays
four fields. `reload_on_change` falls out of `register` + `when` for free —
the same substitution D5 already prescribes for `on_change`.

### 3.2 `for_each_file` — 1 site, 14 files — **DECISION**

`components/nvim/index.yml:65`

```yaml
- name: Deploy config file {{ item.Path }}
  file.template: { src: "{{ item.Src }}", dest: "{{ config_path }}/{{ item.Path }}" }
  for_each_file: "./templates"
  when: not item.IsDir
```

Walks `components/nvim/templates/` (14 files, 3 levels) and renders each.
Spec §2 puts loops out of scope. This is the one site where the loop is not
sugar: dropping it means 14 explicit `template` steps that must be kept in
sync by hand with the directory.

Options:

- **a. 14 explicit steps.** Honest, greppable, no spec change. The tree has
  changed twice in two years; the drift risk is real but small, and a
  missing file is a `validate` error (§8 checks `src` existence), not a
  silent skip.
- **b. `template` grows a directory mode** (`src` is a dir → render the tree
  into `dest`, one step, one line of output, diff per changed file). One
  action gains one behavior; no loop construct enters the language.
- **c. Reopen loops.** Rejected — plan.md's reopen condition is "any step
  repeats itself more than 3 times by copy-paste", and this is one step.

**Recommendation: b.** It keeps §2's "no loops" intact, costs ~40 lines in
`template`, and directory-of-templates is a provisioning primitive, not a
general iteration facility. If b is rejected, a.

### 3.3 `for_each` over brew taps — 2 sites, 4 taps

`platforms/macos/packages.yml:141,151`

Both iterate the same 4-element `taps` var: one `pkg.repo` (already being
rewritten to a `shell` recipe per migration §3) and one `brew trust`. With
`pkg.repo` gone, both collapse into one shell step over a literal list —
no loop needed:

```yaml
- name: Tap and trust brew taps
  shell: |
    set -euo pipefail
    for t in hashicorp/tap borkdude/brew wata727/tflint wagoodman/dive; do
      brew tap | grep -qx "$t" || brew tap "$t"
      brew trust "$t"
    done
  changed_when: false
```

**Recommendation: rewrite as above.** No spec change. The `taps` var with
its `name` field exists only to feed `pkg.repo`; it goes too.

### 3.4 `os.user` — 2 sites

`platforms/arch/index.yml:130`, `components/zsh/index.yml:105`

Both set the login shell to zsh. `components/zsh/index.yml:117` already
asserts the result with a `dscl` command, and the example component in
`examples/components/zsh/index.yml` already shows the target form:

```yaml
- name: Default shell
  shell: chsh -s "$(command -v zsh)" "{{ username }}"
  unless: 'test "$(getent passwd {{ username }} | cut -d: -f7)" = "$(command -v zsh)"'
  sudo: true
```

macOS needs the `dscl` variant of the `unless`. **Recommendation: rewrite,
no spec change.** Add a row to migration.md §2 and a recipe to §3.

### 3.5 `stat` + `pip` — 4 sites, 1 file

`components/nvim/python_venv.yml:3,7,18,24`

The whole file builds two virtualenvs (python2 and python3) and pip-installs
neovim libs into each. `stat` + `as:` is a hand-rolled `creates`; `pip` is a
package manager the spec deliberately does not have (§6.5 lists five, none
of them pip).

Note the file is also **already broken**: line 14 reads
`when: not (python_venv_present and python3_venv_path)` — the second name is
the *path* var, always truthy, so the venv step never runs once the path is
set. This has been dead since it was written.

**Recommendation: delete the file and its import.** It is python2 tooling
for a neovim that has used the remote-plugin host optionally for years. If
the libs are still wanted, one step replaces all four:

```yaml
- name: neovim python host
  shell: python3 -m venv {{ python3_venv_path }} && {{ python3_venv_path }}/bin/pip install pynvim
  creates: "{{ python3_venv_path }}/bin/pynvim"
```

Confirm before deleting — this is the only site whose disposition is
"remove functionality", not "rewrite".

### 3.6 `log` — 2 sites

`platforms/windows/bootstrap.yml:352` (next-steps banner),
`components/moongit/index.yml:248` (token-minting hint)

Prints a message; never changes anything. Spec has no action for it.

**Recommendation: no spec change.** The windows one is operator
documentation at the end of a bootstrap — it belongs in
`platforms/windows/README.md`, not in the run. The moongit one becomes:

```yaml
- name: "Note: mint an API token to use /api/*"
  shell: echo "moongit up on {{ props.moongit_addr }} …"
  changed_when: false
```

`shell` + `changed_when: false` is a `log` action, and costs nothing.

### 3.7 `wait.http` — 1 site

`components/moongit/index.yml:237` — polls `/healthz` for 60s after the
service starts. migration.md §2 says "delete; fleet-peer component goes
away", but this site is in `moongit`, which migration §4 says to **keep**.

**Recommendation: `assert` with a retry.** The readiness gate is real — the
steps after it mint a token against the daemon.

```yaml
- name: Wait for moongit to become ready
  assert:
    command: curl -fsS -o /dev/null http://127.0.0.1{{ props.moongit_addr }}/healthz
    msg: "moongit did not become ready"
  retry: { attempts: 30, delay: 2s }
```

Needs one spec clarification: **does `retry` apply to `assert`?** §4 defines
`retry` as "Re-run on failure" for any step, and §6.7 says `assert` reports
`ok` or `failed` — so yes, by composition. Worth an explicit line in §6.7.

### 3.8 `file.write` `group:` — 2 sites

`shared/bootstrap.yml:58` (`group: root`), `:78` (`group: wheel`)

Both are the sudoers NOPASSWD drop-in, where the group genuinely matters
(`0440 root:wheel` on macOS). Spec §6.3 has `owner`, no `group`.

**Recommendation: add `group` to `file` and `template`.** It is one field,
it rides the same `install(1)` path as `owner` (§6.3 already installs with
owner and mode under sudo), and the alternative is a `shell: chgrp` step
next to every `file` step that needs it. Spec edit: one row in §6.3.

### 3.9 `os.service` `daemon_reload:` — 1 site

`platforms/windows/index.yml:89`

**Recommendation: rewrite as `cmd: [systemctl, daemon-reload]` before the
`service` step** — the same pattern §3.1 uses. No spec change.

### 3.10 `shell: {cmd, creates}` — 2 sites

`creates` nested inside the `shell` body rather than on the step. Spec §6.1
long form takes `{script, interpreter, login}`; `creates` is a step
modifier (§4). Purely mechanical: lift it one level. `shell.cmd` → 
`shell.script` is the same rename migration §2 already lists.

## 4. Template compatibility

All 27 `.j2` files parse as Jinja2. Tags used: `if`/`endif` (6),
`verbatim`/`endverbatim` (4), `for`/`endfor` (1), `else` (1). Filters used:
`expanduser` (18) and `default` (1). No Go-template constructs, no
mooncake-only filters.

Spec §3.4 covers all of it: `expanduser` is a listed custom filter,
`default` and `verbatim` are minijinja built-ins. **Zero template rewrites.**

## 5. Reproducing the census

```python
import yaml, os, collections
ROOT = os.path.expanduser("~/dotfiles")
MOD = {"name","when","unless_command","unless","creates","as_user","sudo","timeout",
       "retry","env","cwd","tags","as","register","changed_when","failed_when",
       "on_change","optional","props","raw","for_each","for_each_file","vars","id"}
c = collections.Counter()
for dp, _, fns in os.walk(ROOT):
    if ".git" in dp: continue
    for fn in fns:
        if not fn.endswith((".yml", ".yaml")): continue
        doc = yaml.safe_load(open(os.path.join(dp, fn)))
        steps = doc if isinstance(doc, list) else (doc or {}).get("steps") or []
        for st in steps:
            if isinstance(st, dict):
                c.update(k for k in st if k not in MOD)
print(c.most_common())
```

## 6. Gate verdict

**Closed 2026-09-07.** All four decisions taken by the owner; every one
followed the recommendation.

| § | Item | Decision | Spec change |
|---|---|---|---|
| 3.1 | `os.systemd` unit generation, 5 sites | rewrite as `template` + `cmd` + `service` (migration §3.8) | no |
| 3.2 | `for_each_file`, 14 nvim templates | `template` gains directory mode | **yes — spec §6.4** |
| 3.3 | `for_each` over brew taps, 2 sites | one shell step over a literal list (migration §3.6) | no |
| 3.4 | `os.user`, 2 sites | `shell: chsh` + platform `unless` (migration §3.5) | no |
| 3.5 | `python_venv.yml` (`stat` + `pip`), dead code | delete the file and its import | no |
| 3.6 | `log`, 2 sites | `shell: echo` + `changed_when: false`; banner to a README | no |
| 3.7 | `wait.http`, 1 site | `assert` + `retry` (migration §3.10) | **yes — spec §6.7 wording** |
| 3.8 | `file` `group:`, 2 sites | add the field | **yes — spec §6.3** |
| 3.9 | `os.service` `daemon_reload:`, 1 site | preceding `cmd: [systemctl, daemon-reload]` | no |
| 3.10 | nested `shell.creates`, 2 sites | lift to a step modifier | no |

Spec §6.3, §6.4, and §6.7 were amended accordingly. migration.md was
raised to v1.0: §2 counts corrected, §3 recipes numbered 3.1–3.10, §4 hand
edits completed.

**Zero unmapped constructs remain. Phase 0 is open.**
