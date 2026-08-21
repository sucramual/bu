# bu

`bu` reports and safely recycles durable Git worktree benches.

## Commands

```text
bu status
bu status --verbose
bu status --color auto|always|never
bu recycle
bu recycle --color auto|always|never
```

`status` is read-only. By default it prints one concise row per bench and a summary. Use `-v` or `--verbose` for configured paths, structured dirty files, and complete failure diagnostics. Status and recycle styling color only the leading marker; `auto` (the default) uses color only on a terminal without `NO_COLOR`, while `always` and `never` override that policy.

`recycle` fetches the configured upstream once, then mutates only benches that pass every safety check. Failed rows include the detailed diagnostic needed to understand the state and recover safely.

## Installation and updates

Install `bu` from this checkout:

```text
cargo install --path . --force
```

Cargo copies the executable into its binary directory, normally `~/.cargo/bin`; it does not link the command to this checkout. After `git pull`, rerun the install command when the pull changes source code or dependencies. Documentation-, test-, and example-only changes do not require reinstalling it.

## Configuration

Run `bu status` from the repository or any of its worktrees. On the first run, `bu` creates `~/.config/bu/config.toml` with `origin` as the remote and `main` as the main branch.

Each default-config run discovers registered sibling worktrees named `<repository>-NN`, maps them to `main-NN`, and appends new ones to the config. The strict sibling name excludes temporary worktrees under `.codex`, `.claude`, `.cheese-worktrees`, and `/tmp`. Existing explicit entries remain managed.

```toml
[repository]
path = "/Users/you/Documents/example-repo"
remote = "origin"
main_branch = "main"

[[benches]]
path = "/Users/you/Documents/example-repo-01"
standin_branch = "main-01"
```

Use `bu --config /path/to/config.toml status` to test a fixed configuration without installing it. Explicit config paths are never created or updated automatically.

## Safety model

`status` only runs read-only Git and GitHub queries. When the default config is used, it may create the file or append newly discovered benches before reporting each managed bench as eligible, blocked, idle, or failed. Ordinary blocked and idle benches do not make the command fail.

`recycle` first fetches `origin/main`. For each eligible bench, it rechecks cleanliness, normal Git operation state, branch attachment, one merged pull request, stand-in ownership, and fast-forwardability. It then atomically advances only the stand-in ref, switches to it without discarding changes, and verifies that the feature ref is unchanged and the worktree is clean. Any operational failure is reported per bench and produces a nonzero exit status.

## Verification

```text
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```
