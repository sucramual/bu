# ADR-002: Use canonical Git and GitHub command-line adapters

### ADR-002: Use canonical Git and GitHub command-line adapters [status: accepted]

- **Context:** Pure Rust Git and GitHub clients exist, but native Git checkout and reference APIs make `bu` responsible for reproducing command-line worktree behavior and authentication.
- **Decision:** Rust owns policy and orchestration while subprocess adapters invoke installed `git` and authenticated `gh` commands with structured output.
- **Alternatives:** `gix` plus `octocrab`, `git2` plus a GitHub client, and hybrid native clients were considered.
- **Consequences:** V0 has fewer integration semantics to reproduce and depends on compatible installed `git` and `gh` versions. Adapter interfaces keep a later native implementation possible.
