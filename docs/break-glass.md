# Break glass: `apply` failed halfway

The machine needs to work again in five minutes. There is no rollback (D5)
and no record of what the last run did beyond what it printed (D9), so the
recovery is: know what state you are in, then pick the smallest of four
moves.

## 1. Stop, if it is still running

Ctrl-C. The current step's process group is killed, not just its shell,
the summary is printed with `interrupted`, and the exit code is 130
(spec §10). Nothing after that step runs. Do not `kill -9` provision: that
skips the process-group kill and can orphan whatever the step started.

## 2. What state the machine is in

- **Everything above the `✗` line happened. Nothing below it did.** `apply`
  stops at the first failed step (spec §8) unless `--keep-going` was given.
- **No template or path error got you here.** `apply` walks the whole plan
  once touching nothing before running its first step (spec §7). A plan that
  does not render fails before step one.
- **Root was reachable when the run started.** The same first walk finds
  every `sudo: true` step and checks for root before anything runs. Without
  `--ask-sudo-pass` that is a cached sudo credential, and a long run can
  outlive it: a `sudo: a password is required` on a late step is that, not
  a broken plan. Re-run with `--ask-sudo-pass`.
- **A `file` or `template` step without `sudo` either wrote the whole file
  or did not touch it.** It writes a temp file beside the destination and
  renames it (spec §6.3).
- **A `file` or `template` step with `sudo` may not have.** It places the
  file with `install`, which copies. If the run was interrupted on that step,
  look at the destination before trusting it.
- **A `shell` step did whatever its command did before it died.** provision
  cannot say more than the failure block under the `✗` line. Run the same
  command again with `--verbose` for both streams in full.
- **A package manager killed mid-transaction holds its own lock.** That is
  the manager's state, not provision's. Check nothing is still running, then
  use the manager's own repair: `sudo dpkg --configure -a` on apt; on pacman,
  remove `/var/lib/pacman/db.lck` only once no `pacman` process exists.

## 3. Pick the smallest move

In this order. Each is bigger than the one before it.

**a. Fix forward.** Read the failure block, fix that step in the plan, run
`provision apply` again. Steps that already converged report `ok` and cost
almost nothing; the run picks up where the machine actually is, not where a
state file says it is. This is the recovery model D5 chose, and it is
usually the five-minute answer.

**b. Step around it.** If the broken step is not what the machine needs
right now, comment it out and `apply` again — or, if it is tagged, pass
`--skip-tags <tag>`. Put it back once the machine works.

**c. Back out the change.** `git revert` (or `git checkout` the plan file
from the last good commit) and `apply` again. Read what this does and does
not do: it re-applies what the *old* plan declares. A `file` or `template`
the old plan owns is rewritten to the old content. Anything the old plan
never mentioned stays exactly as the failed run left it — a package the new
plan installed stays installed, a file it created stays created. provision
keeps no state, so it cannot know what to take away. If the new steps must
be undone, undo them by hand, or write the inverse as a step (`file` with
`state: absent`, `pkg` with `state: absent`).

**d. Fix it by hand.** provision holds no lock and no state on the machine,
so a manual fix is not something it fights — until the next `apply`, which
will re-converge anything the plan still declares. Make the plan agree with
the hand fix before running it again.

## 4. Afterwards

- `provision plan <file.yml>` exit 0 means the machine matches the plan.
  Exit 2 lists what does not.
- If you backed out with `git revert`, the fix still has to land. Re-apply it
  on a branch, and `plan` before `apply`.
- If fail-fast and re-run destroyed something — a re-run made it worse, not
  just not better — that is D5's overturn condition. Write it down as an
  issue with the plan, the output and what was lost.
