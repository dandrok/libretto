//! AC-3 and AC-12: an empty library is not an error, and an unknown id is.

mod common;

use common::Sandbox;

#[test]
fn ls_on_an_empty_library_exits_0_and_says_no_books() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["ls"]);

    assert!(output.status.success(), "an empty library is not a failure");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "no books");
    assert!(
        String::from_utf8_lossy(&output.stderr).is_empty(),
        "nothing to warn about"
    );
}

#[test]
fn ls_lists_title_id_and_progress() {
    let sandbox = Sandbox::new();
    let book = sandbox.book(
        "algorithms.epub",
        Some("Grokking Algorithms"),
        &[
            ("One", "Text."),
            ("Two", "More text."),
            ("Three", "Even more."),
        ],
    );
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let stdout = sandbox.stdout_of(&["ls"]);
    assert_eq!(stdout.lines().count(), 1, "one book, one line: {stdout}");
    assert!(stdout.contains("grokking-algorithms-"));
    assert!(stdout.contains("0%"), "a fresh book is at zero: {stdout}");
    assert!(
        stdout.contains("ch 1/3"),
        "chapter position is shown: {stdout}"
    );
    assert!(stdout.contains("Grokking Algorithms"));
}

#[test]
fn rm_unknown_id_exits_1_and_says_unknown_book() {
    let sandbox = Sandbox::new();
    let book = sandbox.book("book.epub", Some("Some Book"), &[("One", "Text.")]);
    sandbox.stdout_of(&["add", book.to_str().expect("path")]);

    let output = sandbox.run(&["rm", "does-not-exist"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown book"));

    let stdout = sandbox.stdout_of(&["ls"]);
    assert_eq!(stdout.lines().count(), 1, "the library is unchanged");
}

#[test]
fn rm_removes_the_entry_but_never_the_file() {
    let sandbox = Sandbox::new();
    let book = sandbox.book("book.epub", Some("Some Book"), &[("One", "Text.")]);
    let path = book.to_str().expect("path").to_string();
    sandbox.stdout_of(&["add", &path]);

    let stdout = sandbox.stdout_of(&["rm", "some-book"]);
    assert!(stdout.contains("removed"), "got: {stdout}");
    assert!(book.exists(), "the EPUB itself is not ours to delete");

    let after = sandbox.stdout_of(&["ls"]);
    assert_eq!(after.trim(), "no books");
}

#[test]
fn rm_accepts_a_unique_prefix() {
    let sandbox = Sandbox::new();
    let first = sandbox.book("one.epub", Some("Alpha Book"), &[("One", "Text.")]);
    let second = sandbox.book("two.epub", Some("Beta Book"), &[("One", "Other text.")]);
    sandbox.stdout_of(&["add", first.to_str().expect("path")]);
    sandbox.stdout_of(&["add", second.to_str().expect("path")]);

    let stdout = sandbox.stdout_of(&["rm", "alpha"]);
    assert!(stdout.contains("Alpha Book"), "got: {stdout}");
    let remaining = sandbox.stdout_of(&["ls"]);
    assert!(remaining.contains("Beta Book"));
    assert!(!remaining.contains("Alpha Book"));
}

#[test]
fn bare_invocation_on_an_empty_library_is_not_an_error() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&[]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("no books"));
}
