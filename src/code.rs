//! Bridge between the documentation and a code repository.
//!
//! The code elements a document names between backticks (`HttpClient`, `src/api.rs`,
//! `GET /api/health`, `INNERRAG_DATA`, `--allow-writes`) are checked against an index of the
//! project's code folder: does the code still have them? That tells which passages describe code
//! that no longer exists (drift), and which passages an agent should read before changing a file
//! or a symbol (`docs_for`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use regex::Regex;
use serde::Serialize;

use crate::db::{as_i64, as_str, rows, Graph};
use crate::{AppState, Invalid};

const CODE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "java", "kt", "kts", "go", "cs", "rb", "php", "swift", "c", "h",
    "cc", "cpp", "hpp", "scala", "sql", "sh", "ps1", "toml", "yml", "yaml", "json", "gradle", "xml", "vue", "svelte",
    "dockerfile",
];
const SKIP_DIRS: &[&str] = &[
    ".git", "node_modules", "target", "dist", "build", "out", "vendor", ".venv", "venv", "__pycache__", ".next", ".idea",
    ".gradle", "coverage",
];
const MAX_FILES: usize = 30_000;
const MAX_FILE_BYTES: u64 = 1_000_000;
const INDEX_TTL: Duration = Duration::from_secs(60);

/// What the code contains, enough to tell whether a documented element still exists.
#[derive(Default)]
pub struct CodeIndex {
    pub root: PathBuf,
    pub files: Vec<String>,
    identifiers: HashSet<String>,
    flags: HashSet<String>,
    /// String literals that look like routes ("/api/...").
    routes: HashSet<String>,
    /// Where names are defined: name → "file:line".
    definitions: HashMap<String, Vec<String>>,
    pub built: Option<Instant>,
}

fn definition_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // Rust, Go, Python, JS/TS, Kotlin, Swift, Scala…
            r"\b(?:fn|func|def|function|class|struct|enum|trait|interface|type|object|module|record|fun|const|static|macro_rules!)\s+([A-Za-z_][A-Za-z0-9_]{2,})",
            // Java / C# / C++ methods: `public static Foo bar(`
            r"\b(?:public|private|protected|internal|static|final|override|virtual|async)\s+(?:[\w<>\[\],.?]+\s+)+([A-Za-z_][A-Za-z0-9_]*)\s*\(",
            // JS/TS: `name = (` / `name: (` arrow functions and object methods
            r"\b([A-Za-z_][A-Za-z0-9_]*)\s*[:=]\s*(?:async\s*)?\([^)]*\)\s*=>",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("valid regex"))
        .collect()
    })
}

fn identifier_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_]{2,}").expect("valid regex"))
}

fn is_code_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_lowercase();
    if name == "dockerfile" || name == "makefile" {
        return true;
    }
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| CODE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        match entry.file_type() {
            Ok(t) if t.is_dir() => {
                if !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                    walk(&path, out);
                }
            }
            Ok(t) if t.is_file() && is_code_file(&path) => out.push(path),
            _ => {}
        }
    }
}

impl CodeIndex {
    pub fn build(root: &Path) -> Result<Self> {
        if !root.is_dir() {
            return Err(Invalid(format!("code folder {} does not exist", root.display())).into());
        }
        let mut paths = Vec::new();
        walk(root, &mut paths);
        let mut index = CodeIndex { root: root.to_path_buf(), built: Some(Instant::now()), ..Default::default() };
        let flag_re = Regex::new(r"--[a-z][a-z0-9-]{1,40}").expect("valid regex");
        let route_re = Regex::new(r#"["'`](/[A-Za-z0-9_\-./{}:*]{2,120})["'`]"#).expect("valid regex");
        for path in paths {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_FILE_BYTES) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for m in identifier_re().find_iter(&text) {
                index.identifiers.insert(m.as_str().to_string());
            }
            for m in flag_re.find_iter(&text) {
                index.flags.insert(m.as_str().to_string());
            }
            for c in route_re.captures_iter(&text) {
                index.routes.insert(c[1].to_string());
            }
            for (n, line) in text.lines().enumerate() {
                for re in definition_patterns() {
                    for c in re.captures_iter(line) {
                        let defs = index.definitions.entry(c[1].to_string()).or_default();
                        if defs.len() < 5 {
                            defs.push(format!("{rel}:{}", n + 1));
                        }
                    }
                }
            }
            index.files.push(rel);
        }
        index.files.sort();
        Ok(index)
    }

    fn has_file(&self, wanted: &str) -> Vec<&String> {
        let wanted = wanted.trim_start_matches("./").trim_start_matches('/');
        self.files.iter().filter(|f| *f == wanted || f.ends_with(&format!("/{wanted}"))).collect()
    }

    fn has_route(&self, route: &str) -> bool {
        // Compare up to the first parameter: "/api/projects/{p}/search" → "/api/projects/".
        let stem = route.split(['{', ':', '?']).next().unwrap_or(route).trim_end_matches('/');
        !stem.is_empty() && self.routes.iter().any(|r| r.starts_with(stem) || stem.starts_with(r.trim_end_matches('/')) && r.len() > 4)
    }
}

/// Code index of each project's folder, rebuilt when older than a minute.
fn indexes() -> &'static Mutex<HashMap<PathBuf, Arc<CodeIndex>>> {
    static INDEXES: OnceLock<Mutex<HashMap<PathBuf, Arc<CodeIndex>>>> = OnceLock::new();
    INDEXES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn code_root(state: &AppState, project: &str) -> Result<PathBuf> {
    let meta = state.projects.info(project)?.meta;
    let dir = meta.code_dir.filter(|d| !d.trim().is_empty()).ok_or_else(|| {
        Invalid(format!("project `{project}` has no code folder: set one (a folder under {}) in the Code page or with PATCH code_dir", state.config.watch_root.display()))
    })?;
    crate::watch::resolve(&state.config.watch_root, &dir)
}

pub fn index_for(state: &AppState, project: &str) -> Result<Arc<CodeIndex>> {
    let root = code_root(state, project)?;
    let mut map = indexes().lock().map_err(|_| anyhow::anyhow!("code index lock poisoned"))?;
    if let Some(ix) = map.get(&root).filter(|ix| ix.built.is_some_and(|b| b.elapsed() < INDEX_TTL)) {
        return Ok(ix.clone());
    }
    let ix = Arc::new(CodeIndex::build(&root).with_context(|| format!("indexing {}", root.display()))?);
    map.insert(root, ix.clone());
    Ok(ix)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Path,
    Route,
    EnvVar,
    Flag,
    Symbol,
}

/// A code element named in a passage, and whether the code still has it.
#[derive(Debug, Clone, Serialize)]
pub struct Mention {
    pub chunk_id: String,
    pub doc_id: String,
    pub doc_title: String,
    pub page: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocSymbol {
    pub symbol: String,
    pub kind: Kind,
    pub found: bool,
    /// Where the code defines it (file:line) or the matching files.
    pub locations: Vec<String>,
    pub mentions: Vec<Mention>,
}

fn backticks() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`([^`\n]{2,100})`").expect("valid regex"))
}

/// The code element a backtick span names, if it looks like one.
pub fn classify(span: &str) -> Option<(String, Kind)> {
    let s = span.trim().trim_end_matches([';', ':', ',']).trim();
    // Placeholders and patterns (`<id>`, `files/*.jsonl`, `$HOME/...`) are not real elements.
    if s.contains(['<', '>', '*', '$', '|']) || s.starts_with('_') {
        return None;
    }
    static ROUTE: OnceLock<Regex> = OnceLock::new();
    let route = ROUTE.get_or_init(|| Regex::new(r"^(?:GET|POST|PUT|PATCH|DELETE)\s+(/\S*)$").expect("valid regex"));
    if let Some(c) = route.captures(s) {
        return Some((c[1].to_string(), Kind::Route));
    }
    if s.contains(char::is_whitespace) {
        return None; // a code snippet or a phrase, not a single element
    }
    if s.starts_with("--") && s.len() > 3 && s[2..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        let flag = s.split('=').next().unwrap_or(s);
        return Some((flag.to_string(), Kind::Flag));
    }
    if s.starts_with("/api") || (s.starts_with('/') && s.matches('/').count() >= 2 && !s.contains('.')) {
        return Some((s.to_string(), Kind::Route));
    }
    if s.contains('/') && !s.contains("://") {
        let path = s.trim_end_matches('/');
        let has_ext = Path::new(path).extension().is_some();
        if (has_ext || s.ends_with('/')) && (path.contains('/') || has_ext) {
            return Some((path.to_string(), Kind::Path));
        }
    }
    // A bare file name: `build.rs`, `Cargo.toml`.
    if !s.contains('/') && !s.contains(['(', ':']) {
        if let Some(ext) = Path::new(s).extension().and_then(|e| e.to_str()) {
            if CODE_EXTENSIONS.contains(&ext.to_lowercase().as_str()) || matches!(ext, "md" | "txt" | "lock" | "env") {
                return Some((s.to_string(), Kind::Path));
            }
        }
    }
    if s.len() >= 4 && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') && s.contains('_') {
        return Some((s.to_string(), Kind::EnvVar));
    }
    // Identifiers, calls and qualified names: `foo()`, `Bar::baz`, `a.b.C`.
    let name = s.trim_end_matches("()").split('(').next().unwrap_or(s);
    let ok = name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '$' | '#'))
        && name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && name.len() >= 3;
    // Plain words ("true", "Filters", "SUPERSEDES") are values or prose, not symbols worth checking:
    // a symbol has an inner capital, a separator, or call parentheses.
    let inner_capital = name.chars().skip(1).any(|c| c.is_uppercase()) && name.chars().any(|c| c.is_lowercase());
    let looks_like_code = name.contains(['_', '.', ':']) || inner_capital || s.ends_with(')');
    (ok && looks_like_code).then(|| (name.to_string(), Kind::Symbol))
}

fn last_segment(name: &str) -> &str {
    name.rsplit(['.', ':', '#']).find(|p| !p.is_empty()).unwrap_or(name)
}

fn check(ix: &CodeIndex, symbol: &str, kind: Kind) -> (bool, Vec<String>) {
    match kind {
        Kind::Path => {
            let files: Vec<String> = ix.has_file(symbol).into_iter().take(3).cloned().collect();
            (!files.is_empty(), files)
        }
        Kind::Route => (ix.has_route(symbol), Vec::new()),
        Kind::Flag => (ix.flags.contains(symbol), Vec::new()),
        Kind::EnvVar => (ix.identifiers.contains(symbol), Vec::new()),
        Kind::Symbol => {
            let name = last_segment(symbol);
            let defs = ix.definitions.get(name).cloned().unwrap_or_default();
            (ix.identifiers.contains(name), defs)
        }
    }
}

/// Every code element named in the project's published passages, checked against the code.
pub fn doc_symbols(state: &AppState, project: &str, graph: &Graph) -> Result<Vec<DocSymbol>> {
    let ix = index_for(state, project)?;
    let passages = rows(
        &graph.reader()?,
        "MATCH (d:Document)-[:HAS_CHUNK]->(c:Chunk) WHERE d.status = 'PUBLISHED' RETURN c.id, d.id, d.title, c.page, c.text",
        vec![],
    )?;
    let mut by_symbol: HashMap<(String, Kind), DocSymbol> = HashMap::new();
    for r in passages {
        let text = as_str(&r[4]);
        let mut seen = HashSet::new();
        for c in backticks().captures_iter(&text) {
            let Some((symbol, kind)) = classify(&c[1]) else { continue };
            if !seen.insert((symbol.clone(), kind)) {
                continue;
            }
            let entry = by_symbol.entry((symbol.clone(), kind)).or_insert_with(|| {
                let (found, locations) = check(&ix, &symbol, kind);
                DocSymbol { symbol: symbol.clone(), kind, found, locations, mentions: Vec::new() }
            });
            if entry.mentions.len() < 20 {
                entry.mentions.push(Mention {
                    chunk_id: as_str(&r[0]),
                    doc_id: as_str(&r[1]),
                    doc_title: as_str(&r[2]),
                    page: Some(as_i64(&r[3])).filter(|p| *p > 0),
                });
            }
        }
    }
    let mut out: Vec<DocSymbol> = by_symbol.into_values().collect();
    out.sort_by(|a, b| b.mentions.len().cmp(&a.mentions.len()).then(a.symbol.cmp(&b.symbol)));
    Ok(out)
}

#[derive(Debug, Serialize)]
pub struct Drift {
    pub code_dir: String,
    pub files: usize,
    pub symbols: usize,
    pub found: usize,
    /// Named in the documentation, absent from the code.
    pub missing: Vec<DocSymbol>,
    pub by_kind: Vec<(Kind, usize, usize)>,
}

pub fn drift(state: &AppState, project: &str, graph: &Graph) -> Result<Drift> {
    let ix = index_for(state, project)?;
    let symbols = doc_symbols(state, project, graph)?;
    let mut kinds: HashMap<Kind, (usize, usize)> = HashMap::new();
    for s in &symbols {
        let e = kinds.entry(s.kind).or_default();
        e.0 += 1;
        if s.found {
            e.1 += 1;
        }
    }
    let mut by_kind: Vec<(Kind, usize, usize)> = kinds.into_iter().map(|(k, (n, f))| (k, n, f)).collect();
    by_kind.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(Drift {
        code_dir: ix.root.display().to_string(),
        files: ix.files.len(),
        symbols: symbols.len(),
        found: symbols.iter().filter(|s| s.found).count(),
        missing: symbols.into_iter().filter(|s| !s.found).collect(),
        by_kind,
    })
}

#[derive(Debug, Serialize)]
pub struct DocsFor {
    pub target: String,
    /// Files of the code that match the target.
    pub files: Vec<String>,
    /// Code elements looked for in the documentation.
    pub looked_for: Vec<String>,
    pub symbols: Vec<DocSymbol>,
}

/// The documentation passages about a file or a symbol, to read before changing it.
pub fn docs_for(state: &AppState, project: &str, graph: &Graph, target: &str) -> Result<DocsFor> {
    let target = target.trim();
    if target.is_empty() {
        return Err(Invalid("give a file path or a symbol".into()).into());
    }
    let ix = index_for(state, project)?;
    let files: Vec<String> = if target.contains('/') || Path::new(target).extension().is_some() {
        ix.has_file(target).into_iter().cloned().collect()
    } else {
        Vec::new()
    };
    // A file stands for itself and the names it defines.
    let mut names: HashSet<String> = HashSet::new();
    for f in &files {
        names.insert(f.clone());
        if let Some(base) = Path::new(f).file_name().and_then(|n| n.to_str()) {
            names.insert(base.to_string());
        }
        for (name, defs) in &ix.definitions {
            if defs.iter().any(|d| d.rsplit_once(':').is_some_and(|(file, _)| file == f)) {
                names.insert(name.clone());
            }
        }
    }
    if files.is_empty() {
        names.insert(last_segment(target).to_string());
    }
    let symbols: Vec<DocSymbol> = doc_symbols(state, project, graph)?
        .into_iter()
        .filter(|s| match s.kind {
            Kind::Symbol => names.contains(&s.symbol) || names.contains(last_segment(&s.symbol)),
            Kind::Path => {
                let wanted = s.symbol.trim_start_matches("./");
                files.iter().any(|f| f == wanted || f.ends_with(&format!("/{wanted}")))
                    || (files.is_empty() && names.contains(wanted))
            }
            _ => names.contains(&s.symbol),
        })
        .collect();
    let mut looked_for: Vec<String> = names.into_iter().collect();
    looked_for.sort();
    Ok(DocsFor { target: target.to_string(), files, looked_for, symbols })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_backtick_spans() {
        assert_eq!(classify("GET /api/health"), Some(("/api/health".into(), Kind::Route)));
        assert_eq!(classify("src/api.rs"), Some(("src/api.rs".into(), Kind::Path)));
        assert_eq!(classify("INNERRAG_DATA"), Some(("INNERRAG_DATA".into(), Kind::EnvVar)));
        assert_eq!(classify("--allow-writes"), Some(("--allow-writes".into(), Kind::Flag)));
        assert_eq!(classify("HttpClient"), Some(("HttpClient".into(), Kind::Symbol)));
        assert_eq!(classify("search_knowledge"), Some(("search_knowledge".into(), Kind::Symbol)));
        assert_eq!(classify("cancel()"), Some(("cancel".into(), Kind::Symbol)));
        assert_eq!(classify("true"), None);
        assert_eq!(classify("Filters:"), None);
        assert_eq!(classify("SUPERSEDES"), None);
        assert_eq!(classify("<projet>/files"), None);
        assert_eq!(classify("build.rs"), Some(("build.rs".into(), Kind::Path)));
        assert_eq!(classify("files/"), None);
        assert_eq!(classify("cargo build --release"), None);
        assert_eq!(last_segment("crate::search::search"), "search");
    }
}
