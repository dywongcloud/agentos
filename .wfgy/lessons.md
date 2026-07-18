# WFGY lessons

## 2026-07-18 — git_push gate correctly refuses: agentOS has no remote configured, and worktrees add a second blocker layer
Goal (G): Implement Holo auth check, holo-serve health-check loop, and macOS permission
preflight in holoiroh/mac-daemon/src/, verify cargo build succeeds, report to user.
What drifted / what went wrong: At CONSOLIDATE, git_push was denied twice -- first bare
(branch mismatch: worktree is on `worktree-wf_2b002703-24c-6`, not `main`), then again
when re-attempted, because the repo has zero git remotes configured at all
(`git remote -v` empty). This second fact was already recorded in a prior session's
memory (`agentos-no-git-remote`) but had to be independently re-confirmed this session
before trusting it as the real blocker rather than a transient/local issue.
Fix / resolution: Did not retry git_push a third time (would repeat the same denial --
the remote genuinely does not exist, no branch choice fixes that). Applied BBCR:
checkpointed at the last-known-good state (commit 1cb3d34, clean worktree, cargo build
verified passing), surfaced the blocker explicitly to the user instead of confabulating
a push, and left the PRD git-consolidation step correctly unresolved/blocked rather than
marking it falsely complete.
Generalizes to: In this repo (agentOS) specifically, any task that reaches CONSOLIDATE
should expect git_push to fail structurally until a human runs `gh repo create` +
`git remote add origin <url>` -- this is not worth retrying, only worth checking once
per session (`git remote -v`) and then reporting. Separately: worktree sessions add a
second, independent blocker (worktree branch != main) that resolves differently (pass an
explicit `{"branch": "main"}` or `{"branch": "<worktree-branch>"}`) and should not be
conflated with the no-remote blocker -- diagnose which one is actually firing before
picking a recovery action.
