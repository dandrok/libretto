# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.12.5](https://github.com/dandrok/libretto/compare/v1.12.4...v1.12.5) - 2026-11-21

### Fixed

- fix the two things the reviewer found on this PR

### Other

- add ai-review to the pre-push hook and PR pipeline

## [1.12.4](https://github.com/dandrok/libretto/compare/v1.12.3...v1.12.4) - 2026-11-20

### Fixed

- the live line is rewritten in place, not under itself

### Other

- refresh the README and re-record both demos

## [1.12.3](https://github.com/dandrok/libretto/compare/v1.12.2...v1.12.3) - 2026-11-20

### Fixed

- announce an EPUB code listing instead of reading it

## [1.12.2](https://github.com/dandrok/libretto/compare/v1.12.1...v1.12.2) - 2026-11-20

### Fixed

- announce a table instead of reading its cells

## [1.12.1](https://github.com/dandrok/libretto/compare/v1.12.0...v1.12.1) - 2026-11-20

### Fixed

- a footnote is not spoken into the sentence that referenced it

## [1.12.0](https://github.com/dandrok/libretto/compare/v1.11.1...v1.12.0) - 2026-11-20

### Added

- refresh re-indexes a changed book instead of losing it

## [1.11.1](https://github.com/dandrok/libretto/compare/v1.11.0...v1.11.1) - 2026-11-16

### Other

- *(demo)* re-record both takes for the new wordmark, and say what a slow open does

## [1.11.0](https://github.com/dandrok/libretto/compare/v1.10.0...v1.11.0) - 2026-11-16

### Added

- *(ui)* say what the wait is for while a book opens

## [1.10.0](https://github.com/dandrok/libretto/compare/v1.9.1...v1.10.0) - 2026-11-16

### Added

- *(ui)* a wordmark that reads, and a test that proves it reaches the terminal

## [1.9.1](https://github.com/dandrok/libretto/compare/v1.9.0...v1.9.1) - 2026-11-15

### Other

- *(demo)* re-record both demos, and rewrite the README for reading

## [1.9.0](https://github.com/dandrok/libretto/compare/v1.8.0...v1.9.0) - 2026-11-15

### Added

- *(ui)* a bracketed meter instead of a cell per chapter
- *(follow)* a long sentence scrolls instead of growing the block
- *(ui)* the live line speaks the book's language
- *(config)* keep the sync offset in the file, where a machine property belongs
- *(follow)* tighten the underline against the voice

### Fixed

- *(ui)* the percentage sits right next to the cells
- *(ui)* one countdown, and the cells before the voice
- *(ui)* keep the line inside the width it was given
- *(cli)* let --sync-offset accept the sign it documents

## [1.8.0](https://github.com/dandrok/libretto/compare/v1.7.0...v1.8.0) - 2026-11-15

### Added

- *(follow)* the underline follows the voice, word by word

## [1.7.0](https://github.com/dandrok/libretto/compare/v1.6.0...v1.7.0) - 2026-11-15

### Added

- *(follow)* say when the book is held
- *(follow)* the underline follows the player, not a stopwatch

## [1.6.0](https://github.com/dandrok/libretto/compare/v1.5.0...v1.6.0) - 2026-11-15

### Added

- *(follow)* the read-along view and the command that draws it
- *(follow)* lay a sentence out across rows with one word lit

## [1.5.0](https://github.com/dandrok/libretto/compare/v1.4.0...v1.5.0) - 2026-11-15

### Added

- *(captions)* word timings from the engine, and a fallback when it has none

## [1.4.0](https://github.com/dandrok/libretto/compare/v1.3.0...v1.4.0) - 2026-11-15

### Added

- *(ls)* say how much listening is left

## [1.3.0](https://github.com/dandrok/libretto/compare/v1.2.2...v1.3.0) - 2026-11-15

### Added

- *(ls)* read the percentage off the stored sentence counts

## [1.2.2](https://github.com/dandrok/libretto/compare/v1.2.1...v1.2.2) - 2026-11-08

### Other

- *(demo)* the README shows the terminal presentation

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
