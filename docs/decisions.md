# Alignment — before the first line of code

Working name: **libretto** (free on crates.io; `quire`, `sotto`, `tome`, `folio` are taken).
Rename is one `sed` if you prefer `aloud` (also free) — everything else stays.

## 1. Language and toolchain

- Rust, edition 2024, `rust-version = "1.99"` (what is installed), single binary
- MSRV is a promise: CI has an `msrv` job that builds on the pinned version
- No `unsafe`: `#![forbid(unsafe_code)]` in `main.rs`
- Dependencies: clap (derive), serde, serde_json, toml, directories, reqwest (blocking + rustls),
  zip, blake3, anyhow, thiserror. ten dependencies, no frameworks
- Rejected on purpose: `confy` (no atomic write, no lock, stale), `rodio`/`symphonia`
  (decoding in-process is more code and worse device handling than mpv), any async runtime
  (nothing here is concurrent enough to pay for tokio)

## 2. Scope of v1

Reads EPUB v2/v3 aloud from a local library, remembers the position, works offline after first play.

- `libretto add <path>` — index an EPUB
- `libretto ls` — library with progress
- `libretto read <id|prefix>` — play from the stored position
- `libretto rm <id>`
- `libretto doctor` — mpv present? endpoint reachable? cache size?
- bare `libretto` — resume the last book
- Flags: `--voice --speed --endpoint --model --chapter --skip --no-cache --rewind`

Not in v1: PDF/MOBI/AZW3, m4b export, sync, accounts, telemetry, any engine that is not
OpenAI-compatible HTTP.

## 3. CLI only. No TUI.

A TUI costs ratatui + crossterm, an event loop, raw-mode and panic-hook handling, resize logic,
and tests that cannot be written against a terminal — 300–600 lines and a permanent bug class,
to show text that is already being spoken.

What we do instead, in two steps:
1. **v1:** one status line on stderr, updated in place: `ch 4/12 · 37/210 · af_heart 1.25x · 04:12/19:40`
2. **v0.6 (optional):** `--interactive` adds three keys (space, n, q) and nothing else, ~80 lines,
   off by default, skipped when stdin is not a tty

The rule: the terminal is for status, not for layout. Anyone wanting a TUI gets `--interactive`.

## 4. Architecture

Single crate, modules with a one-way dependency order:

```
cli.rs  ->  commands/  ->  store.rs | epub.rs | play.rs
                             |            |         |
                          text.rs      cache.rs   tts.rs
                                          \        /
                                           traits
```

- `traits.rs` — `TtsEngine` and `Player`, so Kokoro, Edge, or a fake in tests are interchangeable
- Errors: `thiserror` in each module, `anyhow` only in `main.rs`; every user-facing failure has an
  exit code (0 ok, 1 unknown book, 2 ambiguous, 3 tts down, 4 bad book, 5 already reading, 6 corrupt state)
- No panics on user input: no `unwrap` outside tests, `expect` only with an invariant comment
- SOLID in practice, not in slogans: one reason to change per module, dependencies point at traits,
  `text.rs` never opens a file, `store.rs` never does HTTP
- DRY where it is the same decision: one sentence iterator feeds both the cache key and the player,
  one path resolver owns every XDG path. Not DRY where it is only similar code
- Split point for later: if m4b export arrives, `epub`/`text`/`tts`/`cache` move to a `libretto-core`
  crate and the CLI stays a thin shell. Not before

## 5. Security standards

- No network except the configured TTS endpoint; URL must parse as http/https, no other scheme
- The only subprocess is `mpv`, resolved from `PATH`, argv built with `Command` — never a shell,
  never a path from the library file or config
- `add` canonicalises and opens the EPUB read-only; nothing is copied, nothing is rewritten
- Writes only under `~/.local/share/libretto`, `~/.cache/libretto`, `~/.config/libretto`
- Atomic writes (temp + rename) for `library.json`; a lock file for concurrent readers
- `cargo-audit` + `cargo-deny` in CI on every PR and weekly on a schedule
- Dependency rule: no crate that is yanked, unmaintained, or untouched for 12 months
- Tests never touch the network: a fake TTS server on a local port counts requests
- The `security-review` skill runs on every PR before merge, not only when we suspect something

## 6. Git and CI

- `main` is protected by review; one task per branch, named `feat/<task>-<slug>`
- Conventional commits (`feat:`, `fix:`, `chore:`, `test:`, `docs:`) — this is what drives the version bump
- One task = one PR = one review. No PR mixes a refactor with a feature
- CI on PR and on `main`: fmt, clippy `-D warnings`, test (Linux + macOS), cargo-deny, cargo-audit, MSRV
- Version, tag and CHANGELOG are automated by `release-plz` after merge — never hand-edited,
  so tags can't drift from what shipped
- Local hooks (`core.hooksPath=.githooks`): pre-commit = fmt + clippy, pre-push = test

## 7. Definition of done for any task

1. Spec AC it satisfies is named in the PR
2. Tests fail before the code, pass after
3. `cargo fmt --check`, `clippy -D warnings`, `cargo test` clean
4. `security-review` run, findings either fixed or written into the PR as accepted risk
5. No perf regression on `ls` (bench threshold 30 ms at 50 books)
