# ADR-001: Carry structured dirty-file observations  [status: accepted]

- **Context:** `GitAdapter::worktree_state` currently reduces stable porcelain output to `Clean` or `Dirty`. The formatter needs file paths and status codes, and raw line-oriented output exposes Git quoting while making rename handling ambiguous.
- **Decision:** Parse NUL-delimited porcelain v1 into `DirtyFile` values containing staged and unstaged status, destination path, and optional original path.
- **Alternatives:** Raw porcelain strings would require less code but would couple formatting to Git syntax. A second Git command during formatting would duplicate inspection and could observe a different worktree state.
- **Consequences:** The domain model becomes richer and the adapter owns a small parser. Paths with spaces, newlines, and rename pairs remain unambiguous, subject to the existing lossy UTF-8 conversion in `run_command`.
