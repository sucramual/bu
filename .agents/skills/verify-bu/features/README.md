# bu verification map

This directory is the maintained source for verifying what a `bu` user sees in the terminal. Read this index first, then use the matching feature file as the recipe.

| Feature | File | Driven against |
|---|---|---|
| Prune dry run | [prune-dry-run.md](prune-dry-run.md) | Live configured repository (read-only) |
| Prune removal | [prune-remove.md](prune-remove.md) | Hermetic fixtures in `tests/prune_e2e.rs` |
| Status | [status.md](status.md) | Live configured repository (read-only) |
| Recycle | [recycle.md](recycle.md) | Hermetic fixtures in `tests/status_e2e.rs` |

## Baseline preconditions

- Build the candidate with `cargo build --release` and drive `target/release/bu`, not the installed `bu`.
- `.agents/skills/verify-bu/scripts/bench-prune.sh --doctor` prints `doctor: ok`.
- Pass a copied config with `--config` so the user's live config is never rewritten.
- Run live commands from a folder outside every worktree of the configured repository.

## Driving conventions

- Read-only commands (`status`, `prune --dry-run`) may run against the live repository.
- Commands that change state (`prune`, `recycle`) run only inside the hermetic `cargo test` fixtures.
- Use `--color never` so outputs compare byte for byte.

## Proof and skip reporting

- CLI proof is the exact command, stdout, and exit code, saved under `$BU_VERIFY_ARTIFACTS/<run-id>/`.
- Read-only proof includes the before and after `git worktree list --porcelain` and `refs/heads` snapshots.
- Report a feature verified only through its fixture tests as "fixture-verified", not "live-verified".

## Feature entry contract

Each feature file starts with an H1 title and one paragraph. It then has exactly four H2 sections: `Sub-features`, `How to get to it (user POV)`, `Driving it with <harness>`, and `Gotchas`.
