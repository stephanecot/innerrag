//! Structure-aware chunking: markdown headings split the text into sections, each
//! section is cut into sentence-aligned chunks with overlap, and every chunk starts
//! with its heading path ("Guide › Installation") so it stays meaningful on its own.
//! Sizes are in characters.

/// Splits text into sentences, keeping the terminator with each sentence.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split("\n\n") {
        let mut current = String::new();
        let mut chars = paragraph.chars().peekable();
        while let Some(c) = chars.next() {
            current.push(if c == '\n' { ' ' } else { c });
            let terminator = matches!(c, '.' | '!' | '?' | '…' | ';');
            if terminator && chars.peek().is_none_or(|n| n.is_whitespace()) {
                push_trimmed(&mut out, &current);
                current.clear();
            }
        }
        push_trimmed(&mut out, &current);
    }
    out
}

fn push_trimmed(out: &mut Vec<String>, s: &str) {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if !collapsed.is_empty() {
        out.push(collapsed);
    }
}

/// Splits a sentence longer than `max` characters on word boundaries.
fn split_long(sentence: String, max: usize) -> Vec<String> {
    if sentence.chars().count() <= max {
        return vec![sentence];
    }
    let mut parts = Vec::new();
    let mut current = String::new();
    for word in sentence.split(' ') {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > max {
            parts.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

use crate::extract::PAGE_MARKER;

/// A chunk and the page it starts on (PDF documents).
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub text: String,
    pub page: Option<i64>,
}

fn page_marker(line: &str) -> Option<i64> {
    line.trim().strip_prefix(PAGE_MARKER)?.strip_suffix("-->")?.trim().parse().ok()
}

type Section = (Vec<String>, String, Option<i64>);

/// Splits markdown into (heading path, body, page) sections. `#` lines inside code fences are
/// not headings; `<!-- page N -->` markers start a new section on the same path.
fn sections(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut body = String::new();
    let mut in_fence = false;
    let mut page: Option<i64> = None;
    let flush = |path: &[(usize, String)], body: &mut String, page: Option<i64>, out: &mut Vec<Section>| {
        if !body.trim().is_empty() {
            out.push((path.iter().map(|(_, t)| t.clone()).collect(), std::mem::take(body), page));
        }
        body.clear();
    };
    for line in text.lines() {
        if !in_fence {
            if let Some(n) = page_marker(line) {
                flush(&path, &mut body, page, &mut out);
                page = Some(n);
                continue;
            }
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        let level = trimmed.chars().take_while(|&c| c == '#').count();
        let heading = (!in_fence && (1..=6).contains(&level))
            .then(|| trimmed[level..].strip_prefix(' '))
            .flatten()
            .map(|t| t.trim().trim_end_matches('#').trim())
            .filter(|t| !t.is_empty());
        if let Some(title) = heading {
            flush(&path, &mut body, page, &mut out);
            path.retain(|(l, _)| *l < level);
            path.push((level, title.to_string()));
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush(&path, &mut body, page, &mut out);
    out
}

/// Chunks a whole document, following its headings (and its pages, for PDFs).
pub fn chunk_document(text: &str, size: usize, overlap: usize) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    for (path, body, page) in sections(text) {
        let prefix = if path.is_empty() { String::new() } else { format!("{}\n", path.join(" › ")) };
        let room = size.saturating_sub(prefix.chars().count()).max(size / 2);
        for chunk in chunk_text(&body, room, overlap) {
            chunks.push(Chunk { text: format!("{prefix}{chunk}"), page });
        }
    }
    chunks
}

/// The text of a passage without its fenced code blocks: entity extraction on code is slow
/// and only finds noise (variable names tagged as people or places).
pub fn prose(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Plain text: no headings; form feeds (pdftotext) separate pages.
pub fn chunk_plain(text: &str, size: usize, overlap: usize) -> Vec<Chunk> {
    let pages: Vec<&str> = text.split('\u{c}').collect();
    let paged = pages.len() > 1;
    let mut chunks = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        for chunk in chunk_text(page, size, overlap) {
            chunks.push(Chunk { text: chunk, page: paged.then_some(i as i64 + 1) });
        }
    }
    chunks
}

pub fn chunk_text(text: &str, size: usize, overlap: usize) -> Vec<String> {
    let size = size.max(100);
    let overlap = overlap.min(size / 2);
    let units: Vec<String> = sentences(text)
        .into_iter()
        .flat_map(|s| split_long(s, size))
        .collect();

    let mut chunks = Vec::new();
    let mut window: Vec<&str> = Vec::new();
    let mut len = 0usize;
    for unit in &units {
        let unit_len = unit.chars().count();
        if !window.is_empty() && len + 1 + unit_len > size {
            chunks.push(window.join(" "));
            // Keep trailing sentences as overlap for the next chunk.
            let mut kept = Vec::new();
            let mut kept_len = 0;
            for s in window.iter().rev() {
                let l = s.chars().count();
                if kept_len + l > overlap {
                    break;
                }
                kept_len += l + 1;
                kept.push(*s);
            }
            kept.reverse();
            window = kept;
            len = kept_len;
        }
        len += unit_len + usize::from(!window.is_empty());
        window.push(unit);
    }
    if !window.is_empty() {
        chunks.push(window.join(" "));
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_chunk() {
        let chunks = chunk_text("Bonjour le monde. Ceci est un test !", 1000, 150);
        assert_eq!(chunks, vec!["Bonjour le monde. Ceci est un test !"]);
    }

    #[test]
    fn respects_size_and_overlaps() {
        let text = (0..40)
            .map(|i| format!("La phrase numéro {i} parle de Paris et de Lyon."))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = chunk_text(&text, 300, 100);
        assert!(chunks.len() > 3);
        for c in &chunks {
            assert!(c.chars().count() <= 300, "{}", c.len());
        }
        // The last sentence of a chunk is repeated in the next one.
        let last_sentence = chunks[0].rsplit(". ").next().unwrap();
        assert!(chunks[1].contains(last_sentence));
    }

    #[test]
    fn decimals_do_not_split_sentences() {
        assert_eq!(sentences("Le prix est 3.5 euros. Fin."), vec!["Le prix est 3.5 euros.", "Fin."]);
    }

    #[test]
    fn headings_split_sections_and_prefix_chunks() {
        let text = "# Guide\nIntro du guide.\n\n## Installation\nInstaller le paquet.\n\n```sh\n# pas un titre\nmake\n```\n\n## Usage\nLancer.\n\n# Annexe\nFin.";
        let chunks: Vec<String> = chunk_document(text, 1000, 100).into_iter().map(|c| c.text).collect();
        assert_eq!(chunks.len(), 4, "{chunks:#?}");
        assert_eq!(chunks[0], "Guide\nIntro du guide.");
        assert!(chunks[1].starts_with("Guide › Installation\nInstaller le paquet."), "{}", chunks[1]);
        assert!(chunks[1].contains("# pas un titre"));
        assert_eq!(chunks[2], "Guide › Usage\nLancer.");
        assert_eq!(chunks[3], "Annexe\nFin.");
    }

    #[test]
    fn plain_text_has_no_prefix() {
        assert_eq!(chunk_document("Juste une phrase.", 1000, 100), vec![Chunk { text: "Juste une phrase.".into(), page: None }]);
    }

    #[test]
    fn page_markers_give_pages_and_stay_out_of_text() {
        let text = "# Chapitre\nDébut.\n\n<!-- page 2 -->\n\nSuite page deux.\n";
        let chunks = chunk_document(text, 1000, 100);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], Chunk { text: "Chapitre\nDébut.".into(), page: None });
        assert_eq!(chunks[1], Chunk { text: "Chapitre\nSuite page deux.".into(), page: Some(2) });
        let plain = chunk_plain("Page un.\u{c}Page deux.", 1000, 0);
        assert_eq!(plain[1].page, Some(2));
    }

    #[test]
    fn long_sentence_is_split_on_words() {
        let text = "mot ".repeat(200);
        let chunks = chunk_text(&text, 120, 0);
        assert!(chunks.iter().all(|c| c.chars().count() <= 120));
        assert_eq!(chunks.join(" ").split(' ').count(), 200);
    }
}
