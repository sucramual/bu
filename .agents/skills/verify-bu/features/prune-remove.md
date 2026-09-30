# Prune removal

`bu prune` removes a scratch worktree and deletes its branch only when the worktree is clean, unlocked, attached to a non-protected branch, and GitHub reports exactly one merged pull request at local `HEAD`. It rechecks every condition just before each removal.

## Sub-features

- `prune-remove-merged` removes the worktree and compare-and-deletes the branch.
- `prune-remove-recheck` refuses when state changed between the scan and the removal.
- `prune-remove-branch-kept` keeps a branch whose ref moved.
- `prune-remove-stale` cleans metadata for missing folders, unless a bench or the main checkout is also missing.
- `prune-remove-config` removes the `branch.<name>` config section.

## How to get to it (user POV)

- Run `bu prune`.

## Driving it with cargo test

Preconditions:

- `cargo build` succeeds.

- **All removal paths.** Run `cargo test --test prune_e2e`. Every test passes. The fixtures build throwaway repositories and put a fake `gh` first on `PATH`.

## Gotchas

- Never run `bu prune` without `--dry-run` against the user's real repository to verify a change. Removal deletes ignored files and the worktree's reflog.
- A speed change must keep the per-worktree recheck immediately before removal (see `docs/adr/scratch-worktree-prune-001.md`).
