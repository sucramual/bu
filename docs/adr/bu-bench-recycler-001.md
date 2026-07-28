# ADR-001: Keep v0 clean-only and reversible

### ADR-001: Keep v0 clean-only and reversible [status: accepted]

- **Context:** Recycling worktrees changes live Git state. Automatically preserving and restoring dirty work adds stash, conflict, untracked-file, and partial-recovery behavior before the successful path is proven.
- **Decision:** V0 skips dirty worktrees, preserves every feature branch ref, advances only a fast-forwardable `main-NN`, and switches only after an immediate safety recheck.
- **Alternatives:** Automatic stash and restore was deferred beyond v0. Force-resetting a stand-in or feature branch was rejected because it can remove the easiest pointer to local history.
- **Consequences:** The first release handles only the safe common case. It remains reversible and leaves complicated worktrees untouched.
