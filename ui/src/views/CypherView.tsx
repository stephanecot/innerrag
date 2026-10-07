import { useMemo, useState } from "react";
import { api, type CypherResult } from "../api";

const EXAMPLES: { label: string; query: string }[] = [
  {
    label: "Entités les plus citées",
    query: "MATCH (e:Entity)<-[m:MENTIONS]-(:Chunk)\nRETURN e.name, e.label, count(m) AS mentions\nORDER BY mentions DESC\nLIMIT 20",
  },
  {
    label: "Relations les plus fortes",
    query: "MATCH (a:Entity)-[r:RELATED]->(b:Entity)\nRETURN a.name, b.name, r.weight\nORDER BY r.weight DESC\nLIMIT 20",
  },
  {
    label: "Documents d'un tag",
    query: "MATCH (d:Document)\nWHERE list_contains(d.tags, 'chimie')\nRETURN d.title, d.status, d.tags",
  },
  { label: "Tables du schéma", query: "CALL SHOW_TABLES() RETURN *" },
];

function Cell({ value }: { value: unknown }) {
  if (value === null || value === undefined) return <span className="muted">null</span>;
  if (typeof value === "object") return <span className="cell-json">{JSON.stringify(value, null, 1)}</span>;
  return <>{String(value)}</>;
}

export default function CypherView({ project }: { project: string }) {
  const p = useMemo(() => api.project(project), [project]);
  const [query, setQuery] = useState(EXAMPLES[1].query);
  const [result, setResult] = useState<CypherResult | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const run = async () => {
    if (!query.trim()) return;
    setBusy(true);
    setError("");
    const t = performance.now();
    try {
      setResult(await p.cypher(query));
      setElapsed(Math.round(performance.now() - t));
    } catch (e) {
      setResult(null);
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="page" aria-labelledby="cypher-title">
      <div className="page-head">
        <div>
          <h1 id="cypher-title">Console Cypher</h1>
          <p>Lecture seule. Les requêtes qui écrivent, chargent ou exportent des fichiers sont refusées.</p>
        </div>
      </div>
      <div className="toolbar">
        {EXAMPLES.map((ex) => (
          <button key={ex.label} type="button" className="pill" onClick={() => setQuery(ex.query)}>{ex.label}</button>
        ))}
      </div>
      <form
        style={{ display: "flex", flexDirection: "column", gap: 10 }}
        onSubmit={(e) => {
          e.preventDefault();
          run();
        }}
      >
        <label htmlFor="cypher" className="sr-only">Requête Cypher</label>
        <textarea
          id="cypher"
          className="cypher-editor"
          rows={6}
          spellCheck={false}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
              e.preventDefault();
              run();
            }
          }}
        />
        <div className="toolbar">
          <button type="submit" className="btn btn-primary" disabled={busy}>{busy ? "Exécution…" : "Exécuter"}</button>
          <span className="muted" style={{ fontSize: 14 }}>Ctrl + Entrée pour exécuter</span>
          {result && (
            <span className="muted" style={{ fontSize: 14, marginLeft: "auto" }}>
              {result.rows.length.toLocaleString("fr-FR")} lignes en {elapsed} ms{result.truncated ? " (tronqué à 1 000)" : ""}
            </span>
          )}
        </div>
      </form>
      {error && <div className="error-banner" role="alert">{error}</div>}
      {result && (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>{result.columns.map((c) => <th key={c} scope="col" className="mono">{c}</th>)}</tr>
            </thead>
            <tbody>
              {result.rows.map((row, i) => (
                <tr key={i}>{row.map((v, j) => <td key={j}><Cell value={v} /></td>)}</tr>
              ))}
            </tbody>
          </table>
          {result.rows.length === 0 && <div className="empty"><p>La requête n'a renvoyé aucune ligne.</p></div>}
        </div>
      )}
    </section>
  );
}
