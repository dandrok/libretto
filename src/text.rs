//! Turning book markup into speakable sentences.
//!
//! Two jobs, deliberately separate: `strip_html` removes everything that is not
//! prose, `sentences` cuts prose into units a TTS engine can speak. Neither
//! touches the filesystem or the network, so both are tested directly.

/// Sentence terminators. `…` is here because EPUBs from retail pipelines use it
/// instead of three dots more often than you would expect.
const TERMINATORS: [char; 4] = ['.', '!', '?', '…'];

/// Words that end in a dot but do not end a sentence. Lowercase, with the dot.
/// Deliberately short: an abbreviation missing here costs one odd pause, while a
/// false positive costs a wrong split in the middle of a sentence.
const ABBREVIATIONS: [&str; 22] = [
    "mr.", "mrs.", "ms.", "dr.", "prof.", "sr.", "jr.", "st.", "vs.", "etc.", "e.g.", "i.e.",
    "fig.", "no.", "nos.", "pp.", "p.", "approx.", "inc.", "ltd.", "co.", "cf.",
];

/// Tags whose boundaries are paragraph breaks for speech purposes.
const BLOCK_TAGS: [&str; 20] = [
    "p",
    "div",
    "br",
    "li",
    "tr",
    "td",
    "th",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "section",
    "article",
    "header",
    "footer",
    "pre",
    "hr",
];

/// Strip markup, script and style content, and collapse whitespace into one
/// paragraph per line.
pub fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut chars = html.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '<' {
            out.push(c);
            continue;
        }
        let tag = read_tag(&mut chars);
        let name = tag_name(&tag);
        if name == "script" || name == "style" {
            skip_element_body(&mut chars, &name);
            continue;
        }
        if BLOCK_TAGS.contains(&name.as_str()) {
            out.push('\n');
        }
    }

    normalise_whitespace(&decode_entities(&out))
}

/// Consume a tag and return its text without the angle brackets.
fn read_tag(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut tag = String::new();
    for c in chars.by_ref() {
        if c == '>' {
            break;
        }
        tag.push(c);
    }
    tag
}

/// `p`, `/p` and `p class="x"` all yield `p`; anything else yields the bare name.
fn tag_name(tag: &str) -> String {
    tag.trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Skip everything up to and including the closing tag of a script or style
/// element, so CSS and JavaScript never reach the speaker.
fn skip_element_body(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, name: &str) {
    let closing: Vec<char> = format!("</{name}").chars().collect();
    let mut recent: Vec<char> = Vec::with_capacity(closing.len());
    for c in chars.by_ref() {
        recent.push(c.to_ascii_lowercase());
        if recent.len() > closing.len() {
            recent.remove(0);
        }
        if recent == closing {
            for c in chars.by_ref() {
                if c == '>' {
                    break;
                }
            }
            return;
        }
    }
}

fn decode_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let Some(end) = tail.find(';').filter(|end| *end <= 12) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        match resolve_entity(entity) {
            Some(text) => {
                out.push_str(&text);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn resolve_entity(entity: &str) -> Option<String> {
    let named = match entity {
        "amp" => Some("&"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "quot" => Some("\""),
        "apos" => Some("'"),
        "nbsp" => Some(" "),
        "mdash" => Some("—"),
        "ndash" => Some("–"),
        "hellip" => Some("…"),
        "lsquo" => Some("‘"),
        "rsquo" => Some("’"),
        "ldquo" => Some("“"),
        "rdquo" => Some("”"),
        _ => None,
    };
    if let Some(text) = named {
        return Some(text.to_string());
    }
    let code = if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        u32::from_str_radix(hex, 16).ok()
    } else {
        entity
            .strip_prefix('#')
            .and_then(|dec| dec.parse::<u32>().ok())
    };
    code.and_then(char::from_u32).map(|c| c.to_string())
}

fn normalise_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&collapsed);
    }
    out
}

/// Is this line just the book title (or author) with no prose of its own?
///
/// Retail EPUBs repeat a running head on every page, and O'Reilly puts it at the
/// top of each chapter file — spoken aloud it is the same sentence dozens of
/// times. A line counts when it equals the title or starts with it and adds
/// nothing substantial ("…, 2nd Edition"), so prose that merely mentions the
/// title survives.
pub fn is_running_head(line: &str, title: &str, author: Option<&str>) -> bool {
    let candidate = normalise_for_match(line);
    if candidate.is_empty() {
        return false;
    }
    let candidate_core = core_of(&candidate);

    let mut noise: Vec<String> = Vec::new();
    for raw in std::iter::once(title).chain(author) {
        let normalised = normalise_for_match(raw);
        if normalised.is_empty() {
            continue;
        }
        let core = core_of(&normalised);
        if core != normalised {
            noise.push(core);
        }
        noise.push(normalised);
    }

    noise
        .iter()
        .any(|entry| matches_loosely(&candidate, entry) || matches_loosely(&candidate_core, entry))
}

/// The part of a title before any parenthetical, edition or subtitle marker.
/// `Designing Data-Intensive Applications (for someone)` and
/// `Designing Data-Intensive Applications, 2nd Edition` share a core.
fn core_of(normalised: &str) -> String {
    normalised
        .split(['(', ',', ';'])
        .next()
        .unwrap_or(normalised)
        .trim()
        .to_string()
}

fn matches_loosely(candidate: &str, entry: &str) -> bool {
    let (shorter, longer) = if candidate.chars().count() <= entry.chars().count() {
        (candidate, entry)
    } else {
        (entry, candidate)
    };
    shorter == longer
        || (longer.starts_with(shorter) && longer.chars().count() <= shorter.chars().count() + 20)
}

/// Remove running heads from a chapter's text.
pub fn drop_running_heads(text: &str, title: &str, author: Option<&str>) -> String {
    text.lines()
        .filter(|line| !is_running_head(line, title, author))
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalise_for_match(line: &str) -> String {
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_lowercase()
}

/// Cut prose into speakable sentences, each at most `max_len` characters.
///
/// `max_len` exists because every TTS endpoint has a request limit and a long
/// request is a long silence before the first word.
pub fn sentences(text: &str, max_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        split_line(line.trim(), max_len, &mut out);
    }
    // A fragment of pure punctuation — a bullet, an em dash, a row of dots from a
    // table of contents — has nothing to say. Left in, it becomes a request the engine
    // answers with an empty body, which used to stop a book mid-chapter.
    out.retain(|sentence| speakable(sentence));
    out
}

/// Is there anything here a voice could pronounce?
pub fn speakable(text: &str) -> bool {
    text.chars().any(|character| character.is_alphanumeric())
}

fn split_line(line: &str, max_len: usize, out: &mut Vec<String>) {
    if line.is_empty() {
        return;
    }
    let chars: Vec<char> = line.chars().collect();
    let mut start = 0;
    for (i, c) in chars.iter().enumerate() {
        if !TERMINATORS.contains(c) || !ends_sentence(&chars, i) {
            continue;
        }
        let end = sentence_end(&chars, i);
        push_limited(
            chars[start..=end].iter().collect::<String>().trim(),
            max_len,
            out,
        );
        start = end + 1;
    }
    if start < chars.len() {
        push_limited(
            chars[start..].iter().collect::<String>().trim(),
            max_len,
            out,
        );
    }
}

/// A terminator ends a sentence when whitespace (or the end) follows it and the
/// word it closes is not an abbreviation or an initial.
fn ends_sentence(chars: &[char], index: usize) -> bool {
    let after = sentence_end(chars, index) + 1;
    if after < chars.len() && !chars[after].is_whitespace() {
        return false;
    }
    !is_abbreviation(&word_before(chars, index))
}

/// Closing punctuation belongs to the sentence it closes: `"Stop."` is one unit.
fn sentence_end(chars: &[char], terminator: usize) -> usize {
    let mut end = terminator;
    while end + 1 < chars.len() && matches!(chars[end + 1], '"' | '\'' | '”' | '’' | ')' | ']')
    {
        end += 1;
    }
    end
}

/// The word a terminator closes, terminator included, so `Dr.` and `e.g.` are
/// matched as they are written.
fn word_before(chars: &[char], terminator: usize) -> String {
    let mut start = terminator;
    while start > 0 {
        let c = chars[start - 1];
        if c.is_alphanumeric() || c == '.' {
            start -= 1;
        } else {
            break;
        }
    }
    chars[start..=terminator].iter().collect()
}

fn is_abbreviation(word: &str) -> bool {
    let lower = word.to_lowercase();
    if ABBREVIATIONS.contains(&lower.as_str()) {
        return true;
    }
    // A single capital letter followed by a dot is an initial: "J. R. Tolkien".
    let mut chars = word.chars();
    matches!((chars.next(), chars.next(), chars.next()), (Some(c), Some('.'), None) if c.is_uppercase())
}

/// Push a sentence, hard-splitting only when one has exceeded `max_len`.
fn push_limited(sentence: &str, max_len: usize, out: &mut Vec<String>) {
    let sentence = sentence.trim();
    if sentence.is_empty() {
        return;
    }
    if sentence.chars().count() <= max_len {
        out.push(sentence.to_string());
        return;
    }
    let mut current = String::new();
    for word in sentence.split_whitespace() {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > max_len {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        out.push(current);
    }
}
