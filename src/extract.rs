//! Text extraction from uploaded files. Everything is turned into light markdown
//! (headings, lists, tables) so the chunker can follow the document structure.
//!
//! - DOCX, PPTX: read in-process (zip + XML).
//! - PDF: `pdftotext` (poppler-utils); DOC: `catdoc`; PPT: `catppt` — shipped in the image.
//! - Markdown: YAML front matter (`title`, `tags`, `status`) is read and removed.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde::Serialize;

use crate::Invalid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Pdf,
    Docx,
    Pptx,
    Doc,
    Ppt,
    Markdown,
    Html,
    Text,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Pdf => "PDF",
            Format::Docx => "Word",
            Format::Pptx => "PowerPoint",
            Format::Doc => "Word 97-2003",
            Format::Ppt => "PowerPoint 97-2003",
            Format::Markdown => "Markdown",
            Format::Html => "HTML",
            Format::Text => "Texte",
        }
    }
}

/// An image found in a document; `<!-- image N -->` in the text marks where it appears.
#[derive(Clone, Default)]
pub struct ExtractedImage {
    /// Empty for an image of a web page that still has to be downloaded from `source`.
    pub bytes: Vec<u8>,
    /// Its own description, when the document gives one (alt text).
    pub alt: Option<String>,
    /// Address of an image of an HTML page, as written in the page (maybe relative).
    pub source: Option<String>,
}

impl std::fmt::Debug for ExtractedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ExtractedImage({} bytes, {:?})", self.bytes.len(), self.alt)
    }
}

pub const IMAGE_MARKER: &str = "<!-- image ";

/// The file extension of an image browsers display (PNG, JPEG, GIF, WebP), from its bytes.
/// Other formats (EMF, WMF, TIFF…) and SVG (which may carry scripts) are left out.
pub fn web_image_ext(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("jpg"),
        [b'G', b'I', b'F', b'8', ..] => Some("gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("webp"),
        _ => None,
    }
}

fn zip_bytes(archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<Vec<u8>> {
    let mut file = archive.by_name(name).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// An alt text worth keeping: not a file name or a generic "Picture 3".
fn useful_alt(alt: Option<String>) -> Option<String> {
    let alt = alt?.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = alt.to_lowercase();
    // Descriptions Office writes by itself ("Une image contenant texte, capture d'écran…").
    let automatic = ["une image contenant", "image contenant", "a picture containing", "an image containing", "ein bild, das", "généré par l'ia", "généré par l’ia", "ai-generated content"];
    if automatic.iter().any(|a| lower.contains(a)) {
        return None;
    }
    let generic = ["picture", "image", "imagen", "grafik", "graphic", "graphique", "chart", "diagram", "logo"];
    let looks_generic = generic.iter().any(|g| lower.starts_with(g) && lower[g.len()..].trim().chars().all(|c| c.is_ascii_digit()));
    let file_name = lower.contains('.') && !lower.contains(' ');
    (!alt.is_empty() && !looks_generic && !file_name && alt.chars().count() <= 300).then_some(alt)
}

#[derive(Debug, Default)]
pub struct Extracted {
    pub text: String,
    /// Images, in the order of their `<!-- image N -->` markers.
    pub images: Vec<ExtractedImage>,
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub status: Option<String>,
    /// PDF pages or presentation slides.
    pub pages: Option<usize>,
    /// True when `#` lines are real headings (markdown, HTML, Word, PowerPoint). In PDFs or
    /// plain text they are often code comments, so they must not drive titles or chunking.
    pub structured: bool,
}

pub const SUPPORTED: &str = ".pdf, .docx, .pptx, .doc, .ppt, .md, .html, .txt";

/// Picks the format from the file signature first, then the extension.
pub fn detect(filename: &str, bytes: &[u8]) -> Result<Format> {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    if bytes.starts_with(b"%PDF") {
        return Ok(Format::Pdf);
    }
    if bytes.starts_with(b"PK\x03\x04") {
        let names: Vec<String> = zip::ZipArchive::new(Cursor::new(bytes))
            .map(|z| z.file_names().map(str::to_string).collect())
            .unwrap_or_default();
        if names.iter().any(|n| n == "word/document.xml") {
            return Ok(Format::Docx);
        }
        if names.iter().any(|n| n == "ppt/presentation.xml") {
            return Ok(Format::Pptx);
        }
    }
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
        return match ext.as_str() {
            "ppt" | "pps" | "pot" => Ok(Format::Ppt),
            _ => Ok(Format::Doc),
        };
    }
    match ext.as_str() {
        "md" | "markdown" | "mdx" => Ok(Format::Markdown),
        "html" | "htm" | "xhtml" => Ok(Format::Html),
        "txt" | "text" | "rst" | "adoc" | "asciidoc" | "org" | "csv" | "log" => Ok(Format::Text),
        _ if std::str::from_utf8(bytes).is_ok() => Ok(Format::Text),
        _ => Err(Invalid(format!("unsupported file format for `{filename}` (supported: {SUPPORTED})")).into()),
    }
}

pub fn extract(filename: &str, bytes: &[u8]) -> Result<(Format, Extracted)> {
    let format = detect(filename, bytes)?;
    let mut out = match format {
        Format::Pdf => pdf(bytes)?,
        Format::Docx => docx(bytes).map_err(|e| Invalid(format!("cannot read Word file `{filename}`: {e:#}")))?,
        Format::Pptx => pptx(bytes).map_err(|e| Invalid(format!("cannot read PowerPoint file `{filename}`: {e:#}")))?,
        Format::Doc => Extracted { text: run_tool("catdoc", &["-w", "-d", "utf-8"], bytes, "doc", &[])?, ..Default::default() },
        Format::Ppt => Extracted { text: run_tool("catppt", &["-d", "utf-8"], bytes, "ppt", &[])?, ..Default::default() },
        Format::Html => {
            let html = String::from_utf8_lossy(bytes);
            let mut images = Vec::new();
            let text = html_markdown(main_region(&html), Some(&mut images));
            caption_from_following(&text, &mut images);
            Extracted { title: html_title(&html), text, images, ..Default::default() }
        }
        Format::Markdown | Format::Text => {
            let raw = String::from_utf8_lossy(bytes);
            Extracted { text: raw.trim_start_matches('\u{feff}').to_string(), ..Default::default() }
        }
    };
    out.structured = out.structured || matches!(format, Format::Markdown | Format::Html | Format::Docx | Format::Pptx);
    out.title = out.title.filter(|t| !t.trim().is_empty());
    if format == Format::Markdown {
        let fm = front_matter(&out.text);
        out.text = fm.body;
        out.title = out.title.or(fm.title);
        out.tags = fm.tags;
        out.status = fm.status;
    }
    // A PDF's first big heading is usually a cover line: its title comes from its
    // properties, else the file name.
    if out.structured && format != Format::Pdf {
        out.title = out.title.or_else(|| first_heading(&out.text).filter(|h| !is_generic_heading(h)));
    }
    if out.text.trim().is_empty() {
        let hint = if format == Format::Pdf { " (scanned PDF? text recognition is not supported)" } else { "" };
        return Err(Invalid(format!("no text could be extracted from `{filename}`{hint}")).into());
    }
    Ok((format, out))
}

// ---- Markdown ---------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
pub struct FrontMatter {
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub body: String,
}

fn unquote(v: &str) -> String {
    v.trim().trim_matches(|c| c == '"' || c == '\'').trim().to_string()
}

/// Reads a leading `---` YAML block (only `title`, `tags`, `status` are used).
pub fn front_matter(text: &str) -> FrontMatter {
    let mut fm = FrontMatter { body: text.to_string(), ..Default::default() };
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return fm;
    };
    let Some(end) = rest.lines().position(|l| l.trim_end() == "---" || l.trim_end() == "...") else {
        return fm;
    };
    let lines: Vec<&str> = rest.lines().collect();
    let mut key = String::new();
    for line in &lines[..end] {
        if let Some(item) = line.trim_start().strip_prefix("- ") {
            if key == "tags" || key == "keywords" {
                fm.tags.push(unquote(item));
            }
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        key = k.trim().to_lowercase();
        let v = v.trim();
        match key.as_str() {
            "title" | "titre" if !v.is_empty() => fm.title = Some(unquote(v)),
            "status" | "statut" if !v.is_empty() => fm.status = Some(unquote(v).to_uppercase()),
            "draft" if v == "true" => fm.status = Some("DRAFT".into()),
            "tags" | "keywords" if !v.is_empty() => {
                fm.tags.extend(v.trim_start_matches('[').trim_end_matches(']').split(',').map(unquote));
            }
            _ => {}
        }
    }
    fm.tags.retain(|t| !t.is_empty());
    fm.body = lines[end + 1..].join("\n");
    fm
}

/// Headings that open many documents without naming them ("Introduction", "Sommaire"…): the
/// file name is a better title.
pub fn is_generic_heading(heading: &str) -> bool {
    const GENERIC: &[&str] = &[
        "introduction", "sommaire", "table des matières", "table des matieres", "contents", "table of contents", "overview",
        "summary", "résumé", "resume", "préambule", "preambule", "avant-propos", "foreword", "preface", "préface", "contexte",
        "context", "objet", "purpose", "scope", "périmètre", "historique", "historique des versions", "suivi des versions",
        "version history", "revision history", "document history", "document control", "glossaire", "glossary", "abstract",
    ];
    let h = heading.trim().trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ' ').trim().to_lowercase();
    GENERIC.contains(&h.trim_end_matches([':', '.']).trim())
}

pub fn first_heading(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("# "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

// ---- HTML ---------------------------------------------------------------------------

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                n if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16).ok().and_then(char::from_u32),
                n if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            c.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Small HTML to markdown conversion: headings, paragraphs, lists, table rows; scripts,
/// styles and the document head are dropped. Enough for saved web pages and exports.
/// Title of an HTML page: its `og:title`, else `<title>`, else its first `<h1>`.
pub fn html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let clean = |s: &str| {
        let text = html_to_markdown(s);
        let t = text.split_whitespace().collect::<Vec<_>>().join(" ").trim_start_matches('#').trim().to_string();
        (!t.is_empty() && t.chars().count() <= 200).then_some(t)
    };
    if let Some(p) = lower.find("property=\"og:title\"").or_else(|| lower.find("property='og:title'")) {
        let tag_start = lower[..p].rfind('<').unwrap_or(p);
        let tag_end = lower[p..].find('>').map_or(lower.len(), |e| p + e);
        let tag = &html[tag_start..tag_end];
        if let Some(c) = tag.to_ascii_lowercase().find("content=") {
            let rest = &tag[c + 8..];
            let quote = rest.chars().next().filter(|q| *q == '"' || *q == '\'');
            if let Some(q) = quote {
                if let Some(end) = rest[1..].find(q) {
                    if let Some(t) = clean(&rest[1..1 + end]) {
                        return Some(t);
                    }
                }
            }
        }
    }
    for (open, close) in [("<title", "</title>"), ("<h1", "</h1>")] {
        if let Some(start) = lower.find(open) {
            let body = start + lower[start..].find('>').map_or(0, |g| g + 1);
            if let Some(end) = lower[body..].find(close) {
                if let Some(t) = clean(&html[body..body + end]) {
                    return Some(t);
                }
            }
        }
    }
    None
}

/// The main content of a page: its `<main>`, else its articles, else its `<body>`; menus,
/// headers and footers around it are left out.
pub fn main_region(html: &str) -> &str {
    let lower = html.to_ascii_lowercase();
    let opening = |tag: &str| {
        lower.match_indices(&format!("<{tag}")).map(|(i, _)| i).find(|&i| {
            lower[i + tag.len() + 1..].starts_with(|c: char| c == '>' || c.is_ascii_whitespace())
        })
    };
    for tag in ["main", "article", "body"] {
        if let (Some(start), Some(end)) = (opening(tag), lower.rfind(&format!("</{tag}>"))) {
            let inner = start + lower[start..].find('>').map_or(0, |g| g + 1);
            // Too little text in <main> or <article>: a shell filled by scripts, keep the body.
            if end > inner && (tag == "body" || html_to_markdown(&html[inner..end]).len() > 200) {
                return &html[inner..end];
            }
        }
    }
    html
}

/// An element that is page furniture by its role, id or class: navigation, menus, sidebars,
/// tables of contents, cookie banners, sharing buttons, category lists…
fn is_furniture(tag: &str) -> bool {
    const ROLES: [&str; 5] = ["navigation", "banner", "contentinfo", "complementary", "search"];
    const WORDS: [&str; 23] = [
        "navbox", "navbar", "navigation", "menu", "sidebar", "breadcrumb", "cookie", "consent", "banner",
        "catlinks", "printfooter", "portlet", "toc", "share", "social", "newsletter", "related", "advert",
        "promo", "skip-link", "language", "comments", "editsection",
    ];
    let attr = |name: &str| {
        let key = format!("{name}=");
        tag.find(&key).map(|p| {
            let rest = &tag[p + key.len()..];
            let quote = rest.chars().next().filter(|q| *q == '"' || *q == '\'');
            match quote {
                Some(q) => rest[1..].split(q).next().unwrap_or("").to_string(),
                None => rest.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or("").to_string(),
            }
        })
    };
    if attr("role").is_some_and(|r| ROLES.contains(&r.trim())) || attr("aria-hidden").as_deref() == Some("true") {
        return true;
    }
    let names = format!("{} {}", attr("id").unwrap_or_default(), attr("class").unwrap_or_default());
    names
        .split(|c: char| c.is_whitespace())
        .filter(|n| !n.is_empty())
        .any(|n| WORDS.iter().any(|w| n == *w || n.starts_with(&format!("{w}-")) || n.ends_with(&format!("-{w}")) || n.contains(&format!("-{w}-"))))
}

const VOID_TAGS: [&str; 14] = ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];

pub fn html_to_markdown(html: &str) -> String {
    html_markdown(html, None)
}

/// Value of an attribute in a raw tag (`img src="…" alt='…'`), case kept.
fn tag_attr(raw: &str, name: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find(name) {
        let at = from + p;
        from = at + name.len();
        // A whole attribute name, followed by `=`.
        if at > 0 && !lower.as_bytes()[at - 1].is_ascii_whitespace() {
            continue;
        }
        let rest = raw[at + name.len()..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or(""),
            _ => rest.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or(""),
        };
        return Some(decode_entities(value));
    }
    None
}

/// On a web page, an image without alt text is usually followed by what it illustrates (a card's
/// title, a figure's legend): that short line becomes its caption.
fn caption_from_following(text: &str, images: &mut [ExtractedImage]) {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    for (i, line) in lines.iter().enumerate() {
        let Some(n) = line.strip_prefix(IMAGE_MARKER).and_then(|r| r.strip_suffix("-->")).and_then(|v| v.trim().parse::<usize>().ok()) else {
            continue;
        };
        let Some(image) = images.get_mut(n).filter(|img| img.alt.is_none()) else { continue };
        if let Some(next) = lines.get(i + 1).filter(|l| !l.starts_with(IMAGE_MARKER)) {
            let caption = next.trim_start_matches(['#', '-', ' ']).trim();
            if (8..=160).contains(&caption.chars().count()) {
                image.alt = Some(caption.to_string());
            }
        }
    }
}

/// Bytes of a `data:image/…;base64,…` address.
fn data_uri(src: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let (head, data) = src.strip_prefix("data:")?.split_once(',')?;
    if !head.ends_with(";base64") || !head.starts_with("image/") {
        return None;
    }
    base64::engine::general_purpose::STANDARD.decode(data.trim()).ok()
}

/// An `<img>` of the content: its address (or embedded bytes) and alt text; small ones
/// (declared under 48 px: icons, spacers, trackers) are left out.
fn html_image(raw: &str) -> Option<ExtractedImage> {
    let size = |n: &str| tag_attr(raw, n).and_then(|v| v.trim_end_matches("px").parse::<u32>().ok());
    if size("width").is_some_and(|w| w < 48) || size("height").is_some_and(|h| h < 48) {
        return None;
    }
    // Lazy-loaded pages keep the real address in data-src and a placeholder in src.
    let src = tag_attr(raw, "src").filter(|s| !s.trim().is_empty() && !s.starts_with("data:image/gif") && !s.starts_with("data:image/svg"));
    let src = tag_attr(raw, "data-src").or(src)?;
    let alt = useful_alt(tag_attr(raw, "alt"));
    match data_uri(&src) {
        Some(bytes) => Some(ExtractedImage { bytes, alt, source: None }),
        None if src.starts_with("data:") => None,
        None => Some(ExtractedImage { bytes: Vec::new(), alt, source: Some(src) }),
    }
}

/// HTML to Markdown; with `images`, each `<img>` of the content becomes a `<!-- image N -->`
/// marker and an entry of `images`.
fn html_markdown(html: &str, mut images: Option<&mut Vec<ExtractedImage>>) -> String {
    let lower = html.to_ascii_lowercase();
    // A <figcaption> names the image of its <figure> when the image has no alt text.
    let mut figure_image: Option<usize> = None;
    let mut caption_start: Option<usize> = None;
    let mut out = String::new();
    let mut i = 0;
    let mut skip_until: Option<&str> = None;
    // Inside a furniture element: its tag name and how deeply the same tag is nested.
    let mut furniture: Option<(String, usize)> = None;
    while i < html.len() {
        if let Some(close) = skip_until {
            match lower[i..].find(close) {
                Some(p) => {
                    i += p + close.len();
                    skip_until = None;
                }
                None => break,
            }
            continue;
        }
        let Some(lt) = html[i..].find('<') else {
            if furniture.is_none() {
                out.push_str(&html[i..]);
            }
            break;
        };
        if furniture.is_none() {
            out.push_str(&html[i..i + lt]);
        }
        i += lt;
        let Some(gt) = html[i..].find('>') else { break };
        let tag = lower[i + 1..i + gt].trim();
        let raw_tag = html[i + 1..i + gt].trim();
        i += gt + 1;
        let closing = tag.starts_with('/');
        let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        let self_closing = tag.ends_with('/') || VOID_TAGS.contains(&name.as_str());
        if let Some((skipped, depth)) = furniture.as_mut() {
            // Drop everything until the furniture element closes (text included).
            if *skipped == name && !self_closing {
                if closing {
                    *depth -= 1;
                } else {
                    *depth += 1;
                }
            }
            if *depth == 0 {
                furniture = None;
            }
            continue;
        }
        if !closing && !self_closing && !name.is_empty() && is_furniture(tag) {
            furniture = Some((name, 1));
            continue;
        }
        match name.as_str() {
            // Code, page furniture and controls carry no content.
            "script" | "style" | "head" | "noscript" | "svg" | "nav" | "aside" | "footer" | "form" | "template"
            | "iframe" | "select" | "button"
                if !closing && !tag.ends_with('/') =>
            {
                skip_until = Some(match name.as_str() {
                    "script" => "</script>",
                    "style" => "</style>",
                    "head" => "</head>",
                    "noscript" => "</noscript>",
                    "nav" => "</nav>",
                    "aside" => "</aside>",
                    "footer" => "</footer>",
                    "form" => "</form>",
                    "template" => "</template>",
                    "iframe" => "</iframe>",
                    "select" => "</select>",
                    "button" => "</button>",
                    _ => "</svg>",
                });
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if closing {
                    out.push_str("\n\n");
                } else {
                    let level = name[1..].parse::<usize>().unwrap_or(1);
                    out.push_str(&format!("\n\n{} ", "#".repeat(level)));
                }
            }
            "img" if images.is_some() => {
                if let (Some(list), Some(image)) = (images.as_deref_mut(), html_image(raw_tag)) {
                    list.push(image);
                    figure_image = Some(list.len() - 1);
                    out.push_str(&format!("\n\n{IMAGE_MARKER}{} -->\n\n", list.len() - 1));
                }
            }
            "figure" if closing => figure_image = None,
            "figcaption" if !closing => caption_start = Some(out.len()),
            "figcaption" => {
                if let (Some(start), Some(n), Some(list)) = (caption_start.take(), figure_image, images.as_deref_mut()) {
                    let caption = decode_entities(&out[start..]).split_whitespace().collect::<Vec<_>>().join(" ");
                    if let Some(image) = list.get_mut(n).filter(|i| i.alt.is_none() && !caption.is_empty()) {
                        image.alt = Some(caption.chars().take(300).collect());
                    }
                }
                out.push_str("\n\n");
            }
            "li" if !closing => out.push_str("\n- "),
            "td" | "th" if !closing => out.push_str(" | "),
            "tr" if closing => out.push_str(" |\n"),
            "br" => out.push('\n'),
            "p" | "div" | "section" | "article" | "ul" | "ol" | "table" | "blockquote" | "pre" | "tr" => {
                out.push_str("\n\n");
            }
            _ => {}
        }
    }
    // Collapse the whitespace runs left by the markup, keeping paragraph breaks.
    let decoded = decode_entities(&out);
    let mut text = String::new();
    let mut blank = 0;
    for line in decoded.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            continue;
        }
        if blank > 0 && !text.is_empty() {
            text.push('\n');
        }
        blank = 0;
        text.push_str(&line);
        text.push('\n');
    }
    text
}

// ---- External tools ---------------------------------------------------------------

/// Runs `tool <args> <tempfile> <trailing>` and returns its standard output.
fn run_tool(tool: &str, args: &[&str], bytes: &[u8], ext: &str, trailing: &[&str]) -> Result<String> {
    let path = std::env::temp_dir().join(format!("innerrag-{}.{ext}", uuid::Uuid::new_v4()));
    std::fs::write(&path, bytes).context("writing temporary file")?;
    let output = Command::new(tool).args(args).arg(&path).args(trailing).output();
    let _ = std::fs::remove_file(&path);
    let output = output.map_err(|e| anyhow!("`{tool}` is not available ({e}); it ships in the innerrag image"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(Invalid(format!("{tool} could not read the file: {}", err.trim())).into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---- PDF to markdown ---------------------------------------------------------------

/// Page markers kept in the markdown so chunks know their page; the reader shows them discreetly.
pub const PAGE_MARKER: &str = "<!-- page ";

#[derive(Debug, Clone)]
struct Run {
    top: f32,
    left: f32,
    height: f32,
    font: usize,
    bold: bool,
    text: String,
}

#[derive(Debug)]
struct Line {
    page: usize,
    top: f32,
    left: f32,
    height: f32,
    size: f32,
    mono: bool,
    bold: bool,
    text: String,
    /// An image placed at this height on the page (its index in the extracted images).
    image: Option<usize>,
}

#[derive(Debug, Clone)]
struct PdfImage {
    top: f32,
    left: f32,
    width: f32,
    height: f32,
    src: String,
}

/// Pages of `pdftohtml -xml`: number, height, text runs, images.
type PdfPage = (usize, f32, Vec<Run>, Vec<PdfImage>);

struct Font {
    size: f32,
    mono: bool,
    bold: bool,
}

fn is_mono(family: &str) -> bool {
    let f = family.to_lowercase();
    ["mono", "courier", "consol", "code", "menlo", "typewriter"].iter().any(|m| f.contains(m))
}

/// Parses `pdftohtml -xml` output into positioned runs per page.
fn pdf_runs(xml: &str) -> Result<(Vec<PdfPage>, HashMap<usize, Font>)> {
    let mut reader = Reader::from_str(xml);
    let mut fonts = HashMap::new();
    let mut pages: Vec<PdfPage> = Vec::new();
    let mut current: Option<Run> = None;
    let mut bold_depth = 0;
    let num = |e: &BytesStart, k: &[u8]| attr(e, k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"fontspec" => {
                let id = num(&e, b"id") as usize;
                let family = attr(&e, b"family").unwrap_or_default();
                let lower = family.to_lowercase();
                fonts.insert(id, Font {
                    size: num(&e, b"size"),
                    mono: is_mono(&family),
                    bold: ["bold", "black", "heavy", "semibold"].iter().any(|b| lower.contains(b)),
                });
            }
            Event::Start(e) if e.local_name().as_ref() == b"page" => {
                pages.push((num(&e, b"number") as usize, num(&e, b"height"), Vec::new(), Vec::new()));
            }
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"image" => {
                if let (Some(src), Some(page)) = (attr(&e, b"src"), pages.last_mut()) {
                    page.3.push(PdfImage {
                        top: num(&e, b"top"),
                        left: num(&e, b"left"),
                        width: num(&e, b"width"),
                        height: num(&e, b"height"),
                        src,
                    });
                }
            }
            Event::Start(e) if e.local_name().as_ref() == b"text" => {
                current = Some(Run {
                    top: num(&e, b"top"),
                    left: num(&e, b"left"),
                    height: num(&e, b"height"),
                    font: num(&e, b"font") as usize,
                    bold: false,
                    text: String::new(),
                });
                bold_depth = 0;
            }
            Event::Start(e) if e.local_name().as_ref() == b"b" => bold_depth += 1,
            Event::Text(t) => {
                if let Some(run) = current.as_mut() {
                    if bold_depth > 0 {
                        run.bold = true;
                    }
                    run.text.push_str(&t.unescape()?);
                }
            }
            Event::End(e) if e.local_name().as_ref() == b"b" => bold_depth -= 1,
            Event::End(e) if e.local_name().as_ref() == b"text" => {
                if let (Some(run), Some(page)) = (current.take(), pages.last_mut()) {
                    page.2.push(run);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((pages, fonts))
}

/// "Fragment………………… 133" (table of contents leaders) → "Fragment … 133".
fn collapse_leaders(text: &str) -> String {
    let is_leader = |c: char| matches!(c, '.' | '·' | '…' | '\u{fffd}' | '_');
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if is_leader(chars[i]) {
            let start = i;
            while i < chars.len() && (is_leader(chars[i]) || (chars[i] == ' ' && i + 1 < chars.len() && is_leader(chars[i + 1]))) {
                i += 1;
            }
            if i - start >= 4 {
                out.push_str(" … ");
            } else {
                out.extend(&chars[start..i]);
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_margin(text: &str) -> String {
    text.chars().filter(|c| !c.is_ascii_digit()).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Converts `pdftohtml -xml` output to markdown: headings from font sizes, reflowed
/// paragraphs, code blocks and inline code from monospaced fonts, running headers,
/// footers and page numbers removed, `<!-- page N -->` markers between pages.
#[cfg(test)]
pub fn pdf_xml_to_markdown(xml: &str) -> Result<String> {
    Ok(pdf_layout(xml, &mut |_| None)?.0)
}

/// Images smaller than this (in page points) are bullets, icons or rules, not figures.
const PDF_MIN_IMAGE: f32 = 60.0;

/// Same as `pdf_xml_to_markdown`, with the images `load` can read placed where they are on
/// their page (`<!-- image N -->`).
fn pdf_layout(xml: &str, load: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Result<(String, Vec<ExtractedImage>)> {
    let (pages, fonts) = pdf_runs(xml)?;
    let mut images: Vec<ExtractedImage> = Vec::new();
    let font = |id: usize| fonts.get(&id).map_or((0.0, false, false), |f| (f.size, f.mono, f.bold));

    // 1. Lines: runs sharing a baseline.
    let mut lines: Vec<Line> = Vec::new();
    let mut page_heights = HashMap::new();
    for (page, height, runs, page_images) in &pages {
        page_heights.insert(*page, *height);
        let first_line = lines.len();
        let mut runs = runs.clone();
        runs.sort_by(|a, b| a.top.total_cmp(&b.top).then(a.left.total_cmp(&b.left)));
        let mut group: Vec<Run> = Vec::new();
        let flush = |group: &mut Vec<Run>, lines: &mut Vec<Line>| {
            if group.is_empty() {
                return;
            }
            group.sort_by(|a, b| a.left.total_cmp(&b.left));
            let chars = |pred: &dyn Fn(&Run) -> bool| group.iter().filter(|r| pred(r)).map(|r| r.text.trim().len()).sum::<usize>();
            let total = chars(&|_| true).max(1);
            let mono_chars = chars(&|r| font(r.font).1);
            let line_mono = mono_chars * 10 >= total * 9;
            let bold = chars(&|r| r.bold || font(r.font).2) * 10 >= total * 9;
            // Dominant size by characters.
            let mut sizes: HashMap<i32, usize> = HashMap::new();
            for r in group.iter() {
                *sizes.entry(font(r.font).0.round() as i32).or_default() += r.text.trim().len().max(1);
            }
            let size = sizes.into_iter().max_by_key(|(_, n)| *n).map_or(0.0, |(s, _)| s as f32);
            let mut text = String::new();
            for r in group.iter() {
                let piece = &r.text;
                if !line_mono && font(r.font).1 && !piece.trim().is_empty() {
                    let lead = &piece[..piece.len() - piece.trim_start().len()];
                    let trail = &piece[piece.trim_end().len()..];
                    text.push_str(&format!("{lead}`{}`{trail}", piece.trim()));
                } else {
                    text.push_str(piece);
                }
            }
            let text = if line_mono { text.trim_end().to_string() } else { collapse_leaders(&text) };
            if !text.trim().is_empty() {
                lines.push(Line {
                    page: *page,
                    top: group[0].top,
                    left: group.iter().map(|r| r.left).fold(f32::MAX, f32::min),
                    height: group.iter().map(|r| r.height).fold(0.0, f32::max),
                    size,
                    mono: line_mono,
                    bold,
                    text,
                    image: None,
                });
            }
            group.clear();
        };
        for run in runs {
            if group.last().is_some_and(|g: &Run| (g.top - run.top).abs() > 3.0) {
                flush(&mut group, &mut lines);
            }
            group.push(run);
        }
        flush(&mut group, &mut lines);
        // Figures take their place among the lines of their page, by height.
        for img in page_images.iter().filter(|i| i.width >= PDF_MIN_IMAGE && i.height >= PDF_MIN_IMAGE) {
            let Some(bytes) = load(&img.src) else { continue };
            images.push(ExtractedImage { bytes, alt: None, source: None });
            lines.push(Line {
                page: *page,
                top: img.top,
                left: img.left,
                height: img.height,
                size: 0.0,
                mono: false,
                bold: false,
                text: String::new(),
                image: Some(images.len() - 1),
            });
        }
        lines[first_line..].sort_by(|a, b| a.top.total_cmp(&b.top));
    }
    if lines.iter().all(|l| l.image.is_some()) {
        return Ok((String::new(), images));
    }

    // 2. Running headers / footers: text repeated in the top or bottom margin of many pages.
    let in_margin = |l: &Line| {
        let h = page_heights.get(&l.page).copied().unwrap_or(1000.0).max(1.0);
        l.top < h * 0.09 || l.top > h * 0.91
    };
    let mut margin_counts: HashMap<String, usize> = HashMap::new();
    for l in lines.iter().filter(|l| in_margin(l)) {
        *margin_counts.entry(normalize_margin(&l.text)).or_default() += 1;
    }
    let repeat_threshold = (pages.len() / 20).max(3);
    lines.retain(|l| {
        if l.image.is_some() {
            // An image entirely in a margin is a logo or a running ornament.
            let h = page_heights.get(&l.page).copied().unwrap_or(1000.0).max(1.0);
            return !(l.top + l.height < h * 0.09 || l.top > h * 0.91);
        }
        if !in_margin(l) {
            return true;
        }
        let key = normalize_margin(&l.text);
        let page_number = key.is_empty() || key.chars().all(|c| !c.is_alphanumeric());
        !(page_number || margin_counts.get(&key).copied().unwrap_or(0) >= repeat_threshold)
    });

    // 3. Body size (most characters) and heading levels (larger sizes, rarest first).
    let mut by_size: HashMap<i32, usize> = HashMap::new();
    let mut short_lines: HashMap<i32, usize> = HashMap::new();
    for l in &lines {
        let s = l.size.round() as i32;
        *by_size.entry(s).or_default() += l.text.len();
        if !l.mono && l.text.chars().count() <= 140 {
            *short_lines.entry(s).or_default() += 1;
        }
    }
    let body = by_size.iter().max_by_key(|(_, n)| **n).map_or(0, |(s, _)| *s);
    // A heading size is used again and again (sections), unlike cover or poster sizes.
    let min_uses = (pages.len() / 50).max(2);
    let mut heading_sizes: Vec<i32> = short_lines
        .iter()
        .filter(|(s, n)| **s >= body + 2 && **n >= min_uses)
        .map(|(s, _)| *s)
        .collect();
    heading_sizes.sort_unstable_by(|a, b| b.cmp(a));
    heading_sizes.truncate(4);
    let level_of = |l: &Line| -> Option<usize> {
        if l.mono || l.text.chars().count() > 140 {
            return None;
        }
        let s = l.size.round() as i32;
        heading_sizes.iter().position(|h| *h == s).map(|i| i + 1)
    };

    // 4. Blocks: headings, code, list items, reflowed paragraphs.
    let mut out = String::new();
    let mut para = String::new();
    let mut code: Vec<String> = Vec::new();
    let mut code_left = 0.0f32;
    let mut prev: Option<&Line> = None;
    let mut page = 0usize;
    let flush_para = |para: &mut String, out: &mut String| {
        if !para.trim().is_empty() {
            out.push_str(para.trim());
            out.push_str("\n\n");
        }
        para.clear();
    };
    let flush_code = |code: &mut Vec<String>, out: &mut String| {
        while code.last().is_some_and(|l| l.trim().is_empty()) {
            code.pop();
        }
        if !code.is_empty() {
            out.push_str("```\n");
            out.push_str(&code.join("\n"));
            out.push_str("\n```\n\n");
        }
        code.clear();
    };
    let mut pending_heading: Option<(usize, String, usize, f32)> = None;
    for l in &lines {
        if l.page != page {
            // Paragraphs may continue on the next page: only close code and headings here.
            flush_code(&mut code, &mut out);
            if let Some((level, text, _, _)) = pending_heading.take() {
                out.push_str(&format!("{} {}\n\n", "#".repeat(level), text));
            }
            if para.is_empty() {
                out.push_str(&format!("{PAGE_MARKER}{} -->\n\n", l.page));
            } else {
                // Keep the marker out of the sentence; it lands after this paragraph.
                para.push_str(&format!("\u{0}{}\u{0}", l.page));
            }
            page = l.page;
        }
        if let Some(n) = l.image {
            // A figure ends the paragraph and the code around it.
            if let Some((lv, text, _, _)) = pending_heading.take() {
                out.push_str(&format!("{} {}\n\n", "#".repeat(lv), text));
            }
            flush_para(&mut para, &mut out);
            flush_code(&mut code, &mut out);
            out.push_str(&format!("{IMAGE_MARKER}{n} -->\n\n"));
            prev = None;
            continue;
        }
        if let Some(level) = level_of(l) {
            flush_para(&mut para, &mut out);
            flush_code(&mut code, &mut out);
            // Consecutive lines of one heading (same size, close together) are merged.
            match pending_heading.as_mut() {
                Some((lv, text, pg, top)) if *lv == level && *pg == l.page && (l.top - *top) < l.height * 2.2 => {
                    text.push(' ');
                    text.push_str(&l.text);
                    *top = l.top;
                }
                _ => {
                    if let Some((lv, text, _, _)) = pending_heading.take() {
                        out.push_str(&format!("{} {}\n\n", "#".repeat(lv), text));
                    }
                    pending_heading = Some((level, l.text.clone(), l.page, l.top));
                }
            }
            prev = Some(l);
            continue;
        }
        if let Some((lv, text, _, _)) = pending_heading.take() {
            out.push_str(&format!("{} {}\n\n", "#".repeat(lv), text));
        }
        if l.mono {
            flush_para(&mut para, &mut out);
            if code.is_empty() {
                code_left = l.left;
            }
            let indent = (((l.left - code_left).max(0.0)) / 7.0).round() as usize;
            code.push(format!("{}{}", " ".repeat(indent), l.text));
            prev = Some(l);
            continue;
        }
        flush_code(&mut code, &mut out);
        let bullet = ["•", "●", "■", "▪", "◦", "–", "-", "*"]
            .iter()
            .find_map(|b| l.text.strip_prefix(b).filter(|r| r.starts_with(' ')).map(str::trim_start));
        let gap_break = prev.is_some_and(|p| p.page == l.page && l.top - p.top > p.height.max(l.height) * 1.45);
        let new_block = bullet.is_some() || gap_break || prev.is_some_and(|p| p.mono) || para.is_empty();
        if new_block {
            flush_para(&mut para, &mut out);
            match bullet {
                Some(item) => {
                    para.push_str("- ");
                    para.push_str(item);
                }
                None => {
                    if l.bold && l.text.chars().count() < 100 && !l.text.ends_with('.') {
                        // A bold label line stands on its own.
                        para.push_str(&format!("**{}**", l.text));
                        flush_para(&mut para, &mut out);
                    } else if l.text.starts_with('#') {
                        // Not a heading (shell or config comment in body text).
                        para.push('\\');
                        para.push_str(&l.text);
                    } else {
                        para.push_str(&l.text);
                    }
                }
            }
        } else if para.ends_with('-') && l.text.chars().next().is_some_and(char::is_lowercase) {
            para.pop();
            para.push_str(&l.text);
        } else {
            para.push(' ');
            para.push_str(&l.text);
        }
        prev = Some(l);
    }
    if let Some((lv, text, _, _)) = pending_heading.take() {
        out.push_str(&format!("{} {}\n\n", "#".repeat(lv), text));
    }
    flush_para(&mut para, &mut out);
    flush_code(&mut code, &mut out);
    // Page markers that fell inside a paragraph go right after it; lone "Chapter" headings
    // (the number is drawn apart) are dropped; heading levels are renumbered without gaps.
    let generic = ["chapter", "chapitre", "part", "partie", "appendix", "annexe", "section"];
    let mut used_levels: Vec<usize> = out
        .split("\n\n")
        .filter_map(|b| {
            let level = b.chars().take_while(|&c| c == '#').count();
            let text = b[level..].trim().to_lowercase();
            ((1..=6).contains(&level) && b[level..].starts_with(' ') && !generic.contains(&text.as_str())).then_some(level)
        })
        .collect();
    used_levels.sort_unstable();
    used_levels.dedup();
    let mut fixed = String::with_capacity(out.len());
    for block in out.split("\n\n") {
        let level = block.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&level) && block[level..].starts_with(' ') {
            let text = block[level..].trim();
            if generic.contains(&text.to_lowercase().as_str()) {
                continue;
            }
            let new_level = used_levels.iter().position(|l| *l == level).map_or(level, |i| i + 1);
            fixed.push_str(&format!("{} {}\n\n", "#".repeat(new_level), text));
            continue;
        }
        if !block.contains('\u{0}') {
            fixed.push_str(block);
            fixed.push_str("\n\n");
            continue;
        }
        let mut text = String::new();
        let mut markers = Vec::new();
        for (i, part) in block.split('\u{0}').enumerate() {
            if i % 2 == 1 {
                markers.push(part.to_string());
            } else {
                text.push_str(part);
            }
        }
        fixed.push_str(text.trim());
        fixed.push_str("\n\n");
        for m in markers {
            fixed.push_str(&format!("{PAGE_MARKER}{m} -->\n\n"));
        }
    }
    Ok((fixed, images))
}

/// `pdftohtml -xml` with the embedded images written next to the XML, in a temporary folder.
fn pdf_with_images(bytes: &[u8]) -> Result<(String, Vec<ExtractedImage>)> {
    let dir = std::env::temp_dir().join(format!("innerrag-pdf-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).context("creating a temporary folder")?;
    let result = (|| {
        let input = dir.join("in.pdf");
        std::fs::write(&input, bytes).context("writing temporary file")?;
        let output = Command::new("pdftohtml")
            .args(["-xml", "-q", "-fmt", "png"])
            .arg(&input)
            .arg(dir.join("out"))
            .output()
            .map_err(|e| anyhow!("`pdftohtml` is not available ({e}); it ships in the innerrag image"))?;
        if !output.status.success() {
            return Err(Invalid(format!("pdftohtml could not read the file: {}", String::from_utf8_lossy(&output.stderr).trim())).into());
        }
        let xml = std::fs::read_to_string(dir.join("out.xml")).context("reading pdftohtml output")?;
        let mut load = |src: &str| {
            let path = Path::new(src);
            let path = if path.is_absolute() { path.to_path_buf() } else { dir.join(path) };
            // Only files pdftohtml wrote in the temporary folder.
            path.starts_with(&dir).then(|| std::fs::read(&path).ok()).flatten()
        };
        pdf_layout(&xml, &mut load)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn pdf(bytes: &[u8]) -> Result<Extracted> {
    let title = run_tool("pdfinfo", &[], bytes, "pdf", &[]).ok().and_then(|info| {
        info.lines()
            .find_map(|l| l.strip_prefix("Title:"))
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    });
    // Structured conversion with the figures first; plain text when the layout cannot be read.
    let pages = run_tool("pdfinfo", &[], bytes, "pdf", &[])
        .ok()
        .and_then(|info| info.lines().find_map(|l| l.strip_prefix("Pages:")).and_then(|p| p.trim().parse::<usize>().ok()));
    match pdf_with_images(bytes) {
        Ok((md, images)) if md.split_whitespace().count() > pages.unwrap_or(1).max(1) * 5 => {
            return Ok(Extracted { text: md, images, title, pages, structured: true, ..Default::default() });
        }
        Ok(_) => tracing::info!("PDF layout gave little text, falling back to pdftotext"),
        Err(e) => tracing::warn!("PDF layout conversion failed ({e:#}), falling back to pdftotext"),
    }
    let mut plain = pdf_plain(bytes)?;
    plain.title = title;
    Ok(plain)
}

fn pdf_plain(bytes: &[u8]) -> Result<Extracted> {
    // Pages are separated by form feeds.
    // "-" sends the text to stdout instead of a .txt file next to the PDF.
    let raw = run_tool("pdftotext", &["-q", "-enc", "UTF-8"], bytes, "pdf", &["-"])?;
    let pages = raw.matches('\u{c}').count().max(1);
    let mut text = String::with_capacity(raw.len());
    for page in raw.split('\u{c}') {
        text.push_str(&join_hyphenated(page.trim()));
        text.push_str("\n\n");
    }
    Ok(Extracted { text, pages: Some(pages), ..Default::default() })
}

/// Rejoins words split across lines by a hyphen ("infor-\nmation").
fn join_hyphenated(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let next_lower = lines.peek().and_then(|n| n.trim_start().chars().next()).is_some_and(char::is_lowercase);
        if let Some(stem) = line.strip_suffix('-').filter(|s| s.chars().last().is_some_and(char::is_alphabetic)) {
            if next_lower {
                out.push_str(stem);
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

// ---- OOXML ------------------------------------------------------------------------

fn zip_text(archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut file = archive.by_name(name).ok()?;
    let mut s = String::new();
    file.read_to_string(&mut s).ok()?;
    Some(s)
}

fn attr(e: &BytesStart, qualified: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == qualified)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn local_attr(e: &BytesStart, local: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == local)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// `dc:title` from docProps/core.xml.
fn core_title(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Option<String> {
    let xml = zip_text(archive, "docProps/core.xml")?;
    let mut reader = Reader::from_str(&xml);
    let mut in_title = false;
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) if e.local_name().as_ref() == b"title" => in_title = true,
            Event::Text(t) if in_title => {
                return t.unescape().ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            }
            Event::End(e) if e.local_name().as_ref() == b"title" => in_title = false,
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn heading_level(style: &str) -> Option<usize> {
    let s = style.to_lowercase().replace([' ', '-', '_'], "");
    if s == "title" || s == "titre" {
        return Some(1);
    }
    if s == "subtitle" || s == "soustitre" {
        return Some(2);
    }
    let digits = s.strip_prefix("heading").or_else(|| s.strip_prefix("titre"))?;
    digits.parse::<usize>().ok().filter(|n| (1..=6).contains(n))
}

pub fn docx(bytes: &[u8]) -> Result<Extracted> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    let xml = zip_text(&mut archive, "word/document.xml").ok_or_else(|| anyhow!("word/document.xml missing"))?;
    let title = core_title(&mut archive);
    // Images are reached through the document's relationships: rId → word/media/image1.png.
    let media: HashMap<String, String> = relationships(&mut archive, "word/_rels/document.xml.rels")
        .into_iter()
        .filter(|(_, _, kind)| kind.ends_with("/image"))
        .map(|(id, target, _)| (id, resolve("word", &target)))
        .collect();
    let mut images: Vec<ExtractedImage> = Vec::new();
    let mut pending_images: Vec<usize> = Vec::new();
    let mut alt: Option<String> = None;

    let mut reader = Reader::from_str(&xml);
    let mut out = String::new();
    let mut para = String::new();
    let mut in_text = false;
    let mut heading: Option<usize> = None;
    let mut list = false;
    let mut table_depth = 0usize;
    let mut cell = String::new();
    let mut row: Vec<String> = Vec::new();

    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"t" => in_text = true,
                b"p" => {
                    para.clear();
                    heading = None;
                    list = false;
                }
                b"numPr" => list = true,
                b"tbl" => table_depth += 1,
                b"tc" if table_depth == 1 => cell.clear(),
                b"tr" if table_depth == 1 => row.clear(),
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                b"tab" => para.push('\t'),
                b"br" | b"cr" => para.push('\n'),
                b"pStyle" => heading = local_attr(&e, b"val").as_deref().and_then(heading_level),
                b"docPr" => alt = local_attr(&e, b"descr").filter(|d| !d.trim().is_empty()).or_else(|| local_attr(&e, b"title")),
                b"blip" => {
                    let bytes = local_attr(&e, b"embed").and_then(|id| media.get(&id).cloned()).and_then(|p| zip_bytes(&mut archive, &p));
                    if let Some(bytes) = bytes {
                        images.push(ExtractedImage { bytes, alt: useful_alt(alt.take()), source: None });
                        pending_images.push(images.len() - 1);
                    }
                }
                _ => {}
            },
            Event::Text(t) if in_text => para.push_str(&t.unescape()?),
            Event::End(e) => match e.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" => {
                    let text = para.trim();
                    if table_depth > 0 {
                        if !text.is_empty() {
                            if !cell.is_empty() {
                                cell.push(' ');
                            }
                            cell.push_str(&text.replace('\n', " "));
                        }
                    } else if !text.is_empty() {
                        match (heading, list) {
                            (Some(level), _) => out.push_str(&format!("{} {}\n\n", "#".repeat(level), text)),
                            (None, true) => out.push_str(&format!("- {text}\n")),
                            (None, false) => out.push_str(&format!("{text}\n\n")),
                        }
                    }
                    // Images of the paragraph come right after it (after the table, inside one).
                    if table_depth == 0 {
                        for n in pending_images.drain(..) {
                            out.push_str(&format!("\n{IMAGE_MARKER}{n} -->\n\n"));
                        }
                    }
                    para.clear();
                }
                b"tc" if table_depth == 1 => row.push(cell.replace('|', "/")),
                b"tr" if table_depth == 1 => {
                    if row.iter().any(|c| !c.is_empty()) {
                        out.push_str(&format!("| {} |\n", row.join(" | ")));
                    }
                }
                b"tbl" => {
                    table_depth = table_depth.saturating_sub(1);
                    if table_depth == 0 {
                        out.push('\n');
                        for n in pending_images.drain(..) {
                            out.push_str(&format!("\n{IMAGE_MARKER}{n} -->\n\n"));
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    for n in pending_images.drain(..) {
        out.push_str(&format!("\n{IMAGE_MARKER}{n} -->\n\n"));
    }
    Ok(Extracted { text: out, title, images, ..Default::default() })
}

/// Images of a slide, in order: (relationship id, alt text).
fn slide_images(xml: &str) -> Vec<(String, Option<String>)> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut alt = None;
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"cNvPr" => alt = local_attr(&e, b"descr").filter(|d| !d.trim().is_empty()).or_else(|| local_attr(&e, b"title")),
                b"blip" => {
                    if let Some(id) = local_attr(&e, b"embed") {
                        out.push((id, alt.take()));
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Resolves `../notesSlides/x.xml` against `ppt/slides/`.
fn resolve(base_dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = base_dir.split('/').filter(|p| !p.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn relationships(archive: &mut zip::ZipArchive<Cursor<&[u8]>>, rels_path: &str) -> Vec<(String, String, String)> {
    let Some(xml) = zip_text(archive, rels_path) else { return Vec::new() };
    let mut reader = Reader::from_str(&xml);
    let mut out = Vec::new();
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"Relationship" => {
                if let (Some(id), Some(target)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                    out.push((id, target, attr(&e, b"Type").unwrap_or_default()));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Slide paths in presentation order.
fn slide_paths(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Vec<String> {
    let rels: HashMap<String, String> = relationships(archive, "ppt/_rels/presentation.xml.rels")
        .into_iter()
        .map(|(id, target, _)| (id, resolve("ppt", &target)))
        .collect();
    let mut ordered = Vec::new();
    if let Some(xml) = zip_text(archive, "ppt/presentation.xml") {
        let mut reader = Reader::from_str(&xml);
        while let Ok(event) = reader.read_event() {
            match event {
                Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"sldId" => {
                    if let Some(path) = attr(&e, b"r:id").and_then(|id| rels.get(&id).cloned()) {
                        ordered.push(path);
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
    }
    if ordered.is_empty() {
        // Fallback: slideN.xml sorted by N.
        let mut names: Vec<(usize, String)> = archive
            .file_names()
            .filter_map(|n| {
                let num = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok()?;
                Some((num, n.to_string()))
            })
            .collect();
        names.sort();
        ordered = names.into_iter().map(|(_, n)| n).collect();
    }
    ordered
}

/// Text of a slide or notes part: (title, paragraphs).
fn slide_text(xml: &str, notes: bool) -> Result<(Option<String>, Vec<String>)> {
    let mut reader = Reader::from_str(xml);
    let mut title = None;
    let mut paragraphs = Vec::new();
    let mut shape_kind = String::new();
    let mut shape_paras: Vec<String> = Vec::new();
    let mut para = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"sp" | b"graphicFrame" => {
                    shape_kind.clear();
                    shape_paras.clear();
                }
                b"p" => para.clear(),
                b"t" => in_text = true,
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                b"ph" => shape_kind = local_attr(&e, b"type").unwrap_or_else(|| "body".into()),
                b"br" => para.push('\n'),
                _ => {}
            },
            Event::Text(t) if in_text => para.push_str(&t.unescape()?),
            Event::End(e) => match e.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" => {
                    let text = para.trim();
                    if !text.is_empty() {
                        shape_paras.push(text.to_string());
                    }
                    para.clear();
                }
                b"sp" | b"graphicFrame" => {
                    let skip = matches!(shape_kind.as_str(), "sldNum" | "sldImg" | "dt" | "ftr" | "hdr");
                    if matches!(shape_kind.as_str(), "title" | "ctrTitle") && !notes && title.is_none() {
                        title = Some(shape_paras.join(" "));
                    } else if !skip {
                        paragraphs.append(&mut shape_paras);
                    }
                    shape_paras.clear();
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((title, paragraphs))
}

pub fn pptx(bytes: &[u8]) -> Result<Extracted> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    let title = core_title(&mut archive);
    let slides = slide_paths(&mut archive);
    let mut out = String::new();
    let mut images: Vec<ExtractedImage> = Vec::new();
    for (n, path) in slides.iter().enumerate() {
        let Some(xml) = zip_text(&mut archive, path) else { continue };
        let (slide_title, paragraphs) = slide_text(&xml, false)?;
        match &slide_title {
            Some(t) => out.push_str(&format!("## Diapositive {} : {}\n\n", n + 1, t)),
            None => out.push_str(&format!("## Diapositive {}\n\n", n + 1)),
        }
        for p in &paragraphs {
            out.push_str(p);
            out.push_str("\n\n");
        }
        // Pictures and speaker notes, through the slide's relationships.
        let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
        let rels = relationships(&mut archive, &format!("{dir}/_rels/{file}.rels"));
        let media: HashMap<String, String> = rels
            .iter()
            .filter(|(_, _, kind)| kind.ends_with("/image"))
            .map(|(id, target, _)| (id.clone(), resolve(dir, target)))
            .collect();
        for (id, alt) in slide_images(&xml) {
            if let Some(bytes) = media.get(&id).and_then(|p| zip_bytes(&mut archive, p)) {
                images.push(ExtractedImage { bytes, alt: useful_alt(alt), source: None });
                out.push_str(&format!("{IMAGE_MARKER}{} -->\n\n", images.len() - 1));
            }
        }
        let notes = rels
            .into_iter()
            .find(|(_, _, kind)| kind.ends_with("/notesSlide"))
            .map(|(_, target, _)| resolve(dir, &target));
        if let Some(notes_xml) = notes.and_then(|p| zip_text(&mut archive, &p)) {
            let (_, lines) = slide_text(&notes_xml, true)?;
            if !lines.is_empty() {
                out.push_str(&format!("Notes : {}\n\n", lines.join(" ")));
            }
        }
    }
    Ok(Extracted { text: out, title, images, pages: Some(slides.len()), ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, content) in files {
            w.start_file(*name, opts).unwrap();
            w.write_all(content.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn front_matter_is_read_and_removed() {
        let fm = front_matter("---\ntitle: \"Guide RH\"\ntags: [rh, paie]\nstatus: draft\n---\n# Intro\nTexte");
        assert_eq!(fm.title.as_deref(), Some("Guide RH"));
        assert_eq!(fm.tags, vec!["rh", "paie"]);
        assert_eq!(fm.status.as_deref(), Some("DRAFT"));
        assert_eq!(fm.body, "# Intro\nTexte");

        let block = front_matter("---\ntags:\n  - a\n  - 'b c'\n---\nx");
        assert_eq!(block.tags, vec!["a", "b c"]);
        assert_eq!(front_matter("pas de front matter").body, "pas de front matter");
    }

    #[test]
    fn plain_text_hashes_are_not_headings() {
        let (_, e) = extract("notes.txt", "Configuration\n# commentaire de code\nsuite".as_bytes()).unwrap();
        assert!(!e.structured);
        assert_eq!(e.title, None);
    }

    #[test]
    fn web_page_keeps_main_content() {
        let page = r#"<html><head><title>Guide &amp; FAQ | Example</title>
            <meta property="og:title" content="Install guide"></head><body>
            <nav><a href="/">Home</a> <a href="/blog">Blog</a></nav>
            <main><h1>Installing</h1><p>Run the installer, then restart the machine so the service starts.</p>
            <aside>Related posts</aside><p>The service listens on port 8080 once it has started properly.</p>
            <form><input name="q"><button>Search</button></form></main>
            <div class="navbox"><div>Related <b>topics</b></div> Graph, Tree</div>
            <footer>© Example 2026</footer></body></html>"#;
        assert_eq!(html_title(page).as_deref(), Some("Install guide"));
        let text = html_to_markdown(main_region(page));
        assert!(text.contains("# Installing") && text.contains("port 8080"), "{text}");
        assert!(!text.contains("Home") && !text.contains("Related") && !text.contains("Search") && !text.contains("2026"), "{text}");
        assert!(!text.contains("Tree"), "{text}");
        assert!(is_furniture(r#"div id="catlinks" class="catlinks""#) && is_furniture(r#"ul role="navigation""#));
        assert!(!is_furniture(r#"div class="mw-content-text""#) && !is_furniture(r#"div class="tocolor""#));
        assert_eq!(html_title("<title>Guide &amp; FAQ</title>").as_deref(), Some("Guide & FAQ"));
        let mut images = Vec::new();
        let md = html_markdown(r#"<p>Intro</p><figure><img src="/a.png" width="300"><figcaption>A <b>property</b> graph</figcaption></figure><img src="x.gif" width="1">"#, Some(&mut images));
        assert_eq!(images.len(), 1, "{md}");
        assert_eq!(images[0].alt.as_deref(), Some("A property graph"));
        assert_eq!(images[0].source.as_deref(), Some("/a.png"));
    }

    #[test]
    fn office_alt_texts() {
        assert_eq!(useful_alt(Some("Une image contenant texte, capture d’écran\n\nLe contenu généré par l’IA peut être incorrect.".into())), None);
        assert_eq!(useful_alt(Some("Picture 3".into())), None);
        assert_eq!(useful_alt(Some("image1.png".into())), None);
        assert_eq!(useful_alt(Some("Flux d'ingestion du MRP".into())).as_deref(), Some("Flux d'ingestion du MRP"));
    }

    #[test]
    fn generic_headings_are_not_titles() {
        assert!(is_generic_heading("Introduction"));
        assert!(is_generic_heading("1. Introduction"));
        assert!(is_generic_heading("Table of Contents"));
        assert!(!is_generic_heading("Global Capacity Management Tool"));
    }

    #[test]
    fn markdown_title_from_heading() {
        let (format, e) = extract("guide.md", "Intro\n\n# Le guide\n\nTexte.".as_bytes()).unwrap();
        assert_eq!(format, Format::Markdown);
        assert_eq!(e.title.as_deref(), Some("Le guide"));
    }

    #[test]
    fn docx_headings_lists_tables() {
        let doc = r#"<?xml version="1.0"?><w:document xmlns:w="w"><w:body>
<w:p><w:pPr><w:pStyle w:val="Titre1"/></w:pPr><w:r><w:t>Congés</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Chaque salarié &amp; </w:t></w:r><w:r><w:t>stagiaire.</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/></w:numPr></w:pPr><w:r><w:t>Point un</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Type</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Jours</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>Payés</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>25</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
</w:body></w:document>"#;
        let core = r#"<cp:coreProperties xmlns:cp="c" xmlns:dc="d"><dc:title>Règlement</dc:title></cp:coreProperties>"#;
        let bytes = zip_of(&[("word/document.xml", doc), ("docProps/core.xml", core)]);
        let (format, e) = extract("r.docx", &bytes).unwrap();
        assert_eq!(format, Format::Docx);
        assert_eq!(e.title.as_deref(), Some("Règlement"));
        assert!(e.text.contains("# Congés\n"), "{}", e.text);
        assert!(e.text.contains("Chaque salarié & stagiaire."), "{}", e.text);
        assert!(e.text.contains("- Point un"), "{}", e.text);
        assert!(e.text.contains("| Payés | 25 |"), "{}", e.text);
    }

    #[test]
    fn pptx_slides_in_order_with_notes() {
        let slide = |title: &str, body: &str| {
            format!(
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{title}</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{body}</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#
            )
        };
        let pres = r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst></p:presentation>"#;
        let rels = r#"<Relationships><Relationship Id="rId2" Type="x/slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="x/slide" Target="slides/slide2.xml"/></Relationships>"#;
        let slide_rels = r#"<Relationships><Relationship Id="rId1" Type="http://x/notesSlide" Target="../notesSlides/notesSlide1.xml"/></Relationships>"#;
        let notes = r#"<p:notes xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:nvPr><p:ph type="body"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Dire bonjour</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#;
        let s1 = slide("Bilan", "Chiffres 2026");
        let s2 = slide("Ouverture", "Bienvenue");
        let bytes = zip_of(&[
            ("ppt/presentation.xml", pres),
            ("ppt/_rels/presentation.xml.rels", rels),
            ("ppt/slides/slide1.xml", &s1),
            ("ppt/slides/slide2.xml", &s2),
            ("ppt/slides/_rels/slide2.xml.rels", slide_rels),
            ("ppt/notesSlides/notesSlide1.xml", notes),
        ]);
        let (format, e) = extract("deck.pptx", &bytes).unwrap();
        assert_eq!(format, Format::Pptx);
        assert_eq!(e.pages, Some(2));
        let first = e.text.find("Diapositive 1 : Ouverture").expect(&e.text);
        let second = e.text.find("Diapositive 2 : Bilan").expect(&e.text);
        assert!(first < second);
        assert!(e.text.contains("Notes : Dire bonjour"), "{}", e.text);
    }

    #[test]
    fn html_becomes_markdown() {
        let html = "<html><head><title>x</title><style>p{}</style></head><body><h1>Politique</h1><p>Le RSSI &amp; l&#39;équipe.</p><script>alert(1)</script><ul><li>Un</li><li>Deux</li></ul><table><tr><td>A</td><td>B</td></tr></table></body></html>";
        let md = html_to_markdown(html);
        assert!(md.starts_with("# Politique\n"), "{md}");
        assert!(md.contains("Le RSSI & l'équipe."), "{md}");
        assert!(md.contains("- Un\n- Deux"), "{md}");
        assert!(md.contains("| A | B |"), "{md}");
        assert!(!md.contains("alert") && !md.contains("p{}"), "{md}");
    }

    #[test]
    fn pdf_layout_to_markdown() {
        let xml = r##"<pdf2xml>
<page number="1" height="1000" width="700">
<fontspec id="0" size="12" family="Helvetica" color="#000"/>
<fontspec id="1" size="24" family="Helvetica-Bold" color="#000"/>
<fontspec id="2" size="11" family="Courier" color="#000"/>
<fontspec id="3" size="10" family="Helvetica" color="#000"/>
<text top="20" left="300" width="200" height="12" font="3">CHAPTER 1: Intro</text>
<text top="100" left="80" width="300" height="29" font="1">Getting Started</text>
<text top="150" left="80" width="500" height="17" font="0">Android apps are built from activi-</text>
<text top="168" left="80" width="300" height="17" font="0">ties and the </text>
<text top="168" left="380" width="60" height="17" font="2">Intent</text>
<text top="168" left="440" width="100" height="17" font="0"> class.</text>
<text top="220" left="80" width="300" height="15" font="2">public class A {</text>
<text top="236" left="94" width="300" height="15" font="2">int x;</text>
<text top="252" left="80" width="300" height="15" font="2">}</text>
<text top="300" left="80" width="300" height="17" font="0">• first point</text>
<text top="960" left="340" width="20" height="12" font="3">1</text>
</page>
<page number="2" height="1000" width="700">
<text top="20" left="300" width="200" height="12" font="3">CHAPTER 1: Intro</text>
<text top="100" left="80" width="300" height="29" font="1">Next Steps</text>
<text top="150" left="80" width="300" height="17" font="0">More text here.</text>
<text top="960" left="340" width="20" height="12" font="3">2</text>
</page>
<page number="3" height="1000" width="700">
<text top="20" left="300" width="200" height="12" font="3">CHAPTER 1: Intro</text>
<text top="150" left="80" width="300" height="17" font="0">Last page.</text>
</page>
</pdf2xml>"##;
        let md = pdf_xml_to_markdown(xml).unwrap();
        assert!(md.contains("# Getting Started\n"), "{md}");
        assert!(md.contains("Android apps are built from activities and the `Intent` class."), "{md}");
        assert!(md.contains("```\npublic class A {\n  int x;\n}\n```"), "{md}");
        assert!(md.contains("- first point"), "{md}");
        assert!(!md.contains("CHAPTER 1"), "running header kept: {md}");
        assert!(md.contains("<!-- page 2 -->"), "{md}");
        assert!(md.contains("# Next Steps"), "{md}");
    }

    #[test]
    fn toc_leaders_are_collapsed() {
        assert_eq!(collapse_leaders("Fragment\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd} 133"), "Fragment … 133");
        assert_eq!(collapse_leaders("Intents . . . . . . . 45"), "Intents … 45");
        assert_eq!(collapse_leaders("Voir p. 3... fin"), "Voir p. 3... fin");
    }

    #[test]
    fn hyphenation_is_joined() {
        assert_eq!(join_hyphenated("infor-\nmation et Jean-\nPierre"), "information et Jean-\nPierre\n");
    }

    #[test]
    fn unknown_binary_is_rejected() {
        assert!(extract("x.bin", &[0, 159, 146, 150]).is_err());
    }
}
