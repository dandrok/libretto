//! AC-20: `follow` is a presentation of `read`, and a pipe cannot tell them apart.

mod common;

use common::Sandbox;
use common::fake_tts::{FakeTts, Reply};

const AUDIO: &[u8] = b"ID3\x04\x00\x00fake-mp3";

#[test]
fn follow_on_a_pipe_is_read_byte_for_byte() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "followed.epub",
        Some("Followed Book"),
        &[
            ("One", "First sentence. Second sentence."),
            ("Two", "Third."),
        ],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let shared = [
        "followed",
        "--player",
        "true",
        "--endpoint",
        endpoint.as_str(),
        "--sentences",
        "1",
        "--chapter",
        "1",
    ];
    let mut read_args = vec!["read"];
    read_args.extend_from_slice(&shared);
    let mut follow_args = vec!["follow"];
    follow_args.extend_from_slice(&shared);

    let read = sandbox.stdout_of(&read_args);
    // Same starting point, and no warm cache to report a hit the other run did not have:
    // the comparison is about presentation, not about the cache.
    std::fs::remove_dir_all(sandbox.root().join("cache")).expect("clear cache");
    let follow = sandbox.stdout_of(&follow_args);
    assert_eq!(
        read, follow,
        "no terminal means no block, no escapes and no different words"
    );
    assert!(
        !follow.contains('\x1b'),
        "a pipe must never receive escape sequences: {follow:?}"
    );
}

#[test]
fn follow_quiet_is_plain_and_says_where_it_stopped() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "quiet.epub",
        Some("Quiet Book"),
        &[("One", "Only sentence.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let output = sandbox.run(&[
        "follow",
        "quiet",
        "--player",
        "true",
        "--endpoint",
        &endpoint,
        "--quiet",
    ]);
    assert!(output.status.success(), "quiet follow still reads");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("stopped at chapter 1"), "{stdout}");
    assert!(
        !stdout.contains('\x1b'),
        "no escapes with --quiet: {stdout:?}"
    );
}

#[test]
fn follow_accepts_the_same_arguments_as_read() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "args.epub",
        Some("Arguable Book"),
        &[("One", "A. B. C. D."), ("Two", "E.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let stdout = sandbox.stdout_of(&[
        "follow",
        "arguable",
        "--player",
        "true",
        "--endpoint",
        &endpoint,
        // No --chapter: that flag means "start this chapter", which resets the position
        // and would swallow --skip.
        "--skip",
        "1",
        "--sentences",
        "1",
        "--speed",
        "1.25",
        "--width",
        "50",
    ]);
    assert!(stdout.contains("starting at sentence 2/"), "{stdout}");
}

/// The one thing a pipe cannot show: that a terminal gets the block at all, and that a
/// block taller than one row is replaced rather than printed again underneath. `script`
/// gives the child a pty, which is the only way `stderr` counts as a terminal; it is
/// util-linux's, so this proof lives on Linux rather than pretending about macOS.
#[cfg(target_os = "linux")]
fn under_pty(sandbox: &Sandbox, args: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    if !Path::new("/usr/bin/script").exists() && !Path::new("/bin/script").exists() {
        panic!(
            "no `script` binary: this proof cannot run, and skipping it silently would hide that"
        );
    }

    let root = sandbox.root().display();
    let player = sandbox.root().join("slow-player");
    if !player.exists() {
        std::fs::write(&player, "#!/bin/sh\nsleep 1\n").expect("player script");
        std::fs::set_permissions(&player, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let inner = format!(
        "env XDG_DATA_HOME='{root}/data' XDG_CACHE_HOME='{root}/cache' \
             XDG_CONFIG_HOME='{root}/config' HOME='{root}' COLUMNS=60 \
             '{}' {args} --player '{}' --width 60",
        env!("CARGO_BIN_EXE_libretto"),
        player.display(),
    );
    let output = Command::new("script")
        .args(["-qec", &inner, "/dev/null"])
        .output()
        .expect("run under a pty");
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[cfg(target_os = "linux")]
#[test]
fn on_a_terminal_follow_draws_the_sentence_and_lights_a_word() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book(
        "screen.epub",
        Some("Screen Book"),
        &[("One", "Alpha beta gamma delta epsilon zeta eta theta.")],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    let raw = under_pty(
        &sandbox,
        &format!("follow screen --endpoint '{endpoint}' --sentences 1"),
    );

    assert!(
        raw.contains("Alpha beta gamma"),
        "the sentence reaches the terminal: {raw:?}"
    );
    assert!(raw.contains("\x1b[4m"), "a word is underlined: {raw:?}");
}

#[test]
fn a_negative_sync_offset_is_a_value_not_a_flag() {
    let sandbox = Sandbox::new();
    let fake = FakeTts::start(Reply::Audio(AUDIO.to_vec()));
    let book = sandbox.book("sync.epub", Some("Sync Book"), &[("One", "One. Two.")]);
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let endpoint = fake.endpoint();
    for offset in ["-80", "80", "0"] {
        let output = sandbox.run(&[
            "follow",
            "sync",
            "--player",
            "true",
            "--endpoint",
            &endpoint,
            "--chapter",
            "1",
            "--sentences",
            "1",
            "--sync-offset",
            offset,
        ]);
        assert!(
            output.status.success(),
            "{offset} was refused as a flag: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
