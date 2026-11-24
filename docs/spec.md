---
feature: libretto
risk: [filesystem, spawn, third-party]
status: implemented — v1.12.4
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
- `libretto follow [id|prefix]` — the same, with the current sentence on screen and the word
  being spoken underlined
- bare `libretto` — resume the last book read
- `libretto rm <id|prefix>` — remove an entry (never the source file)
- `libretto refresh <id|prefix>` — re-index a book whose file changed since it was added,
  keeping the position
- `libretto doctor` — library, config, player, endpoint and cache in one report
- Local OpenAI-compatible TTS (`http://127.0.0.1:8880/v1` by default), with a disk cache
- Sentence and chapter skipping, and rewinding
- A listening budget that ends the run before the chapter does (`--for 30m`)

Out of scope (explicit): MOBI/AZW3, `.m4b` export, a full-screen TUI, cross-device sync,
accounts, telemetry, any TTS engine that is not OpenAI-compatible HTTP.

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
  `tests/cli_read.rs::on_a_terminal_the_wordmark_is_drawn_once`,
  `src/ui.rs::tests::the_plain_line_matches_what_scripts_have_always_seen`
- AC-17: `--quiet` suppresses the wordmark, the bar and the breath even on a terminal, and
  changes no exit code —
  `tests/cli_read.rs::a_piped_read_stays_plain_and_quiet_is_accepted`,
  `src/ui.rs::tests::quiet_wins_over_a_terminal`
- AC-18: `ls` reports a percentage taken from the per-chapter sentence counts stored at
  `add`, so a position inside a chapter is exact; an index written before those counts
  existed still lists at chapter granularity, and the first `read` of that book fills the
  counts in —
  `tests/cli_ls.rs::ls_reports_a_percentage_inside_the_chapter_the_reader_is_in`,
  `tests/cli_ls.rs::ls_falls_back_to_chapter_granularity_for_an_old_record`,
  `tests/cli_read.rs::read_fills_in_sentence_counts_for_a_library_added_before_them`,
  `src/store.rs::tests::a_percentage_lands_inside_the_chapter_the_reader_is_in`
- AC-19: `ls` and the opening line of `read` say how much listening is left, estimated from
  the characters stored per chapter at the measured 16.1 characters per second and scaled by
  the configured speed; an index written before those counts existed shows no estimate rather
  than a wrong one, and a finished book reads `done` —
  `tests/cli_ls.rs::ls_reports_how_long_is_left`,
  `tests/cli_ls.rs::ls_omits_the_estimate_for_an_index_written_before_the_counts`,
  `src/store.rs::tests::time_left_shrinks_as_the_reader_moves`,
  `src/estimate.rs::tests::a_sentence_takes_its_characters_at_the_measured_rate`
- AC-20: `follow` is a presentation of `read`: on a terminal it draws the sentence being spoken
  wrapped to the width of the terminal it is drawn on — `--width` when given, then the
  terminal's own size, then `$COLUMNS`, then eighty columns — with the word being spoken
  underlined, redrawing in place, and no row it paints is wider than that width, because a
  row the terminal soft-wraps occupies two rows while the painter counted one and every
  frame after it lands inside the block before; on a pipe or under `--quiet` it emits
  exactly what `read` emits — same words, no escape sequences, same exit code —
  `tests/cli_follow.rs::follow_on_a_pipe_is_read_byte_for_byte`,
  `tests/cli_follow.rs::follow_quiet_is_plain_and_says_where_it_stopped`,
  `tests/cli_follow.rs::follow_accepts_the_same_arguments_as_read`,
  `tests/cli_follow.rs::a_narrow_terminal_gets_rows_that_fit_it`,
  `src/follow.rs::tests::the_active_word_is_lit_on_the_row_that_holds_it`,
  `src/follow.rs::tests::a_long_sentence_wraps_without_breaking_a_word`,
  `src/ui.rs::tests::a_row_wider_than_the_width_counts_as_the_rows_it_takes`
- AC-21: with mpv as the player, `follow` draws the underline against mpv's own playback
  position over its IPC socket, so the underline stands still while playback is paused and
  moves again when it resumes, and the line shows a pause mark instead of the breath for as
  long as it is held; a player that reports no position falls back to a wall clock and its
  line cannot show a pause, and the socket lives in a private directory that is removed when
  the sentence ends —
  `src/play.rs::tests::the_position_is_read_from_the_socket_including_a_pause`,
  `src/play.rs::tests::the_player_is_told_where_the_socket_is`,
  `src/play.rs::tests::no_socket_and_no_position_are_the_same_answer`,
  `src/ui.rs::tests::the_player_position_beats_the_wall_clock`
- AC-22: `follow` asks a Kokoro-shaped endpoint for captioned speech — the `/dev` sibling of
  a `/v1` endpoint — and draws the underline from the engine's own per-word timings, with a
  punctuation-only token folded into the word before it so the list covers the printed
  sentence; those timings are cached beside the audio, and a warm sentence costs no request.
  An endpoint without the path, or a list that does not cover the sentence, interpolates by
  length instead and is not asked again in that run. `read` never asks —
  `tests/cache.rs::timings_are_stored_beside_the_audio_and_cost_nothing_to_replay`,
  `tests/cache.rs::a_cached_sentence_without_timings_is_not_re_synthesized_to_get_them`,
  `src/play.rs::tests::the_words_of_a_sentence_reach_the_caller_before_it_plays`,
  `src/tts.rs::tests::a_versioned_endpoint_has_a_captioned_sibling`,
  `src/captions.rs::tests::punctuation_of_its_own_belongs_to_the_word_before_it`
- AC-23: the read-along block never grows past the window it is given — the current sentence
  shows at most four of its rows, scrolling so the word being spoken is always one of them, and
  the neighbouring sentences stay one row each —
  `src/follow.rs::tests::a_sentence_longer_than_the_window_scrolls_with_the_voice`
- AC-24: opening a book on a terminal says what it is waiting for. The wordmark appears at
  once, before anything about the book is known, and — when the wait outlasts 150 ms — a single
  line under it names the file's kind and size with a mark that advances while the file is
  hashed and parsed, which is seconds for a PDF because `pdftotext` converts it whole on every
  run. A quicker open draws no line at all, so nothing flashes. `add` shows the line without
  the wordmark, because an import has no live view of its own. The line is gone before the
  header row is written, so it never becomes part of the screen; a pipe and `--quiet` see the
  wordmark, the line and the mark not at all —
  `tests/cli_read.rs::on_a_terminal_a_slow_open_is_named_and_erased`,
  `tests/cli_read.rs::on_a_terminal_a_quick_open_draws_no_waiting_line`,
  `tests/cli_read.rs::on_a_terminal_the_wordmark_is_drawn_once`,
  `src/ui.rs::tests::the_waiting_line_names_the_work_and_keeps_moving`
- AC-25: a book whose source file changed since it was added is recoverable in place instead
  of only by `rm` and `add`, which costs the position: `refresh <prefix>` rewrites the
  record from the file on disk — chapters, sentence counts, character totals and the
  content hash — keeps the book's `id`, and keeps the stored position with the chapter and
  sentence indices clamped to the new index rather than read as the old one; the next `read`
  opens it instead of exiting 4. Refreshing a file that did not change writes nothing and
  exits 0, and an unknown id exits 1 as it does everywhere else —
  `tests/cli_refresh.rs::refreshing_a_rewritten_book_keeps_the_position_and_reads_it`,
  `tests/cli_refresh.rs::refreshing_an_unchanged_book_changes_nothing`,
  `tests/cli_refresh.rs::refresh_on_an_unknown_id_exits_1`
- AC-26: footnote apparatus never reaches the speaker. The body of an `aside` — where EPUB 3
  keeps a footnote — and the body of a noteref anchor (`a epub:type="noteref"`, which holds
  the raised marker) are dropped the way `script` and `style` already are, so the sentence
  that referenced the note is spoken whole instead of being cut in half by the note, and no
  marker digit is spoken after the word it marked. A `sup` outside a noteref anchor is real
  text, not apparatus, and stays —
  `tests/split.rs::strip_html_drops_a_footnote_and_its_marker`,
  `tests/split.rs::strip_html_keeps_a_superscript_that_is_not_a_note`
- AC-27: a table is announced instead of enumerated. The cells of a `table` are not spoken,
  because `td` and `th` are block breaks and a table of running times arrives as a column of
  bare numbers with no column names; in their place one sentence says what the table is and
  how big it is — its `caption` when it has one, otherwise `Table`, then the row and column
  counts — and an empty table says nothing at all rather than announcing a shape it does not
  have —
  `tests/split.rs::strip_html_announces_a_table_instead_of_reading_its_cells`,
  `tests/split.rs::strip_html_names_a_table_by_its_caption`,
  `tests/split.rs::strip_html_says_nothing_for_an_empty_table`
- AC-28: an EPUB code listing is announced, not read. The body of a `pre` is dropped and
  replaced by one sentence giving its line count, because a listing spoken aloud is a run of
  identifiers that also swallows the paragraph that introduced it; inline `code` inside a
  sentence is untouched and still spoken, since a sentence cannot lose its verb. An empty
  `pre` says nothing. PDF carries no markup to recognise a listing by, so a listing in a PDF
  is still spoken —
  `tests/split.rs::strip_html_announces_a_code_sample_instead_of_reading_it`,
  `tests/split.rs::strip_html_reads_inline_code_in_its_sentence`,
  `tests/split.rs::strip_html_says_nothing_for_an_empty_code_sample`
- AC-29: `--for <duration>` ends a run when its window closes instead of at the end of the
  chapter. The flag takes a plain duration — `90s`, `30m`, `1h30m`, or a bare count of
  seconds — and the window starts when playback does, not while the book is being hashed and
  parsed, because a PDF's seconds of `pdftotext` are not listening. A sentence is never cut:
  the check happens between sentences, so the one being spoken is finished, the saved
  position is the one after it, and a window that has already closed plays nothing at all.
  The budget covers one run, and a run is one chapter, so a timer longer than the chapter
  stops where the chapter does and the `next: … --chapter N` line carries on. A duration
  that does not parse is bad input — exit 2, nothing played. A window this machine's clock
  cannot hold is not a panic and not a false promise — that instant can never arrive, so the
  run is unbounded and the chapter is read as it would be without `--for` —
  `tests/cli_read.rs::a_timer_that_has_already_expired_plays_nothing`,
  `tests/cli_read.rs::a_timer_that_cannot_be_parsed_exits_2_without_playing`,
  `tests/cli_read.rs::an_unrepresentable_window_is_not_a_panic`,
  `tests/cli_read.rs::a_window_longer_than_the_chapter_stops_at_the_chapter_end`,
  `src/estimate.rs::tests::a_duration_is_read_in_the_units_a_listener_says`,
  `src/play.rs::tests::the_run_stops_at_a_sentence_boundary_once_the_time_is_up`

Proven at the store layer only: exit 5 for a second concurrent reader
(`src/store.rs::a_lock_held_by_this_process_is_respected`,
`src/store.rs::a_stale_lock_from_a_dead_process_is_recovered`). No integration test spawns
two binaries and asserts the code.

## 4. Data

New files only, all under XDG paths; nothing outside them is touched.

- `$XDG_DATA_HOME/libretto/library.json` — `version`, `books[]` (`id`, `title`, `author`,
  `source_path`, `content_hash`, `chapter_count`, `sentence_counts`, `chapter_chars`,
  `added_at`),
  `progress{}` per book
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
- a footnote body (`aside`) and the raised marker of a noteref are not spoken: the note never
  interrupts the sentence that referenced it, and a superscript that is not a note is kept
- a table is announced by its `caption` and its shape (`Table, two rows, three columns`) and
  its cells are not spoken; a `caption` outside a table and a `figcaption` are still ordinary
  text
- a code listing (`pre`) is announced by its line count and not read; inline `code` stays part
  of its sentence. A PDF's text has no tags, so a listing inside a PDF is still spoken: that
  gap is known and not handled here
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
- `--for 30m` stops between sentences, never inside one, and its window begins at the first
  sentence rather than at the open, so a book that took ten seconds to parse still gets
  thirty minutes of listening; the budget is one run, and a run is one chapter
- a second reader on the same library exits 5; a lock left by a dead pid is recovered
- a corrupt library is reported with its path and never overwritten (exit 6)
- a changed source file is refused with exit 4 until it is refreshed; `refresh` never
  deletes the entry, never changes the `id` the position is keyed on, and clamps a stored
  position that now falls past the end rather than treating it as an error

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
