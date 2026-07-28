# bu

`bu` reports and safely recycles explicitly configured Multiplier worktree benches.

## Commands

```text
bu status
bu recycle
```

`status` is read-only. `recycle` fetches the configured upstream once, then mutates only benches that pass every safety check.

## Configuration

Copy [`config.example.toml`](config.example.toml) to `~/.config/bu/config.toml` and list only the durable benches that `bu` may manage. Each bench maps to its own long-lived `main-NN` stand-in branch.

```toml
[repository]
path = "/Users/you/Documents/multiplier"
remote = "origin"
main_branch = "main"

[[benches]]
path = "/Users/you/Documents/multiplier-01"
standin_branch = "main-01"
```

Use `bu --config /path/to/config.toml status` to test a configuration without installing it.

## Safety model

`status` only runs read-only Git and GitHub queries. It reports each configured bench as eligible, skipped, or failed; ordinary skips do not make the command fail.

`recycle` first fetches `origin/main`. For each eligible bench, it rechecks cleanliness, normal Git operation state, branch attachment, one merged pull request, stand-in ownership, and fast-forwardability. It then atomically advances only the stand-in ref, switches to it without discarding changes, and verifies that the feature ref is unchanged and the worktree is clean. Any operational failure is reported per bench and produces a nonzero exit status.

## Verification

```text
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```
