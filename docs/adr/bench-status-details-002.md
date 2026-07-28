# ADR-002: Print complete dirty-file details by default  [status: accepted]

- **Context:** A dirty skip currently tells the user that cleanup is required but not which paths caused it. Summary-only output remains compact, while generated trees can make complete output large.
- **Decision:** `bu status` prints every dirty path beneath its bench with no cap and no opt-in flag.
- **Alternatives:** A count plus a detail flag would keep default output short. A fixed cap would bound output but hide paths and introduce an arbitrary threshold.
- **Consequences:** One command provides the complete cleanup inventory. Very dirty benches can dominate terminal output, which is an explicitly accepted trade-off.
