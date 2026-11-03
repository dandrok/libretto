---
feature: bookreader-cli
risk: [filesystem, spawn, third-party]
status: draft
owner: dandrok
assumptions:
  - "Kokoro-FastAPI stays OpenAI-compatible at /v1/audio/speech (ASSUMPTION: endpoint rewrite in one file if it changes)"
  - "mpv is present and used as the player rather than decoding audio in-process (ASSUMPTION: swap for a library later if mpv disappears)"
  - "Books are DRM-free EPUB 2/3 (ASSUMPTION: DRM input is refused, not supported)"
---

# bookreader — a CLI that reads an EPUB library aloud

## 1. Goal

A user can add EPUB files to a local library and have any book spoken aloud from
where they stopped, with no GUI and no cloud.

## 2. Scope

In scope:
- `bookreader add <path>` — import an EPUB, index its chapters
- `bookreader ls` — list the library with per-book progress
- `bookreader read <id|prefix>` — synthesize and play from the stored position, resuming on next run
- bare `bookreader` — resume the most recently read book
- `bookreader rm <id>` — remove an entry (not the source file)
- Local OpenAI-compatible TTS (`http://127.0.0.1:8880/v1` by default), with a disk cache
- Sentence and chapter skipping

Out of scope (explicit):
- PDF, MOBI, AZW3 — EPUB only in v1
- Any full-screen TUI, highlighting, or on-screen layout: output is a progress line on stderr
- Cross-device sync, accounts, telemetry
- Audiobook export (`.m4b`) — a v2 concern

## 3. Acceptance criteria

- AC-1: `bookreader add <valid epub>` creates exactly one library entry with non-empty `id`, `title` and `chapter_count > 0`
  test: `tests/cli_add.rs::add_creates_one_entry`
- AC-2: adding the same file twice leaves exactly one entry and exits 0
  test: `tests/cli_add.rs::add_is_idempotent_by_content_hash`
- AC-3: `bookreader ls` on an empty library exits 0 and prints the single line `no books`
  test: `tests/cli_ls.rs::ls_empty_library_exits_0`
- AC-4: `bookreader read <prefix>` matching two books exits 2, prints both ids, and starts no synthesis
  test: `tests/cli_read.rs::ambiguous_prefix_exits_2`
- AC-5: rerunning `read` on a 5-sentence chapter issues exactly 0 synthesis requests and reports 5 cache hits
  test: `tests/cache.rs::second_play_serves_from_cache`
- AC-6: after playing 5 sentences and quitting, the stored position is sentence 5 of the same chapter, and the next `read` starts there
  test: `tests/resume.rs::resume_starts_at_stored_sentence`
- AC-7: with the TTS endpoint unreachable, `read` exits 3, names the endpoint on stderr, and leaves the stored position unchanged
  test: `tests/tts_down.rs::unreachable_tts_exits_3`
- AC-8: `--skip 3` starts 3 sentences after the stored position and persists the advanced position
  test: `tests/skip.rs::skip_advances_and_persists`
- AC-9: the splitter emits no segment longer than 400 characters and never splits after `Mr.`, `Dr.` or `e.g.`
  test: `tests/split.rs::splitter_respects_abbreviations`
- AC-10: `bookreader ls` with 50 books in the library completes in under 30 ms of wall clock
  test: `tests/perf.rs::ls_50_books_under_30ms`
- AC-11: `bookreader add` on a file whose OPF has no title uses the file stem and exits 0
  test: `tests/cli_add.rs::add_falls_back_to_filename`
- AC-12: `bookreader rm <id>` on an unknown id exits 1, prints `unknown book`, and leaves the library unchanged
  test: `tests/cli_ls.rs::rm_unknown_id_exits_1`

## 4. Data changes

New files only, all under XDG paths; no existing data is touched.
- `~/.local/share/bookreader/library.json`
  - `version: u32`
  - `books: [{ id, title, author, source_path, content_hash, chapter_count, added_at }]`
  - `progress: { <book id>: { chapter_index, sentence_index, updated_at } }`
  - `last_read: Option<String>`
- `~/.cache/bookreader/<book id>/<chapter index>/<cache key>.mp3`
  - cache key = blake3 of `sentence_text + voice + speed + model`
- `~/.config/bookreader/config.toml` — `endpoint`, `model`, `voice`, `speed`, `prefetch`

## 5. Edge cases

- EPUB with no OPF title → filename stem, exit 0
- DRM/encrypted EPUB → exit 4, `encrypted epub not supported`, nothing written
- Empty spine → exit 4, `no readable chapters`
- Chapter containing only images → skipped, not counted in `chapter_count`
- Stored sentence index beyond chapter length → clamp to last sentence, continue
- TTS returns non-200 → two retries with backoff, then exit 3, position unchanged
- Two readers on the same book → second exits 5, `already reading`
- Library file corrupt → exit 6 with the path, never silently overwritten

## 6. Constraints

- Isolation: network calls go only to the configured TTS endpoint; no other outbound requests
- Data handling: the source EPUB is opened read-only and copied nowhere; only sentence text is sent, and only to the configured endpoint
- Subprocess: only `mpv`, resolved from PATH; never a command or path taken from the library file or config
- Limits: 400 characters per sentence; 5 sentences prefetched ahead; 3 TTS attempts; `ls` under 30 ms at 50 books
- Compatibility: Rust edition 2021, single binary, Linux first; no audio-decoding dependency in the build

## 7. Non-goals / no-change list

Must not change:
- Anything outside `~/.local/share/bookreader`, `~/.cache/bookreader`, `~/.config/bookreader`
- The user's EPUB files (read-only)

Non-goals:
- TTS engines other than OpenAI-compatible HTTP
- Word-level timing or karaoke highlight
- A daemon, server mode, or background prefetch after exit

## 8. Verification

```bash
cargo test
cargo clippy -- -D warnings
cargo build --release && time ./target/release/bookreader ls
python3 scripts/validate-spec.py docs/spec.md --tasks docs/tasks.json
```
