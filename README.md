# bu

`bu` reports and safely recycles durable Git worktree benches. It also removes scratch worktrees after their work has merged on GitHub.

## Commands

```text
bu status
bu status --verbose
bu status --color auto|always|never
bu recycle
bu recycle --force
bu recycle --color auto|always|never
bu prune --dry-run
bu prune
bu prune --verbose
bu prune --color auto|always|never
```

`status` is read-only. By default it prints one concise row per bench and a summary. Use `-v` or `--verbose` for configured paths, structured dirty files, and complete failure diagnostics. Status and recycle styling color only the leading marker; `auto` (the default) uses color only on a terminal without `NO_COLOR`, while `always` and `never` override that policy.

`recycle` fetches the configured upstream once, then mutates only benches that pass every safety check. A dirty bench is `forceable` only when exactly one merged pull request has a head commit equal to local `HEAD`. Ordinary recycle leaves it unchanged. `bu recycle --force` permanently discards its staged, unstaged, and untracked changes. Failed rows include the detailed diagnostic needed to understand the state and recover safely.

`prune` removes scratch worktrees. A scratch worktree is any registered linked worktree that is not the main checkout or a configured bench. It is prunable when it is clean, unlocked, attached to a non-protected branch, and has exactly one merged pull request whose head commit equals local `HEAD`. Squash merges qualify. `bu prune --dry-run` prints the classification without changes. Every other scratch worktree gets a one-line reason and stays untouched. `-v` adds full paths and dirty files.

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

A config file needs no `[[benches]]` entries. A `[repository]` table alone is enough to prune a repository without benches:

```text
bu --config ~/.config/bu/dotfiles.toml prune --dry-run
```

## Safety model

`status` only runs read-only Git and GitHub queries. When the default config is used, it may create the file or append newly discovered benches before reporting each managed bench as merged (and eligible to recycle), forceable, blocked, idle, or failed. Ordinary forceable, blocked, and idle benches do not make the command fail.

`recycle` first fetches `origin/main`. For each eligible bench, it rechecks cleanliness, normal Git operation state, branch attachment, one merged pull request, stand-in ownership, and fast-forwardability. It then atomically advances only the stand-in ref, switches to it without discarding changes, and verifies that the feature ref is unchanged and the worktree is clean.

With `--force`, the same immediate recheck must classify a dirty bench as forceable. `bu` then runs `git reset --hard HEAD` followed by `git clean -fd`. This deletes tracked and untracked changes without creating a stash; ignored files and existing stashes remain. Cleanup is never attempted when the pull request is missing, unmerged, ambiguous, or points to a different commit. A cleanup failure names the failed phase and reports the paths that remain. Any operational failure is reported per bench and produces a nonzero exit status.

`prune` never passes `--force` and never touches remote branches. It protects the main checkout, every configured bench, the configured main branch, every stand-in branch, and the worktree that contains the current directory. Immediately before each removal, it lists the worktrees again and repeats every check, including the pull-request lookup. It then runs `git worktree remove <path>` and deletes the branch with `git update-ref -d refs/heads/<branch> <expected-commit>`. After a successful delete, it removes the `branch.<name>` config section, so a later branch with the same name does not inherit the old upstream. A failure there is printed as a note and does not fail the run. If the branch moved, Git refuses the delete, `bu` reports the branch as `kept`, and its config stays. Registered worktrees whose folders are missing are cleaned by one `git worktree prune`, which removes metadata only. `bu` skips that prune when a configured bench folder is also missing. Ignored files in a removed worktree are deleted with its folder. A failed lookup, removal, or deletion is reported per worktree and produces a nonzero exit status. See `docs/adr/scratch-worktree-prune-001.md`.

## Verification

```text
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```
