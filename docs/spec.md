---
feature: libretto
risk: [filesystem, spawn, third-party]
status: implemented — v1.1.1
owner: dandrok
assumptions:
  - "Kokoro-FastAPI stays OpenAI-compatible at /v1/audio/speech (ASSUMPTION: endpoint rewrite in one file if it changes)"
  - "mpv is present and used as the player rather than decoding audio in-process (ASSUMPTION: swap for a library later if mpv disappears)"
  - "Books are DRM-free EPUB 2/3, or PDFs that have a text layer (ASSUMPTION: DRM input is refused, not supported)"
  - "Poppler's pdftotext is installed when PDF import is used; pdfinfo only supplies the PDF's title (ASSUMPTION: EPUB import needs neither)"
---

# libretto — a CLI that reads a book library aloud

This document is the contract each acceptance criterion is proven against. The reasons
behind the shape of the thing — why a CLI and not a TUI, why mpv and not a decoder, why
`ureq` and not `reqwest` — are in `decisions.md`.

## 1. Goal

A user can add EPUB and PDF books to a local library and have any book spoken aloud from
where they stopped, with no GUI and no cloud.

## 2. Scope

In scope:

- `libretto add <path>` — import an EPUB, or a PDF with a text layer, and index its chapters
- `libretto ls` — the library with per-book progress
- `libretto read [id|prefix]` — synthesize and play from the stored position, resuming next run
- bare `libretto` — resume the last book read
- `libretto rm <id|prefix>` — remove an entry (never the source file)
- `libretto doctor` — library, config, player, endpoint and cache in one report
- Local OpenAI-compatible TTS (`http://127.0.0.1:8880/v1` by default), with a disk cache
- Sentence and chapter skipping, and rewinding

Out of scope (explicit): MOBI/AZW3, `.m4b` export, a full-screen TUI, cross-device sync,
accounts, telemetry, any TTS engine that is not OpenAI-compatible HTTP, word-level
highlighting.

## 3. Acceptance criteria

Every criterion below names the test that proves it, and every one of those tests passes
on the tagged tree.

- AC-1: `add <valid epub>` creates exactly one library entry with non-empty `id`, `title`
  and `chapter_count > 0` — `tests/cli_add.rs::add_creates_one_entry`
- AC-2: adding the same file twice leaves one entry and exits 0 —
  `tests/cli_add.rs::add_is_idempotent_by_content_hash`
- AC-3: `ls` on an empty library exits 0 and prints `no books` —
  `tests/cli_ls.rs::ls_on_an_empty_library_exits_0_and_says_no_books`
- AC-4: `read <prefix>` matching two books exits 2, lists both ids, and synthesizes nothing —
  `tests/cli_read.rs::an_ambiguous_prefix_lists_the_candidates_and_exits_2`
- AC-5: replaying a chapter issues 0 synthesis requests and reports every sentence as a
  cache hit — `tests/cache.rs::second_pass_over_a_chapter_issues_no_requests`
- AC-6: after playing part of a chapter, the stored position is the next sentence and the
  next `read` starts there — `tests/cli_read.rs::resume_continues_from_the_stored_sentence`
- AC-7: with the endpoint unreachable, `read` exits 3, names the endpoint, and leaves the
  position unchanged — `tests/cli_read.rs::a_dead_tts_endpoint_exits_3_and_leaves_the_position_alone`
- AC-8: `--skip N` starts N sentences after the stored position and persists the advance —
  `tests/cli_read.rs::skip_advances_the_position_without_playing_what_it_skips`
- AC-9: the splitter emits no segment longer than 400 characters and never splits after
  `Mr.`, `Dr.` or `e.g.` — `tests/split.rs::splitter_respects_abbreviations`,
  `tests/split.rs::splitter_never_exceeds_the_limit`
- AC-10: `ls` with 50 books completes under 30 ms of wall clock —
  `tests/perf.rs::loading_and_summarising_fifty_books_is_under_the_budget` (in-process) and
  `tests/perf.rs::ls_prints_fifty_books_and_stays_within_its_budget` (the spawned binary)
- AC-11: `add` on an EPUB whose OPF has no title uses the file stem and exits 0 —
  `tests/cli_add.rs::add_falls_back_to_the_file_name_when_the_epub_has_no_title`
- AC-12: `rm` on an unknown id exits 1, prints `unknown book`, leaves the library unchanged —
  `tests/cli_ls.rs::rm_unknown_id_exits_1_and_says_unknown_book`
- AC-13: `add <pdf with a text layer>` produces chapters from the running heads, and the
  prose read aloud carries none of the page furniture —
  `tests/pdf.rs::a_pdf_is_indexed_with_the_chapters_its_running_heads_describe`,
  `tests/pdf.rs::reading_a_pdf_speaks_the_prose_and_not_the_page_furniture`
- AC-14: a stored title containing control characters cannot drive the terminal —
  `tests/cli_read.rs::a_title_with_control_characters_cannot_drive_the_terminal`
- AC-15: a source file changed since it was added is refused with exit 4 instead of spoken —
  `tests/cli_read.rs::a_changed_book_file_is_refused_instead_of_read`
- AC-16: on a terminal, `read` draws the wordmark once, a sentence bar with a percentage, and
  a slow breath (◡ ∘ ⊙ ◎ ⊙ ∘) that moves only while the player holds a sentence; the same run piped emits the
  plain `\r` status line and no escape sequences —
  `tests/cli_read.rs::a_piped_read_stays_plain_and_quiet_is_accepted`,
  `src/ui.rs::tests::the_plain_line_matches_what_scripts_have_always_seen`
- AC-17: `--quiet` suppresses the wordmark, the bar and the breath even on a terminal, and
  changes no exit code —
  `tests/cli_read.rs::a_piped_read_stays_plain_and_quiet_is_accepted`,
  `src/ui.rs::tests::quiet_wins_over_a_terminal`

Proven at the store layer only: exit 5 for a second concurrent reader
(`src/store.rs::a_lock_held_by_this_process_is_respected`,
`src/store.rs::a_stale_lock_from_a_dead_process_is_recovered`). No integration test spawns
two binaries and asserts the code.

## 4. Data

New files only, all under XDG paths; nothing outside them is touched.

- `$XDG_DATA_HOME/libretto/library.json` — `version`, `books[]` (`id`, `title`, `author`,
  `source_path`, `content_hash`, `chapter_count`, `added_at`), `progress{}` per book
  (`chapter_index`, `sentence_index`, `updated_at`), `last_read`. File `0600`, directory
  `0700`, written through temp + rename, capped at 8 MiB before parsing.
- `$XDG_CACHE_HOME/libretto/audio/<shard>/<key>.mp3` — key is blake3 of
  `model \x1f voice \x1f speed \x1f text`. Deleting the cache is always safe.
- `$XDG_CONFIG_HOME/libretto/config.toml` — `endpoint`, `model`, `voice`, `speed`,
  `player`, `prefetch`; every key optional, unknown keys are an error. Flags win over the
  file, the file over the defaults.

An explicit `XDG_*` variable wins on every platform; an empty one is not an override.

## 5. Behaviour and edge cases

EPUB:
- chapters in spine order; running heads (the title reprinted at every chapter top) dropped;
  image-only chapters skipped and not counted; sentence splitting at 400 characters with
  `Mr.`, `Dr.`, `e.g.` and closing quotes respected
- no OPF title → file stem, exit 0; DRM/encrypted → exit 4, nothing written; empty spine →
  exit 4, `no readable chapters`

PDF:
- imported only when the text layer is dense enough (a scan is refused with a reason), and
  only when the plain and the layout-preserving extractions agree on word order — a
  two-column page interleaves and is refused rather than read as word salad
- chapters come from the line that appears at the top of a run of pages; page numbers,
  figure captions and the table of contents are dropped; lines broken mid-word are rejoined
  and ligatures repaired
- `pdftotext` missing → exit 6 with the install hint (a missing tool is state, not bad input);
  `pdfinfo` missing costs the title only; a text layer over 64 MiB → refused

Playback and synthesis:
- a fragment with nothing to pronounce (a bullet, a rule) is not a sentence; when the
  endpoint nonetheless answers `200` with zero bytes, that sentence is skipped and counted
  rather than ending the book — a run of five in a row surfaces as an endpoint error, so a
  dead server is not read as a quiet book
- **no retries**: an unreachable endpoint, a non-200 or an oversized reply ends the run with
  exit 3 and the position unchanged. The draft of this spec promised two retries with
  backoff; the code does not implement them, and the promise is removed rather than left
  standing
- bare `libretto` with no history exits 0 and says so; `--chapter N` ignores the stored
  position and starts at that chapter's first sentence; a stored sentence index past the end
  is clamped rather than treated as an error
- a second reader on the same library exits 5; a lock left by a dead pid is recovered
- a corrupt library is reported with its path and never overwritten (exit 6)

## 6. Constraints

- Isolation: the only outbound request is to the configured endpoint, which must parse as
  http(s).
- Subprocesses: `mpv` when reading; `pdftotext` and `pdfinfo` for PDF import only. Argv
  built with `Command`, never a shell string; no program name and no path is taken from the
  library file or the config.
- Writes: only the three XDG directories above. The source book is opened read-only.
- Limits: 400 characters per sentence; 32 MiB of audio per sentence; 16 MiB per EPUB entry
  and 64 MiB of prose per book; prefetch default 5, capped at 64; speed clamped to 0.25–4.0;
  `ls` under 30 ms at 50 books.
- Compatibility: edition 2024, MSRV 1.99, single binary, `#![forbid(unsafe_code)]`,
  no audio-decoding dependency in the build.

## 7. Verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check && cargo audit
cargo test --release --test perf     # the 30 ms budget, where it is meaningful
libretto doctor                      # the machine, not the code
```
