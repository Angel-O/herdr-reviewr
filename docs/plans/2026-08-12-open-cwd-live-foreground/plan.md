# Open on the live foreground cwd: Plan

Delivers `specs/herdr-host.md#repo-discovery` by finishing PR #59 (external contribution, branch `fix/open-cwd-live-foreground`).

## Problem

Opening reviewr beside a pane running `claude -w <worktree>` reviews the main checkout's branch, not the worktree's. herdr's action context carries the pane's launch cwd, and `claude -w` chdirs into the worktree only inside its own process. PR #59 fixes the case but diverges from the locked design in two ways: a live cwd outside any git repo refuses an open the launch cwd could place, and the live read runs on every mode including close.

## Goal

PR #59 merged, matching the Repo discovery contract exactly: live foreground cwd when it lies in a git repo, launch cwd otherwise, refusal only when neither qualifies, and no live read off the open path.

## Definition of Done

- [ ] An open beside a `claude -w <worktree>` pane reviews the worktree (PR's existing test `open_prefers_the_focused_panes_live_foreground_cwd`).
- [ ] A live cwd outside any git repo falls back to the launch cwd instead of refusing (new test).
- [ ] A failed or empty live read falls back to the launch cwd (PR's existing test `open_keeps_the_context_cwd_without_a_live_foreground_cwd`).
- [ ] `close`, a closing `toggle`, and `auto-open` issue no `pane get` (new assertions on the fake-herdr call log).
- [ ] PR #59 merges with the contributor's commits intact.

## Out of Scope

- Refusal message wording. Builder's mechanism, spec owns only the refusal itself.
- The `docs/herdr-api-notes.md` addition. Already correct in the PR, lands as-is.

## Execution Plan

1. [ ] `gh pr checkout 59`, then rebase onto `main` if behind.
2. [ ] In `herdr/pane.sh`: move the live-read block from before the mode dispatch to after it, just above the "Opening from here on" repo check, gated `[ "$mode" != auto-open ]`.
3. [ ] In the same block: accept `$live` only when `git -C "$live" rev-parse --show-toplevel` succeeds, else keep the context cwd.
4. [ ] In `tests/pane_actions.rs`: add `open_falls_back_when_the_live_cwd_is_not_a_repo` (paneget fixture whose `foreground_cwd` is a non-repo tempdir, context cwd a repo, assert `--cwd <repo>` in the call log).
5. [ ] In `tests/pane_actions.rs`: assert `pane get` absent from the call log in a close-path test and an auto-open test.
6. [ ] Commit on top of the contributor's commits and push to their branch (`maintainerCanModify` is true).

## Likely Files

| file                     | change                                                        |
| ------------------------ | ------------------------------------------------------------- |
| `herdr/pane.sh`          | relocate the live read to the open path, add the repo guard   |
| `tests/pane_actions.rs`  | one fallback test, two no-live-read assertions                |
| `specs/herdr-host.md`    | promote Repo discovery Draft to Current at the merge gate     |

## Verification

- `cargo test --test pane_actions` → all pass, including the PR's two existing tests unchanged.
- `just ci` → clean.
- Tight: everything the diff adds is exercised by a DoD line. Delete or defer the rest.
- Gate: high-effort `/code-review` loop on the branch until clean, then `/garfield` end to end, then promote `specs/herdr-host.md` to Current.

## Replan

- If pushing to the fork branch fails despite `maintainerCanModify`, then recreate the branch in this repo from their head with commits intact and retarget the merge.
- 2026-08-12: initial plan.
