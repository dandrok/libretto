# AGENTS.md — how an agent works in this repo

Read `docs/spec.md` and `docs/decisions.md` before touching code.

- One task from `docs/tasks.json` per branch and PR. Do not fold two tasks into one branch.
- Do not invent scope. If the spec is wrong, change the spec in its own commit and say so.
- Tests come first and must fail for the right reason before the implementation exists.
- Never `unwrap()` outside tests, never `unsafe`, never a shell command string.
- No new dependency without a line in the PR saying why the standard library cannot do it.
- Run before every commit: `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings`.
- Run before every push: `cargo test`.
- Every PR states: the AC ids it closes, what it does not do, and the security-review result.
- Never commit a library file, an epub, a cache or a token. Fixtures are tiny public-domain EPUBs.
