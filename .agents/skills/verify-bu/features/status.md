# Status

`bu status` prints one row per configured bench and a summary, and changes nothing.

## Sub-features

- `status-rows` gives each bench one row with its state.
- `status-verbose` adds paths, dirty files, and full diagnostics with `--verbose`.

## How to get to it (user POV)

- Run `bu status` or `bu status --verbose`.

## Driving it with the release binary

Preconditions:

- `cargo build --release` succeeds and a copied config exists.

- **Rows.** Run `target/release/bu --config <copy> status --color never` from a neutral folder. Exit code `0` and one row per `[[benches]]` entry.
- **Fixture check.** Run `cargo test --test status_e2e`. Every test passes.

## Gotchas

- Without `--config`, `bu` may append newly discovered sibling benches to `~/.config/bu/config.toml`.
