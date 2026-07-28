# ADR-003: Configure every managed bench explicitly

### ADR-003: Configure every managed bench explicitly [status: accepted]

- **Context:** The Multiplier repository has durable benches alongside many temporary Codex, Claude, and Cheese worktrees. Scanning every Git worktree would include unmanaged targets.
- **Decision:** `~/.config/bu/config.toml` explicitly maps the repository and every managed bench path to its `main-NN` stand-in branch.
- **Alternatives:** Path-pattern discovery, Git-metadata discovery, and per-run command-line paths were considered.
- **Consequences:** Managed scope is reviewable and safe. Adding or removing a bench requires a configuration edit.
