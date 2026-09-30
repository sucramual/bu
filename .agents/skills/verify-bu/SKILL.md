---
name: verify-bu
description: Verify the bu CLI (Git worktree bench manager) through its real command-line surface. Use to check that a bu change still classifies worktrees correctly, to benchmark `bu prune --dry-run` speed against a baseline Git ref, or as the correctness gate in a performance loop.
---

# Verify bu

`bu` is a Rust CLI. Its user surface is the terminal: `bu status`, `bu recycle`, `bu prune`. This skill drives the real binary against the real configured repository for read-only commands. It uses the hermetic end-to-end tests for commands that change state.

Read [`features/README.md`](features/README.md) before driving a feature. It lists the preconditions and the per-feature recipes.

## Launch

`bu` is short-lived, so there is no server to start. Build once per candidate:

```bash
cargo build --release
```

The binary is `target/release/bu`. Do not use the installed `~/.cargo/bin/bu` as the candidate. It is whatever the user last installed, not the code under test.

## Doctor

Run this read-only check before driving:

```bash
.agents/skills/verify-bu/scripts/bench-prune.sh --doctor --baseline-ref main
```

It requires `cargo`, `perl`, an authenticated `gh`, a readable `~/.config/bu/config.toml` (override with `BU_CONFIG`), a configured repository that is a Git repository, and a baseline ref that exists. It prints `doctor: ok` on success and exits nonzero otherwise.

## Drive

**Correctness gate (always first):**

```bash
cargo test
```

`tests/prune_e2e.rs` and `tests/status_e2e.rs` build throwaway repositories with a fake `gh`. They are the only safe place to exercise real removals (`bu prune` without `--dry-run`, and `bu recycle`).

**Live prune benchmark and output comparison:**

```bash
BU_VERIFY_ARTIFACTS="$SCRATCH/bu-verify" .agents/skills/verify-bu/scripts/bench-prune.sh --baseline-ref main --runs 3
```

The script:

1. Builds the baseline ref from a `git archive` copy, cached per commit in `$BU_VERIFY_ARTIFACTS/cache/`.
2. Builds the candidate from the working tree, including uncommitted changes.
3. Copies the user's config and passes it with `--config`. Explicit config paths are never rewritten, so the live config cannot gain auto-discovered benches.
4. Runs `bu --config <copy> prune --dry-run --verbose --color never` from a neutral folder, as baseline → candidate → baseline in each round.
5. Counts a round only when both baseline outputs agree. It retries a round up to twice when other sessions change the live repository mid-round.
6. Compares candidate output byte for byte with the baseline, plus the exit code. If the only differences are inside `in use by N processes (…)` suffixes and the exit codes match, the round is process drift, not a mismatch. It is retried like a round whose baselines disagree.
7. Runs each binary once more with `git` and `gh` shims on `PATH` that count every subprocess call. Call counts are deterministic; wall time is not on a busy machine.

The script's exit code is `0` when every conclusive round matched, `1` on a mismatch, and `3` when no round was conclusive. Process drift never fails the run.

## Evidence

Each run writes `$BU_VERIFY_ARTIFACTS/<run-id>/`:

- `summary.txt`: baseline and candidate medians (min of the two baseline runs per round), speedup, `git`/`gh` call counts per binary, mismatch and inconclusive counts, and `process_drift_rounds`. That last count is rounds that still showed process drift after every retry. They are left out of the medians, like inconclusive rounds.
- `calls-baseline.log`, `calls-candidate.log`: one line per subprocess call.
- `times.tsv`: round, baseline seconds, candidate seconds, baseline exit, candidate exit.
- `baseline-*-a.txt`, `baseline-*-b.txt`, `candidate-*.txt`: full outputs.
- `diff-*.txt`: only present for a real mismatch.
- `drift-*.txt`: the diff for a round counted in `process_drift_rounds`.
- `state-before.txt`, `state-after.txt`: `git worktree list --porcelain` plus every `refs/heads` value. This is the read-only boundary for `--dry-run`. A change here while other sessions are active is expected. If it changes in a quiet repository, the dry run mutated state. Stop and report it.

## Cleanup

The script creates no processes that outlive it. It leaves artifacts and the baseline binary cache in place. Delete only the `<run-id>` folders you created, and only after reporting them. Never run `bu prune` without `--dry-run` against the real repository as part of verification.

## Gotchas

- The live repository is shared with other agent sessions. Outputs drift when someone commits or stages files mid-run. That is why the script runs baseline → candidate → baseline.
- Other sessions also start short-lived processes, such as `git`, inside worktrees. A `blocked` row can then read `in use by 4 processes (claude, disclaimer, git, zsh)` in one run and `3 processes` in the next. The script treats a difference as process drift only when every changed line differs just in that suffix. A row that changes label, such as `blocked` to `prunable`, is still a real mismatch. The two baseline runs must still match byte for byte.
- Wall time for identical code has varied 2× when the load average is high (other sessions, `cargo build`). Check `uptime` and trust call counts plus every-round wins over one median.
- `gh` latency varies from about 0.5s to 1s per call. Compare medians across at least 3 rounds before calling a change faster.
- Each round makes about 3 × (number of scratch worktrees) `gh` calls. Keep `--runs` modest to stay far below GitHub's hourly GraphQL limit.
- `git status` inside each worktree may take Git's optional index lock. It is harmless, but it can briefly contend with another session's Git command in the same worktree.
- A run inside a scratch worktree of the configured repository reports that worktree as blocked. The script avoids this by running from its artifacts folder.
