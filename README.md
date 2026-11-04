# libretto

[![crates.io version](https://img.shields.io/crates/v/libretto.svg)](https://crates.io/crates/libretto)

Listen to EPUB books from the terminal, in a good voice, starting where you stopped.

It is for the books you keep meaning to read — the 25-hour technical ones — while you
cook, walk or fold laundry. No subscription, no cloud, no account: the voice runs on
your own machine.

```
$ libretto add "Grokking Algorithms.epub"
added grokking-algorithms-second-e-ebcf — Grokking Algorithms, Second Edition (25 chapters)

$ libretto read grokking --chapter 9 --sentences 3
Grokking Algorithms, Second Edition — chapter 9/25 · starting at sentence 1/287
  Grokking Algorithms, Second … · ch 9/25 · 3/287 · af_heart ×1.00
stopped at chapter 9, sentence 3/287 (3 played, 0 from cache)
```

Run it again and it starts at sentence 4 of 287. Type `libretto` with no arguments and
it resumes the last book.

## What you need

- **A TTS endpoint.** Kokoro is the default — free, local, 82M parameters:

  ```bash
  docker run -d --name kokoro --restart unless-stopped \
    -p 127.0.0.1:8880:8880 ghcr.io/remsky/kokoro-fastapi-cpu:latest
  ```

  Anything that speaks the OpenAI `/audio/speech` shape works too (Edge, Gemini, a
  shim, another machine on your LAN).
- **A player.** `mpv` by default; `--player` takes any program that plays a file and
  exits (it receives `--no-video --really-quiet <file>`, so it expects mpv's flags).
- **A book.** A DRM-free EPUB, or a PDF that has a text layer (poppler's
  `pdftotext` reads it; `pdfinfo` supplies the title when the file carries one).
  MOBI and AZW are not supported.

## Install

One Rust binary, built once — crates.io carries the source, `cargo` compiles it while you
install. There are no prebuilt downloads.

**Rust 1.99 or newer.** `rustup` is the shortest path:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Then install it:**

```bash
cargo install --locked libretto
```

`cargo install` puts it in `~/.cargo/bin`, which `rustup` already keeps on your
`PATH`. `--locked` builds against the dependency versions the crate was tested with.

To track `main` instead of a release, build the checkout:

```bash
git clone https://github.com/dandrok/libretto
cd libretto
cargo install --path . --locked
```

To install somewhere else instead — `~/.local/bin`, a container image — copy the
release build:

```bash
cargo build --release
install -m755 target/release/libretto ~/.local/bin/
```

**Check the build before you rely on it.** `doctor` names whatever is missing —
the player, the endpoint, the cache:

```bash
libretto --version    # libretto 1.1.1
libretto doctor
```

Upgrading is one command either way: `cargo install --locked libretto --force` from
crates.io, or `git pull` then `cargo install --path . --locked --force` from a checkout.

## Use

```bash
libretto add path/to/book.epub      # or book.pdf
libretto ls                         # what is in the library, with progress
libretto read <id>                  # speak it from where you stopped
libretto                             # same, for the last book you read
libretto doctor                      # why is it silent?
```

`<id>` is the readable slug from `ls`, and any unique prefix works: `libretto read grok`.

Flags that matter while listening:

| Flag | Why |
|---|---|
| `--chapter N` | jump to a chapter (1-based), starting at its first sentence |
| `--skip N` / `--rewind N` | move forward, or replay a little, without losing your place |
| `--sentences N` | play N sentences and stop — good for trying a voice |
| `--voice`, `--speed` | `af_heart` (default), `af_bella`, `bf_emma`; speed 0.25–4.0 |
| `--endpoint`, `--player` | point at another TTS server or player |

## How it works

1. The EPUB is unpacked and its chapters come out in spine order. Running heads (the
   book title printed at the top of every chapter) are dropped, because read aloud they
   are the same sentence hundreds of times.
2. Each chapter is cut into sentences of at most 400 characters, respecting `Dr.`,
   `e.g.` and closing quotes.
3. A sentence is **synthesized once and cached by content**: the key is a hash of text,
   voice, speed and model in `~/.cache/libretto/audio`. Replaying a chapter costs no
   requests and works offline.
4. `mpv` plays one file at a time while the next sentence is already being synthesized,
   so there is no gap between them.
5. Your position is saved after **every sentence** — a kill loses at most one sentence.

## Config (optional)

`~/.config/libretto/config.toml`, every key optional; flags win over the file:

```toml
voice = "af_heart"
speed = 1.25
endpoint = "http://127.0.0.1:8880/v1"
player = "mpv"
prefetch = 5
```

## Exit codes

| Code | Meaning |
|---|---|
| 0 | ok |
| 1 | unknown book |
| 2 | ambiguous id (lists the candidates) |
| 3 | TTS endpoint unreachable |
| 4 | bad book (DRM, corrupt, or the file changed since it was added) |
| 5 | another reader holds the lock |
| 6 | state problem (library, config, player) — `libretto doctor` explains |

## PDFs

`libretto add book.pdf` works when the PDF has a text layer, and it is honest about what
it can follow:

- **Works:** single-column text PDFs. Chapters come from the running head at the top of
  each page, which changes exactly at a chapter boundary; page numbers, figure captions
  and the table of contents are dropped rather than read aloud; words broken across
  lines are rejoined and ligatures repaired (`workﬂow` → `workflow`).
- **Refused, with a reason:** scans with no text layer, and two-column or complex
  layouts, which the extraction interleaves into word salad.
- **Expect some noise:** a stray "Part I" or "CONTENTS" group. `cargo run --example
  chapters -- book.pdf` prints what the import produced before you listen to it for
  eight hours.

## What it does not do

MOBI/AZW, `.m4b` export, cross-device sync, a full-screen UI. The cache is disposable:
delete `~/.cache/libretto` any time.

## License

MIT OR Apache-2.0.
