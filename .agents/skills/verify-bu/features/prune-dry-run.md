# Prune dry run

`bu prune --dry-run` lists every scratch worktree of the configured repository with one reason each, and changes nothing. A scratch worktree is any linked worktree that is not the main checkout or a configured bench.

## Sub-features

- `prune-dry-classify` gives each scratch worktree one row: prunable, stale, blocked, or skipped, with a reason.
- `prune-dry-verbose` adds `path:` lines and dirty file lists with `--verbose`.
- `prune-dry-summary` ends with `Checked N scratch worktrees`, the counts, and `Dry run: no changes were made`.
- `prune-dry-speed` finishes in a time the user accepts (the benchmark target).

## How to get to it (user POV)

- Run `bu prune --dry-run` from any folder.
- Run `bu prune --dry-run --verbose` for paths and dirty files.

## Driving it with bench-prune.sh

Preconditions:

- `bench-prune.sh --doctor --baseline-ref main` prints `doctor: ok`.

- **Classify and compare.** Run `.agents/skills/verify-bu/scripts/bench-prune.sh --baseline-ref main --runs 3`. `summary.txt` reports `output_mismatches=0` and the candidate output files end with `Dry run: no changes were made`.
- **Speed.** In the same run, `summary.txt` reports `candidate_median_s` and `speedup` against the baseline ref.
- **Read-only boundary.** Compare `state-before.txt` and `state-after.txt`. Any difference must be explained by another session's activity.

## Gotchas

- Row order is part of the output contract. A parallel implementation must print rows in `git worktree list` order.
- Other sessions change the live repository. Trust only rounds whose two baseline outputs agree.
- Other sessions also start short-lived processes in worktrees, so an `in use by N processes (…)` suffix can change between runs. The script counts a candidate that differs only in those suffixes as `process_drift_rounds`, not `output_mismatches`. Any other change, including a row label change, is still a mismatch.
- A failed `gh` call shows as a `failed` row. Check `gh auth status` before blaming the candidate.
