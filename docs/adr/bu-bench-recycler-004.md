# ADR-004: Discover only numbered sibling benches

### ADR-004: Discover only numbered sibling benches [status: accepted]

- **Context:** Explicit bench entries kept temporary worktrees outside `bu`, but the config became stale whenever a new durable bench was added.
- **Decision:** Default-config runs discover registered worktrees beside the repository whose names match `<repository>-NN`. Each match maps to `main-NN` and is appended to the generated config. Explicit `--config` files remain fixed.
- **Alternatives:** Scanning every registered worktree was rejected because it includes temporary agent worktrees. Requiring a manual sync command was rejected because it can become stale in the same way as manual config.
- **Consequences:** New durable benches are managed on the next run. Non-numbered, nested, and temporary worktrees remain excluded. Removed or manually configured entries remain in the config until the user removes them.
