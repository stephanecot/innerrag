//! Ingesting a web page or a file from its URL: the server downloads it, then it goes through
//! the same extraction as an uploaded file (HTML pages keep their main content only).

use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::ingest::{self, FileFields, IngestRequest};
use crate::Invalid;

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: u32 = 5;

const MAX_PAGE_IMAGES: usize = 40;
const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// Downloads `url` (http or https) and returns a file name that tells its format, its bytes,
/// and the address it was finally served from (after redirects).
pub fn fetch(url: &str, max_bytes: u64) -> Result<(String, Vec<u8>, String)> {
    let parsed = url.trim();
    let lower = parsed.to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(Invalid(format!("only http and https addresses can be ingested, not `{parsed}`")).into());
    }
    let agent = ureq::AgentBuilder::new()
        .timeout(TIMEOUT)
        .redirects(MAX_REDIRECTS)
        .user_agent(concat!("innerrag/", env!("CARGO_PKG_VERSION"), " (knowledge base; +https://github.com/stephanecot/innerrag)"))
        .build();
    let response = match agent
        .get(parsed)
        .set("Accept", "text/html,application/xhtml+xml,application/pdf,text/markdown,text/plain;q=0.9,*/*;q=0.5")
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => {
            return Err(Invalid(format!("`{parsed}` answered HTTP {code}")).into());
        }
        Err(e) => return Err(Invalid(format!("cannot reach `{parsed}`: {e}")).into()),
    };
    let final_url = response.get_url().to_string();
    let content_type = response.content_type().to_lowercase();
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading `{parsed}`"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(Invalid(format!("`{parsed}` is larger than {} MB", max_bytes / 1024 / 1024)).into());
    }
    Ok((file_name(&final_url, &content_type), bytes, final_url))
}

/// An address of a page resource, resolved against the page's own address.
fn resolve_url(base: &str, src: &str) -> Option<String> {
    let src = src.trim();
    if src.starts_with("http://") || src.starts_with("https://") {
        return Some(src.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    if let Some(protocol_relative) = src.strip_prefix("//") {
        return Some(format!("{scheme}://{protocol_relative}"));
    }
    let host = rest.split('/').next()?;
    if src.starts_with('/') {
        return Some(format!("{scheme}://{host}{src}"));
    }
    if src.contains(':') {
        return None; // another scheme (javascript:, blob:…)
    }
    // Relative to the page's folder, with `./` and `../` applied.
    let path = rest[host.len()..].split(['?', '#']).next().unwrap_or("");
    let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if !path.ends_with('/') {
        parts.pop();
    }
    for seg in src.split(['?', '#']).next().unwrap_or(src).split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let query = src.find(['?', '#']).map(|q| &src[q..]).unwrap_or("");
    Some(format!("{scheme}://{host}/{}{query}", parts.join("/")))
}

/// Downloads the images of a web page (their bytes stay empty when it fails: they are left out).
fn fetch_images(images: &mut [crate::extract::ExtractedImage], page_url: &str) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .redirects(MAX_REDIRECTS)
        .user_agent(concat!("innerrag/", env!("CARGO_PKG_VERSION")))
        .build();
    let mut fetched = 0;
    for image in images.iter_mut().filter(|i| i.bytes.is_empty()) {
        if fetched >= MAX_PAGE_IMAGES {
            break;
        }
        let Some(url) = image.source.as_deref().and_then(|s| resolve_url(page_url, s)) else { continue };
        fetched += 1;
        let Ok(response) = agent.get(&url).set("Referer", page_url).call() else { continue };
        let mut bytes = Vec::new();
        if response.into_reader().take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes).is_ok() && (bytes.len() as u64) <= MAX_IMAGE_BYTES {
            image.bytes = bytes;
            image.source = Some(url);
        }
    }
}

/// A file name whose extension says the format: from the content type, else from the URL path.
fn file_name(url: &str, content_type: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let host = path.split("://").nth(1).unwrap_or(path).split('/').next().unwrap_or("page");
    // A page at the root is named after its host; otherwise after its last path segment, minus extension.
    let stem = if last.is_empty() || last == host { host } else { last.rsplit_once('.').map_or(last, |(s, _)| s) };
    let ext = match content_type {
        t if t.contains("pdf") => "pdf",
        t if t.contains("wordprocessingml") => "docx",
        t if t.contains("presentationml") => "pptx",
        t if t.contains("msword") => "doc",
        t if t.contains("ms-powerpoint") => "ppt",
        t if t.contains("markdown") => "md",
        t if t.contains("html") || t.contains("xhtml") => "html",
        t if t.contains("text/plain") => {
            if last.ends_with(".md") || last.ends_with(".markdown") { "md" } else { "txt" }
        }
        // Unknown or missing type: trust the URL's own extension, else assume a page.
        _ => match last.rsplit_once('.').map(|(_, e)| e.to_lowercase()) {
            Some(e) if crate::extract::SUPPORTED.split(", ").any(|s| s.trim_start_matches('.') == e) => return last.to_string(),
            _ => "html",
        },
    };
    let stem: String = stem.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '-' }).collect();
    format!("{}.{ext}", if stem.trim_matches('-').is_empty() { "page" } else { stem.trim_matches('-') })
}

/// A stable document id for a URL, so ingesting the same address again replaces it.
pub fn url_id(url: &str) -> String {
    let without_scheme = url.trim().split("://").nth(1).unwrap_or(url.trim());
    let without_fragment = without_scheme.split('#').next().unwrap_or(without_scheme).trim_end_matches('/');
    let cleaned: String = without_fragment
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "._/-".contains(c) { c } else { '-' })
        .collect();
    format!("url/{}", cleaned.trim_matches('-'))
}

/// Downloads a URL and turns it into an ingestion request (source: the URL).
pub fn url_request(url: &str, max_bytes: u64, mut fields: FileFields) -> Result<(IngestRequest, String)> {
    let (filename, bytes, final_url) = fetch(url, max_bytes)?;
    fields.id = fields.id.filter(|i| !i.trim().is_empty()).or_else(|| Some(url_id(url)));
    fields.source = Some(url.trim().to_string());
    let mut metadata = fields.metadata.take().filter(serde_json::Value::is_object).unwrap_or_else(|| serde_json::json!({}));
    let now = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
    metadata["url"] = serde_json::json!({ "address": url.trim(), "fetched_at": now });
    fields.metadata = Some(metadata);
    let mut req = ingest::file_request(&filename, bytes, fields)?;
    // A page's figures are downloaded too, from their address relative to the page.
    fetch_images(&mut req.images, &final_url);
    Ok((req, filename))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids() {
        assert_eq!(file_name("https://example.com/docs/guide", "text/html; charset=utf-8"), "guide.html");
        assert_eq!(file_name("https://example.com/", "text/html"), "example.com.html");
        assert_eq!(file_name("https://example.com/files/report.pdf?x=1", "application/octet-stream"), "report.pdf");
        assert_eq!(file_name("https://example.com/a/spec", "application/pdf"), "spec.pdf");
        assert_eq!(url_id("https://example.com/docs/guide/#intro"), "url/example.com/docs/guide");
        assert!(fetch("file:///etc/passwd", 10).is_err());
        let base = "https://example.com/docs/guide/page.html?x=1";
        assert_eq!(resolve_url(base, "img/a.png").as_deref(), Some("https://example.com/docs/guide/img/a.png"));
        assert_eq!(resolve_url(base, "../b.png").as_deref(), Some("https://example.com/docs/b.png"));
        assert_eq!(resolve_url(base, "/c.png").as_deref(), Some("https://example.com/c.png"));
        assert_eq!(resolve_url(base, "//cdn.example.org/d.png").as_deref(), Some("https://cdn.example.org/d.png"));
        assert_eq!(resolve_url(base, "javascript:alert(1)"), None);
    }
}
