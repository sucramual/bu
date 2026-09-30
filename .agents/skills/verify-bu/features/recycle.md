# Recycle

`bu recycle` fetches once, then resets eligible benches to their stand-in branch after guarded rechecks. `--force` discards changes in a dirty bench whose exact `HEAD` is a merged pull request.

## Sub-features

- `recycle-eligible` recycles clean, merged benches.
- `recycle-forceable` reports dirty merged benches as forceable and changes them only with `--force`.

## How to get to it (user POV)

- Run `bu recycle` or `bu recycle --force`.

## Driving it with cargo test

Preconditions:

- `cargo build` succeeds.

- **Recycle paths.** Run `cargo test --test status_e2e`. Every recycle test passes.

## Gotchas

- Never run `bu recycle` against the user's real benches as verification. It resets them.
