# Backlog — what is missing, what could be built

Not a contract and not a plan. `docs/spec.md` is the contract, and the plan for a slice
lives in its PR (AGENTS.md), because a plan that finishes drifts out of date. This file
only records what we noticed and have not decided, with the evidence in the tree, so the
observation is not re-derived twice. An entry here commits us to nothing. When one is
picked up it gets a spec commit, then a PR, then this line goes away.

Status: written against v1.12.4. A finished entry is **deleted**, not ticked off —
`CHANGELOG.md` is the record of what shipped, and this file only holds what is still open.

Gate: `spec` means `docs/spec.md` must change in its own commit before any code. `none`
means it fits the contract as written.

## Cheap, because most of the work is already done

### B-1 Chapter titles are parsed and thrown away — gate: spec

`src/epub.rs:54` carries `Chapter { title: Option<String>, text }` and `add` never stores
it: `BookRecord` (`src/store.rs:58`) has counts and character totals but no names. One
`#[serde(default)]` field plus a printer buys `libretto toc`, `ls` naming the chapter you
are in, and the live line reading `ch 4/12 · The Long Way` instead of `ch 4/12`.
Touches: `store.rs` (field), `main.rs` (add + ls row), `ui.rs` (live line), a new
subcommand in `cli.rs`. No new dependency. Charset and PDF running heads already produce
a title where the EPUB has none.

### B-2 Per-book voice and speed — gate: spec

`src/config.rs` resolves one global setting. Daily readers want 1.25x for non-fiction and
a different voice for fiction. The existing order (flags > file > default) gains one layer:
flags > book > file > default. Touches: a per-book override field, `config.rs` resolution,
`--voice`/`--speed` already exist as flags. Migration is `serde(default)` on the same record.

### B-3 `--json` on `ls` and `doctor` — gate: spec

Scripts are a declared audience (AC-16 keeps pipes plain and quiet), but progress cannot be
parsed reliably out of `~3h left`. `serde_json` and the structs are already present.
Unlocks status bars, editor plugins, shell prompts.

### B-4 Cache has no lifecycle — gate: spec

`src/cache.rs` exposes `get`, `put`, `timings` and nothing else. It grows until the disk
complains; `doctor` reports the size and offers no remedy. Spec already promises deleting
the cache is always safe, so `libretto cache ls | prune [--older-than Nd] | verify` is
low-risk. Touches: `cache.rs` walk, `cli.rs`, exit codes unchanged.

## Real holes a daily user hits

### B-6 No absolute navigation — gate: spec

`--chapter N`, `--skip N`, `--rewind N` only. There is no `goto <chapter>:<sentence>` and no
`find "phrase"`. `find` is cheap on EPUB and slow on PDF, because `pdftotext` converts the
whole file on every run (already noted in AC-24) — decide whether `find` accepts that or
waits for B-7.

One lever is nearly free and missing anyway: `--from 4:12:30`. `src/estimate.rs` already
turns characters into seconds at 16.1/s and back, so a timestamp is that lookup reversed,
and it is the number a listener actually has in hand (from a commute, from `ls`). Also
missing: a chapter range (`--chapters 3-5`) — one loop over an index `--chapter` already
computes.

### B-7 PDF re-conversion on every open — gate: spec

AC-24 admits `pdftotext` runs whole-file on every run, seconds per open. Caching the
extracted text beside the audio would fix `find`, `refresh` and both open paths at once,
at the cost of a second on-disk artifact that must be invalidated by content hash.

### B-8 Progress is unrecoverable, not just safe — gate: spec

`save` is temp + rename, but there is no `.bak` and no export. A corrupt `library.json`
exits 6 and refuses to write — the right call, and the listening history is gone anyway.
A rotating `.bak` makes "never overwritten" stronger, not weaker. `libretto export` to a
file you can rsync is a backup, not the cross-device sync the spec refuses.

### B-9 `add` takes one path — gate: spec

`libretto add ~/Books/*.epub` is N process starts, N lock acquisitions, N full hashes.
A directory or repeated positional would be one lock and one save.

### B-10 Pronunciations — gate: spec

Names and foreign words are read wrong and there is no lever. A `[pronounce]` table is
small, but note the trap: the cache key is the request fingerprint (`src/cache.rs:59`), so
the substitution set must be in the fingerprint or stale audio replays silently.

### B-11 Smaller ones

- Bookmarks, notes, and an explicit `done` — `done` today only happens by reading to the end.
- `--interactive` (space / n / q): promised in `decisions.md` §3 as optional v0.6, never
  shipped. Build it or delete the paragraph; a promise in a decisions doc reads like a plan.
- Shell completions and a man page. A tool that lives in a shell has neither. `clap_complete`
  or `clap_mangen` is a new dependency, so the PR owes the AGENTS.md line saying why the
  standard library cannot do it; the man page alone could be a committed file.
- `follow` keeps its pause mark only while the player can report a position (`src/play.rs`).
  `--player ffplay` silently loses that. Saying so on stderr the first time, or probing the
  socket before promising a pause, is small and honest.

## Argued for, currently out of scope

### B-12 `.m4b` / mp3 export — gate: spec (deliberately, §2)

Out of scope today, and the single thing a TTS reader gets asked for (car, phone, gym). The
audio already sits in the cache and mp3 frames concatenate without a decoder, so the hard
part is paid for. The only item here I would actively argue to pull in.

## Proof gaps

- No test for a malformed OPF, an empty spine, or a `library.json` whose `version` is from
  the future.
- `version` exists but no migration table is documented, while AC-18 already special-cases
  one older format by field absence. That rot compounds with every new field.

## Refused, and staying refused

Cross-device sync, accounts, telemetry, a full-screen TUI, multi-voice dialogue, decoding
audio in-process, any engine that is not OpenAI-compatible HTTP. `decisions.md` §3 and
`spec.md` §2 give the reasons; nothing above changes them.
