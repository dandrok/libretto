//! Reading an EPUB: a zip with an OPF package document inside.
//!
//! Only what the speaker needs is extracted — title, author, and chapters in
//! spine order. Nothing here writes to disk, and nothing is decoded twice.
//!
//! The file is untrusted input, so every entry is read with a ceiling: a zip
//! header can *declare* an uncompressed size of 40 GB in a 3 KB file, and
//! trusting that number is how a book turns into an out-of-memory abort.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use thiserror::Error;

use crate::text;

/// Largest single entry (chapter, package document, stylesheet) we will inflate.
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
/// Largest amount of prose one book may produce in total.
const MAX_BOOK_PROSE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum EpubError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not a readable EPUB archive: {reason}")]
    Archive { path: String, reason: String },
    #[error("{what} is missing from the archive")]
    Missing { what: &'static str },
    #[error("cannot parse {what}: {reason}")]
    Xml { what: &'static str, reason: String },
    #[error("{entry} unpacks to more than {limit_bytes} bytes; refusing to read it")]
    TooLarge { entry: String, limit_bytes: u64 },
    #[error("this EPUB has encrypted content; only unencrypted books are supported")]
    Encrypted,
    #[error("the EPUB has no readable chapters")]
    NoChapters,
}

/// A book as the speaker sees it: metadata plus ordered chapters of clean prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Book {
    pub title: String,
    pub author: Option<String>,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub title: Option<String>,
    pub text: String,
}

/// Read an EPUB from disk. The file is opened read-only and never modified.
pub fn load(path: &Path) -> Result<Book, EpubError> {
    let shown = path.display().to_string();
    let file = File::open(path).map_err(|source| EpubError::Io {
        path: shown.clone(),
        source,
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|reason| EpubError::Archive {
        path: shown.clone(),
        reason: reason.to_string(),
    })?;

    let container =
        read_entry(&mut archive, "META-INF/container.xml", &shown)?.ok_or(EpubError::Missing {
            what: "META-INF/container.xml",
        })?;
    let opf_path = package_path(&container)?;
    let opf = read_entry(&mut archive, &opf_path, &shown)?.ok_or(EpubError::Missing {
        what: "the package document",
    })?;
    let package = Package::parse(&opf)?;

    if has_encrypted_content(&mut archive, &opf_path, &package, &shown)? {
        return Err(EpubError::Encrypted);
    }

    let title = package
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| fallback_title(path));

    let mut chapters = Vec::with_capacity(package.spine.len());
    let mut prose_bytes: u64 = 0;
    for idref in &package.spine {
        let Some(item) = package.items.iter().find(|item| &item.id == idref) else {
            continue;
        };
        if !item.is_document() {
            continue;
        }
        let href = resolve(&dir_of(&opf_path), &item.href);
        let Some(markup) = read_entry(&mut archive, &href, &shown)? else {
            continue;
        };
        let body = text::drop_running_heads(
            &text::strip_html(&markup),
            &title,
            package.author.as_deref(),
        );
        if body.is_empty() {
            continue; // a chapter that is only an image says nothing
        }
        prose_bytes += body.len() as u64;
        if prose_bytes > MAX_BOOK_PROSE_BYTES {
            return Err(EpubError::TooLarge {
                entry: "the book's prose".to_string(),
                limit_bytes: MAX_BOOK_PROSE_BYTES,
            });
        }
        chapters.push(Chapter {
            title: heading(&markup, &title, package.author.as_deref()),
            text: body,
        });
    }

    if chapters.is_empty() {
        return Err(EpubError::NoChapters);
    }

    Ok(Book {
        title,
        author: package.author,
        chapters,
    })
}

fn fallback_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("untitled")
        .to_string()
}

/// Read one archive entry, refusing anything that inflates past the ceiling.
///
/// The declared size is checked first so an absurd header fails before a
/// single byte is allocated, and the read itself is bounded so a lying header
/// cannot win either.
fn read_entry(
    archive: &mut zip::ZipArchive<File>,
    name: &str,
    shown: &str,
) -> Result<Option<String>, EpubError> {
    match archive.by_name(name) {
        Ok(mut entry) => {
            if entry.size() > MAX_ENTRY_BYTES {
                return Err(EpubError::TooLarge {
                    entry: name.to_string(),
                    limit_bytes: MAX_ENTRY_BYTES,
                });
            }
            let mut bytes = Vec::with_capacity(entry.size().min(MAX_ENTRY_BYTES) as usize);
            entry
                .by_ref()
                .take(MAX_ENTRY_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|source| EpubError::Io {
                    path: shown.to_string(),
                    source,
                })?;
            if bytes.len() as u64 > MAX_ENTRY_BYTES {
                return Err(EpubError::TooLarge {
                    entry: name.to_string(),
                    limit_bytes: MAX_ENTRY_BYTES,
                });
            }
            Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(EpubError::Archive {
            path: shown.to_string(),
            reason: format!("{name}: {e}"),
        }),
    }
}

/// `META-INF/container.xml` names the package document; that path is the root
/// for every other href in the book.
fn package_path(container: &str) -> Result<String, EpubError> {
    let doc = roxmltree::Document::parse(container).map_err(|e| EpubError::Xml {
        what: "META-INF/container.xml",
        reason: e.to_string(),
    })?;
    doc.descendants()
        .find(|node| node.has_tag_name("rootfile"))
        .and_then(|node| node.attribute("full-path"))
        .map(|path| path.to_string())
        .ok_or(EpubError::Xml {
            what: "META-INF/container.xml",
            reason: "no rootfile with a full-path".to_string(),
        })
}

#[derive(Debug)]
struct Item {
    id: String,
    href: String,
    media_type: String,
}

impl Item {
    fn is_document(&self) -> bool {
        self.media_type.contains("html")
            || self.href.ends_with(".xhtml")
            || self.href.ends_with(".html")
            || self.href.ends_with(".htm")
    }
}

#[derive(Debug)]
struct Package {
    title: Option<String>,
    author: Option<String>,
    items: Vec<Item>,
    spine: Vec<String>,
}

impl Package {
    fn parse(opf: &str) -> Result<Self, EpubError> {
        let doc = roxmltree::Document::parse(opf).map_err(|e| EpubError::Xml {
            what: "the package document",
            reason: e.to_string(),
        })?;

        let first_text = |name: &str| {
            doc.descendants()
                .find(|node| node.tag_name().name() == name)
                .and_then(|node| node.text())
                .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        };

        let items = doc
            .descendants()
            .filter(|node| node.has_tag_name("item"))
            .filter_map(|node| {
                Some(Item {
                    id: node.attribute("id")?.to_string(),
                    href: node.attribute("href")?.to_string(),
                    media_type: node.attribute("media-type").unwrap_or_default().to_string(),
                })
            })
            .collect();

        let spine = doc
            .descendants()
            .filter(|node| node.has_tag_name("itemref"))
            .filter_map(|node| node.attribute("idref").map(str::to_string))
            .collect();

        Ok(Self {
            title: first_text("title"),
            author: first_text("creator"),
            items,
            spine,
        })
    }
}

/// Font obfuscation is normal in retail EPUBs; encrypted *documents* are DRM.
fn has_encrypted_content(
    archive: &mut zip::ZipArchive<File>,
    opf_path: &str,
    package: &Package,
    shown: &str,
) -> Result<bool, EpubError> {
    let Some(xml) = read_entry(archive, "META-INF/encryption.xml", shown)? else {
        return Ok(false);
    };
    let Ok(doc) = roxmltree::Document::parse(&xml) else {
        return Ok(true); // unreadable encryption list: assume the worst
    };
    let base = dir_of(opf_path);
    for reference in doc
        .descendants()
        .filter(|node| node.has_tag_name("CipherReference"))
    {
        let Some(uri) = reference.attribute("URI") else {
            continue;
        };
        let path = resolve(&base, uri);
        let is_document = package
            .items
            .iter()
            .any(|item| item.is_document() && resolve(&base, &item.href) == path);
        if is_document || path.ends_with(".xhtml") || path.ends_with(".html") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(pos) => path[..pos].to_string(),
        None => String::new(),
    }
}

/// Join an href onto the package directory, dropping anchors and resolving
/// `.`/`..`, because spine hrefs are relative to the OPF and occasionally messy.
fn resolve(base_dir: &str, href: &str) -> String {
    let href = percent_decode(href.split('#').next().unwrap_or_default());
    let joined = match (base_dir.is_empty(), href.starts_with('/')) {
        (true, _) => href.trim_start_matches('/').to_string(),
        (false, true) => href.trim_start_matches('/').to_string(),
        (false, false) => format!("{base_dir}/{href}"),
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in joined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or_default();
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A chapter's own heading, used for the progress line. Running heads are
/// skipped: in O'Reilly books every chapter file starts with the book title.
fn heading(markup: &str, title: &str, author: Option<&str>) -> Option<String> {
    let lower = markup.to_ascii_lowercase();
    for tag in ["h1", "h2", "h3"] {
        let open = format!("<{tag}");
        let mut from = 0;
        while let Some(offset) = lower[from..].find(&open) {
            let start = from + offset;
            let Some(body_start) = lower[start..].find('>').map(|o| start + o + 1) else {
                break;
            };
            let close = format!("</{tag}>");
            let Some(end) = lower[body_start..].find(&close).map(|o| body_start + o) else {
                break;
            };
            let inner = text::strip_html(&markup[body_start..end]);
            if !inner.is_empty() && !text::is_running_head(&inner, title, author) {
                return Some(inner);
            }
            from = end + close.len();
        }
    }
    None
}
