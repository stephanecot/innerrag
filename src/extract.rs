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

#[derive(Debug, Default)]
pub struct Extracted {
    pub text: String,
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
        Format::Html => Extracted { text: html_to_markdown(&String::from_utf8_lossy(bytes)), ..Default::default() },
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
pub fn html_to_markdown(html: &str) -> String {
    let lower = html.to_lowercase();
    let mut out = String::new();
    let mut i = 0;
    let mut skip_until: Option<&str> = None;
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
            out.push_str(&html[i..]);
            break;
        };
        out.push_str(&html[i..i + lt]);
        i += lt;
        let Some(gt) = html[i..].find('>') else { break };
        let tag = lower[i + 1..i + gt].trim();
        i += gt + 1;
        let closing = tag.starts_with('/');
        let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        match name.as_str() {
            "script" | "style" | "head" | "noscript" | "svg" if !closing => {
                skip_until = Some(match name.as_str() {
                    "script" => "</script>",
                    "style" => "</style>",
                    "head" => "</head>",
                    "noscript" => "</noscript>",
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
}

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
fn pdf_runs(xml: &str) -> Result<(Vec<(usize, f32, Vec<Run>)>, HashMap<usize, Font>)> {
    let mut reader = Reader::from_str(xml);
    let mut fonts = HashMap::new();
    let mut pages: Vec<(usize, f32, Vec<Run>)> = Vec::new();
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
                pages.push((num(&e, b"number") as usize, num(&e, b"height"), Vec::new()));
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
pub fn pdf_xml_to_markdown(xml: &str) -> Result<String> {
    let (pages, fonts) = pdf_runs(xml)?;
    let font = |id: usize| fonts.get(&id).map_or((0.0, false, false), |f| (f.size, f.mono, f.bold));

    // 1. Lines: runs sharing a baseline.
    let mut lines: Vec<Line> = Vec::new();
    let mut page_heights = HashMap::new();
    for (page, height, runs) in &pages {
        page_heights.insert(*page, *height);
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
    }
    if lines.is_empty() {
        return Ok(String::new());
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
    Ok(fixed)
}

fn pdf(bytes: &[u8]) -> Result<Extracted> {
    let title = run_tool("pdfinfo", &[], bytes, "pdf", &[]).ok().and_then(|info| {
        info.lines()
            .find_map(|l| l.strip_prefix("Title:"))
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    });
    // Structured conversion first; plain text when the layout cannot be read.
    if let Ok(xml) = run_tool("pdftohtml", &["-xml", "-i", "-q", "-stdout"], bytes, "pdf", &[]) {
        let pages = xml.matches("<page ").count();
        match pdf_xml_to_markdown(&xml) {
            Ok(md) if md.split_whitespace().count() > pages.max(1) * 5 => {
                return Ok(Extracted { text: md, title, pages: Some(pages), structured: true, ..Default::default() });
            }
            Ok(_) => tracing::info!("PDF layout gave little text, falling back to pdftotext"),
            Err(e) => tracing::warn!("PDF layout conversion failed ({e:#}), falling back to pdftotext"),
        }
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
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(Extracted { text: out, title, ..Default::default() })
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
        // Speaker notes, through the slide's relationships.
        let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
        let notes = relationships(&mut archive, &format!("{dir}/_rels/{file}.rels"))
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
    Ok(Extracted { text: out, title, pages: Some(slides.len()), ..Default::default() })
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
