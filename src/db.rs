//! LadybugDB access: database lifetime, schema, query helpers and value conversions.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use anyhow::{anyhow, Context, Result};
use lbug::{Connection, Database, LogicalType, SystemConfig, Value};
use serde_json::{json, Map, Value as Json};

pub type Row = Vec<Value>;

/// LadybugDB extensions: the vector index (k-NN). `fts` is only loaded to drop the full-text
/// indexes of earlier versions (they triggered data loss, see `keywords.rs`).
const EXTENSIONS: [&str; 2] = ["vector", "fts"];

pub struct Graph {
    db: Database,
    /// BM25 keyword index over the passages, in memory.
    keywords: std::sync::RwLock<crate::keywords::KeywordIndex>,
    /// The project directory (holds the database and the `files/` of originals).
    dir: std::path::PathBuf,
    write_lock: Mutex<()>,
    dim: usize,
    /// Bumped after every write: cached search results of an older version are stale.
    version: std::sync::atomic::AtomicU64,
    /// Recent search results and cross-encoder scores (see search.rs).
    pub cache: crate::cache::SearchCache,
}

/// The single writer of a database: LadybugDB allows one write transaction at a time.
pub struct Writer<'a> {
    // Declared first so the connection is dropped before the lock is released.
    conn: Connection<'a>,
    version: &'a std::sync::atomic::AtomicU64,
    _guard: MutexGuard<'a, ()>,
}

impl Drop for Writer<'_> {
    fn drop(&mut self) {
        self.version.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl<'a> std::ops::Deref for Writer<'a> {
    type Target = Connection<'a>;
    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

impl Graph {
    pub fn open(path: &Path, buffer_pool_mb: u64, threads: usize, dim: usize) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let config = SystemConfig::default()
            .buffer_pool_size(buffer_pool_mb * 1024 * 1024)
            .max_num_threads(threads as u64);
        let db = Database::new(path, config).with_context(|| format!("opening {}", path.display()))?;
        // Extensions are loaded per database; every later connection sees them.
        let conn = Connection::new(&db)?;
        for ext in EXTENSIONS {
            conn.query(&format!("LOAD {ext};")).with_context(|| {
                format!("loading the LadybugDB {ext} extension (run `innerrag install-extensions` once with network access)")
            })?;
        }
        drop(conn);
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let graph = Self {
            db,
            dir,
            keywords: Default::default(),
            write_lock: Mutex::new(()),
            dim,
            version: Default::default(),
            cache: Default::default(),
        };
        graph.init_schema()?;
        graph.repair()?;
        graph.checkpoint()?;
        graph.load_keywords()?;
        Ok(graph)
    }

    /// Downloads the extensions into `$HOME/.lbdb` so the server can later run offline.
    pub fn install_extensions() -> Result<()> {
        let db = Database::in_memory(SystemConfig::default().buffer_pool_size(64 * 1024 * 1024))?;
        let conn = Connection::new(&db)?;
        for ext in EXTENSIONS {
            conn.query(&format!("INSTALL {ext};"))?;
            conn.query(&format!("LOAD {ext};"))?;
            tracing::info!("extension {ext} installed");
        }
        Ok(())
    }

    fn init_schema(&self) -> Result<()> {
        let dim = self.dim;
        let conn = self.writer()?;
        let statements = [
            "CREATE NODE TABLE IF NOT EXISTS Document(id STRING PRIMARY KEY, title STRING, source STRING, metadata STRING, status STRING, creator STRING, tags STRING[], created_at TIMESTAMP, updated_at TIMESTAMP, content STRING)".to_string(),
            format!("CREATE NODE TABLE IF NOT EXISTS Chunk(id STRING PRIMARY KEY, doc_id STRING, idx INT64, text STRING, embedding FLOAT[{dim}], page INT64)"),
            format!("CREATE NODE TABLE IF NOT EXISTS Entity(id STRING PRIMARY KEY, name STRING, label STRING, embedding FLOAT[{dim}])"),
            "CREATE REL TABLE IF NOT EXISTS HAS_CHUNK(FROM Document TO Chunk)".to_string(),
            "CREATE REL TABLE IF NOT EXISTS NEXT(FROM Chunk TO Chunk)".to_string(),
            "CREATE REL TABLE IF NOT EXISTS MENTIONS(FROM Chunk TO Entity, score DOUBLE, surface STRING)".to_string(),
            "CREATE REL TABLE IF NOT EXISTS RELATED(FROM Entity TO Entity, weight INT64)".to_string(),
        ];
        for stmt in &statements {
            conn.query(stmt).with_context(|| format!("schema: {stmt}"))?;
        }
        // Columns added after the first release: bring older project databases up to date.
        for stmt in [
            "ALTER TABLE Document ADD content STRING DEFAULT ''",
            "ALTER TABLE Chunk ADD page INT64 DEFAULT 0",
        ] {
            if let Err(e) = conn.query(stmt) {
                if !e.to_string().contains("already") {
                    return Err(anyhow!("migrating schema ({stmt}): {e}"));
                }
            }
        }
        // Full-text indexes of an earlier version: their presence made commits lose Document rows.
        for index in ["chunk_fts_en", "chunk_fts_fr"] {
            if conn.query(&format!("CALL DROP_FTS_INDEX('Chunk', '{index}')")).is_ok() {
                tracing::info!(index, "removed the LadybugDB full-text index (replaced by the in-memory keyword index)");
            }
        }
        for (table, index) in [("Chunk", "chunk_vec"), ("Entity", "entity_vec")] {
            let stmt = format!("CALL CREATE_VECTOR_INDEX('{table}', '{index}', 'embedding', metric := 'cosine')");
            if let Err(e) = conn.query(&stmt) {
                if !e.to_string().contains("already exists") {
                    return Err(anyhow!("creating vector index {index}: {e}"));
                }
            }
        }
        Ok(())
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Cleans what the full-text index bug left behind: passages whose document vanished, and
    /// entities no passage mentions any more.
    fn repair(&self) -> Result<()> {
        let conn = self.writer()?;
        let orphans = rows(
            &conn,
            "MATCH (c:Chunk) WHERE NOT EXISTS { MATCH (d:Document) WHERE d.id = c.doc_id } RETURN count(c)",
            vec![],
        )?
        .first()
        .map_or(0, |r| as_i64(&r[0]));
        if orphans > 0 {
            tracing::warn!(orphans, dir = %self.dir.display(), "removing passages whose document was lost");
            exec(&conn, "MATCH (c:Chunk) WHERE NOT EXISTS { MATCH (d:Document) WHERE d.id = c.doc_id } DETACH DELETE c", vec![])?;
            exec(&conn, "MATCH (e:Entity) WHERE NOT EXISTS { MATCH (e)<-[:MENTIONS]-(:Chunk) } DETACH DELETE e", vec![])?;
        }
        Ok(())
    }

    fn load_keywords(&self) -> Result<()> {
        let conn = self.reader()?;
        let mut index = self.keywords.write().map_err(|_| anyhow!("keyword index lock poisoned"))?;
        for r in rows(&conn, "MATCH (c:Chunk) RETURN c.id, c.text", vec![])? {
            index.insert(&as_str(&r[0]), &as_str(&r[1]));
        }
        tracing::debug!(passages = index.len(), "keyword index built");
        Ok(())
    }

    /// Replaces a document's passages in the keyword index.
    pub fn index_keywords(&self, doc_id: &str, passages: &[(String, String)]) {
        self.version.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut index) = self.keywords.write() {
            index.remove_document(doc_id);
            for (id, text) in passages {
                index.insert(id, text);
            }
        }
    }

    pub fn search_keywords(&self, query: &str, top: usize) -> Vec<(String, f64)> {
        self.keywords.read().map(|i| i.search(query, top)).unwrap_or_default()
    }

    /// Where original files are kept: `<project>/files/`.
    pub fn files_dir(&self) -> std::path::PathBuf {
        self.dir.join("files")
    }

    pub fn writer(&self) -> Result<Writer<'_>> {
        let guard = self.write_lock.lock().map_err(|_| anyhow!("writer lock poisoned"))?;
        Ok(Writer { conn: Connection::new(&self.db)?, version: &self.version, _guard: guard })
    }

    /// Changes after every write; part of the search cache keys.
    pub fn version(&self) -> u64 {
        self.version.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A fresh connection for read queries, so reads do not wait behind ingestion.
    pub fn reader(&self) -> Result<Connection<'_>> {
        Ok(Connection::new(&self.db)?)
    }

    /// Folds the WAL into the database file, so the file alone is a consistent copy
    /// (safe to commit to git while the server runs).
    pub fn checkpoint(&self) -> Result<()> {
        let conn = self.writer()?;
        conn.query("CHECKPOINT;")?;
        Ok(())
    }
}

/// Runs a query (prepared when it has parameters) and collects all rows.
pub fn rows(conn: &Connection, query: &str, params: Vec<(&str, Value)>) -> Result<Vec<Row>> {
    let result = if params.is_empty() {
        conn.query(query)
    } else {
        let mut stmt = conn.prepare(query)?;
        conn.execute(&mut stmt, params)
    }
    .map_err(|e| anyhow!("{e}\n  in query: {}", query.trim()))?;
    Ok(result.collect())
}

pub fn exec(conn: &Connection, query: &str, params: Vec<(&str, Value)>) -> Result<()> {
    rows(conn, query, params).map(|_| ())
}

/// Runs `f` inside an explicit transaction, rolling back on error.
pub fn transaction<T>(conn: &Connection, body: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    conn.query("BEGIN TRANSACTION;")?;
    match body(conn) {
        Ok(v) => {
            conn.query("COMMIT;")?;
            Ok(v)
        }
        Err(e) => {
            if let Err(rb) = conn.query("ROLLBACK;") {
                tracing::error!("rollback failed: {rb}");
            }
            Err(e)
        }
    }
}

// ---- Value builders -------------------------------------------------------

pub fn s(v: impl Into<String>) -> Value {
    Value::String(v.into())
}

pub fn i(v: i64) -> Value {
    Value::Int64(v)
}

pub fn f(v: f64) -> Value {
    Value::Double(v)
}

pub fn floats(v: &[f32]) -> Value {
    Value::List(LogicalType::Float, v.iter().copied().map(Value::Float).collect())
}

pub fn strings<I: IntoIterator<Item = S>, S: Into<String>>(items: I) -> Value {
    Value::List(LogicalType::String, items.into_iter().map(|x| Value::String(x.into())).collect())
}

/// Builds a `LIST[STRUCT(...)]` parameter for `UNWIND $rows AS r` queries.
pub struct Structs {
    fields: Vec<(String, LogicalType)>,
    rows: Vec<Value>,
}

impl Structs {
    pub fn new(fields: &[(&str, LogicalType)]) -> Self {
        Self {
            fields: fields.iter().map(|(n, t)| ((*n).to_string(), t.clone())).collect(),
            rows: Vec::new(),
        }
    }

    pub fn push(&mut self, values: Vec<Value>) {
        debug_assert_eq!(values.len(), self.fields.len());
        let row = self.fields.iter().map(|(n, _)| n.clone()).zip(values).collect();
        self.rows.push(Value::Struct(row));
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn into_value(self) -> Value {
        Value::List(LogicalType::Struct { fields: self.fields }, self.rows)
    }
}

pub fn float_list_type() -> LogicalType {
    LogicalType::List { child_type: Box::new(LogicalType::Float) }
}

// ---- Value readers --------------------------------------------------------

pub fn as_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null(_) => String::new(),
        other => other.to_string(),
    }
}

pub fn as_i64(v: &Value) -> i64 {
    match v {
        Value::Int64(x) => *x,
        Value::Int32(x) => i64::from(*x),
        Value::Int16(x) => i64::from(*x),
        Value::Int8(x) => i64::from(*x),
        Value::UInt64(x) => i64::try_from(*x).unwrap_or(i64::MAX),
        Value::UInt32(x) => i64::from(*x),
        Value::UInt16(x) => i64::from(*x),
        Value::UInt8(x) => i64::from(*x),
        Value::Int128(x) => i64::try_from(*x).unwrap_or(i64::MAX),
        Value::Double(x) => *x as i64,
        _ => 0,
    }
}

pub fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Double(x) => *x,
        Value::Float(x) => f64::from(*x),
        other => as_i64(other) as f64,
    }
}

pub fn as_floats(v: &Value) -> Vec<f32> {
    match v {
        Value::List(_, items) | Value::Array(_, items) => items.iter().map(|x| as_f64(x) as f32).collect(),
        _ => Vec::new(),
    }
}

pub fn as_strings(v: &Value) -> Vec<String> {
    match v {
        Value::List(_, items) | Value::Array(_, items) => {
            items.iter().filter(|x| !matches!(x, Value::Null(_))).map(as_str).collect()
        }
        _ => Vec::new(),
    }
}

/// Generic conversion for the Cypher console.
pub fn to_json(v: &Value) -> Json {
    match v {
        Value::Null(_) => Json::Null,
        Value::Bool(b) => json!(b),
        Value::Int64(_) | Value::Int32(_) | Value::Int16(_) | Value::Int8(_) | Value::UInt32(_)
        | Value::UInt16(_) | Value::UInt8(_) => json!(as_i64(v)),
        Value::UInt64(x) => json!(x),
        Value::Int128(x) => i64::try_from(*x).map_or_else(|_| json!(x.to_string()), |v| json!(v)),
        Value::Double(x) => json!(x),
        Value::Float(x) => json!(x),
        Value::String(s) => json!(s),
        Value::Json(j) => j.clone(),
        Value::List(_, items) | Value::Array(_, items) => Json::Array(items.iter().map(to_json).collect()),
        Value::Struct(fields) => {
            Json::Object(fields.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
        }
        Value::Map(_, entries) => Json::Array(
            entries.iter().map(|(k, v)| json!({ "key": to_json(k), "value": to_json(v) })).collect(),
        ),
        Value::Node(node) => {
            let mut obj = Map::new();
            obj.insert("_label".into(), json!(node.get_label_name()));
            obj.insert("_id".into(), json!(node.get_node_id().to_string()));
            for (k, v) in node.get_properties() {
                obj.insert(k.clone(), to_json(v));
            }
            Json::Object(obj)
        }
        Value::Rel(rel) => {
            let mut obj = Map::new();
            obj.insert("_label".into(), json!(rel.get_label_name()));
            obj.insert("_src".into(), json!(rel.get_src_node().to_string()));
            obj.insert("_dst".into(), json!(rel.get_dst_node().to_string()));
            for (k, v) in rel.get_properties() {
                obj.insert(k.clone(), to_json(v));
            }
            Json::Object(obj)
        }
        Value::RecursiveRel { nodes, rels } => json!({
            "nodes": nodes.iter().map(|n| to_json(&Value::Node(n.clone()))).collect::<Vec<_>>(),
            "rels": rels.iter().map(|r| to_json(&Value::Rel(r.clone()))).collect::<Vec<_>>(),
        }),
        Value::Union { value, .. } => to_json(value),
        other => json!(other.to_string()),
    }
}

