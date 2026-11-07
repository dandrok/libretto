# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.2.1](https://github.com/dandrok/libretto/compare/v1.2.0...v1.2.1) - 2026-11-05

### Other

- the README describes 1.2.0

## [1.2.0](https://github.com/dandrok/libretto/compare/v1.1.2...v1.2.0) - 2026-11-04

### Added

- wordmark, sentence bar and a breathing indicator

## [1.1.2](https://github.com/dandrok/libretto/compare/v1.1.1...v1.1.2) - 2026-11-04

### Other

- version badge

## [1.1.1] - 2026-11-04

### Fixed

- `libretto add` names the file it could not read, and for a relative path that does
  not exist prints where it resolved. A missing path is bad input (exit 4), not a
  broken library (exit 6) — the two used to be reported as the same failure.
- `libretto read` steps over a fragment with nothing to pronounce — a bullet, a
  horizontal rule — instead of stopping the book on it. A run of five silent replies
  in a row still reports the endpoint as broken, so a dead server is not mistaken for
  a quiet book.

## [1.1.0] - 2026-11-03

### Added

- PDF import: `libretto add book.pdf` reads PDFs that have a text layer, with chapters
  taken from the running head, page numbers and figure captions dropped, wrapped lines
  rejoined and ligatures repaired.
- Scans and two-column layouts are refused with a reason instead of producing word
  salad. `cargo run --example chapters -- book.pdf` shows what an import produced
  before you listen to it.
- `tests/fixtures/*.pdf`: PDFs generated with ghostscript, so the PDF tests do not
  depend on a book anyone happens to own.

## [1.0.0] - 2026-11-03

First release. EPUB only; PDF is next.

### Added

- `libretto add <book.epub>` — indexes a DRM-free EPUB: chapters in spine order,
  running heads removed, sentences cut at 400 characters with abbreviations respected.
- `libretto ls` — the library with per-book progress; 50 books in 1.2 ms (release build).
- `libretto rm <id|prefix>` — removes one book and its position.
- `libretto read [id|prefix]` — speaks a book; any unique prefix works, and bare
  `libretto` resumes the last book read.
- Flags while listening: `--chapter`, `--skip`, `--rewind`, `--sentences`, `--voice`,
  `--speed`, `--endpoint`, `--model`, `--player`, `--prefetch`.
- Content-addressed audio cache: a sentence is synthesized once per
  text/voice/speed/model, so replaying a chapter costs no requests and works offline.
- `libretto doctor` — library, config, player, TTS endpoint and cache in one report,
  with the version of the player and the voice count the endpoint reports.
- Optional `config.toml`; command-line flags win over the file, the file over defaults.
- Exit codes a script can branch on: 0 ok, 1 unknown book, 2 ambiguous id, 3 TTS
  unreachable, 4 bad book, 5 already reading, 6 state problem.

### Security

- EPUB reads are bounded (16 MiB per entry, 64 MiB of prose per book) so a small
  archive cannot declare a huge allocation.
- Book-derived text cannot drive the terminal: control characters are stripped from
  everything printed, including titles that came from a hand-edited library file.
- The file behind a library entry is re-hashed before it is read; a swapped file is
  refused with exit 4 instead of being spoken.
- Library, cache and config are created `0600`/`0700` under the user's own XDG paths.
- Unknown keys in `config.toml` are an error, so a misspelled `voise` cannot silently
  keep the default voice.

### Notes

- Requires an OpenAI-compatible TTS endpoint (Kokoro by default) and `mpv`.
- Polish is not among Kokoro's languages yet; voices exist for English, Spanish,
  French, Hindi, Italian, Japanese, Portuguese and Chinese.
