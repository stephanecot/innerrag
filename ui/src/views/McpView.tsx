import { useEffect, useState } from "react";
import { mcp, type McpResult, type McpTool } from "../api";
import { href } from "../App";
import { useT, type Key, type T } from "../i18n";

/** Tools with a plain-words guide (mcp.guide.<tool>.short/what/when). The server's own (English) description is what agents read. */
const GUIDED = new Set([
  "search_knowledge", "read_passages", "cite_sources", "docs_for", "doc_drift", "explore_entity", "explore_relation",
  "graph_stats", "run_cypher", "list_documents", "ingest_document", "ingest_file", "ingestion_status", "list_projects",
]);

function guide(t: T, tool: string): { short: string; what: string; when: string } | undefined {
  if (!GUIDED.has(tool)) return undefined;
  const k = (part: string) => t(`mcp.guide.${tool}.${part}` as Key);
  return { short: k("short"), what: k("what"), when: k("when") };
}

const GROUPS: { title: Key; intro?: Key; tools: string[] }[] = [
  { title: "mcp.group.search", intro: "mcp.group.searchIntro", tools: ["search_knowledge", "read_passages", "cite_sources"] },
  {
    title: "mcp.group.graph",
    intro: "mcp.group.graphIntro",
    tools: ["explore_entity", "explore_relation", "graph_stats", "run_cypher"],
  },
  { title: "mcp.group.code", intro: "mcp.group.codeIntro", tools: ["docs_for", "doc_drift"] },
  {
    title: "mcp.group.docs",
    intro: "mcp.group.docsIntro",
    tools: ["list_documents", "ingest_document", "ingest_file", "ingestion_status"],
  },
  { title: "mcp.group.projects", tools: ["list_projects"] },
];

/** Tools that change the base: documented here, not tried from this page. */
const WRITES = new Set(["ingest_document", "ingest_file"]);

const TYPES = new Set(["string", "integer", "number", "boolean", "array"]);

export default function McpView({ project, params }: { project: string; params: URLSearchParams }) {
  const [tools, setTools] = useState<McpTool[] | null>(null);
  const [error, setError] = useState("");
  const t = useT();
  const endpoint = `${window.location.origin}/mcp/${project}`;

  useEffect(() => {
    mcp.tools(project).then(setTools).catch((e) => setError((e as Error).message));
  }, [project]);

  const byName = new Map((tools ?? []).map((tool) => [tool.name, tool]));
  const grouped = new Set(GROUPS.flatMap((g) => g.tools));
  const others = (tools ?? []).filter((tool) => !grouped.has(tool.name)).map((tool) => tool.name);
  const groups = (others.length ? [...GROUPS, { title: "mcp.group.others" as Key, tools: others }] : GROUPS)
    .map((g) => ({ ...g, tools: g.tools.filter((n) => byName.has(n)) }))
    .filter((g) => g.tools.length);
  const selected = byName.get(params.get("tool") ?? "") ?? byName.get(groups[0]?.tools[0] ?? "");

  return (
    <section className="page mcp-page" aria-labelledby="mcp-title">
      <div className="page-head">
        <div>
          <h1 id="mcp-title">{t("mcp.title")}</h1>
          <p>{t("mcp.intro")}</p>
        </div>
      </div>

      <div className="mcp-connect">
        <Copyable label={t("mcp.endpoint")} value={endpoint} />
        <Copyable label="Claude Code" value={`claude mcp add --transport http innerrag ${endpoint}`} />
      </div>

      {error && <div className="error-banner" role="alert">{t("mcp.loadError", { error })}</div>}

      {tools && (
        <div className="mcp-layout">
          <nav className="panel mcp-list" aria-label={t("mcp.toolsAria")}>
            {groups.map((g) => (
              <div key={g.title} className="mcp-list-group">
                <span className="mcp-list-title">{t(g.title)}</span>
                {g.tools.map((name) => (
                  <a
                    key={name}
                    href={href("mcp", { tool: name })}
                    aria-current={selected?.name === name ? "true" : undefined}
                  >
                    <span className="mono">{name}</span>
                    <span className="mcp-list-short">
                      {guide(t, name)?.short ?? byName.get(name)?.description.split(". ")[0]}
                      {WRITES.has(name) && <span className="tool-kind writes">{t("mcp.writes")}</span>}
                    </span>
                  </a>
                ))}
              </div>
            ))}
          </nav>
          {selected && <ToolDetail key={selected.name} tool={selected} project={project} />}
        </div>
      )}
    </section>
  );
}

function Copyable({ label, value }: { label: string; value: string }) {
  const [copied, setCopied] = useState(false);
  const t = useT();
  return (
    <div className="copyable">
      <span className="copyable-label">{label}</span>
      <code className="mono" title={value}>{value}</code>
      <button
        type="button"
        className="copyable-button"
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(value);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          } catch {
            /* clipboard unavailable: the text stays selectable */
          }
        }}
      >
        {copied ? t("mcp.copied") : t("mcp.copy")}
      </button>
    </div>
  );
}

/** Parameters, required ones first. */
function params(tool: McpTool) {
  const required = new Set(tool.inputSchema.required ?? []);
  const props = Object.entries(tool.inputSchema.properties ?? {}).sort(([a], [b]) => Number(required.has(b)) - Number(required.has(a)));
  return { props, required };
}

function ToolDetail({ tool, project }: { tool: McpTool; project: string }) {
  const t = useT();
  const help = guide(t, tool.name);
  const { props, required } = params(tool);
  const writes = WRITES.has(tool.name);

  return (
    <article className="panel tool-detail" aria-labelledby="tool-title">
      <header className="tool-head">
        <h2 id="tool-title" className="mono">{tool.name}</h2>
        <span className={`tool-kind${writes ? " writes" : ""}`}>{writes ? t("mcp.modifies") : t("mcp.readOnly")}</span>
      </header>
      <p>{help?.what ?? tool.description}</p>
      {help && <p className="muted">{t("mcp.when", { when: help.when.charAt(0).toLowerCase() + help.when.slice(1) })}</p>}

      {props.length > 0 && (
        <dl className="tool-params">
          {props.map(([name, p]) => (
            <div key={name}>
              <dt>
                <span className="mono">{name}</span>
                <span className="muted">
                  {p.enum ? p.enum.join(t("mcp.or")) : TYPES.has(p.type ?? "") ? t(`mcp.type.${p.type}` as Key) : p.type ?? ""}
                  {p.minimum !== undefined && p.maximum !== undefined && t("mcp.range", { min: String(p.minimum), max: String(p.maximum) })}
                </span>
                {required.has(name) && <span className="tool-required">{t("mcp.required")}</span>}
              </dt>
              {p.description && <dd>{p.description}</dd>}
            </div>
          ))}
        </dl>
      )}

      <details className="tool-raw">
        <summary>{t("mcp.raw")}</summary>
        <p>{tool.description}</p>
      </details>

      {writes ? (
        <p className="note">{t("mcp.writeNote")}</p>
      ) : (
        <TryTool tool={tool} project={project} />
      )}
    </article>
  );
}

/** Calls the tool exactly as an agent would, and shows what the agent would read. */
function TryTool({ tool, project }: { tool: McpTool; project: string }) {
  const { props, required } = params(tool);
  const [values, setValues] = useState<Record<string, string | boolean>>({});
  const [result, setResult] = useState<McpResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const t = useT();

  const args = () => {
    const out: Record<string, unknown> = {};
    for (const [name, p] of props) {
      const v = values[name];
      if (v === undefined || v === "") continue;
      if (p.type === "boolean") out[name] = v === true;
      else if (p.type === "integer" || p.type === "number") out[name] = Number(v);
      else if (p.type === "array") out[name] = String(v).split(",").map((s) => s.trim()).filter(Boolean);
      else out[name] = v;
    }
    return out;
  };

  return (
    <form
      className="tool-try"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        setError("");
        try {
          setResult(await mcp.call(project, tool.name, args()));
        } catch (err) {
          setError((err as Error).message);
        } finally {
          setBusy(false);
        }
      }}
    >
      <h3>{t("mcp.try")}</h3>
      <div className="tool-try-fields">
        {props.map(([name, p]) => {
          const id = `try-${tool.name}-${name}`;
          if (p.type === "boolean") {
            return (
              <label key={name} className="field-box" htmlFor={id}>
                <input id={id} type="checkbox" checked={values[name] === true} onChange={(e) => setValues({ ...values, [name]: e.target.checked })} />
                {name}
              </label>
            );
          }
          return (
            <div key={name} className="field" style={{ flex: name === "query" ? "1 1 100%" : "1 1 180px" }}>
              <label htmlFor={id} className="mono">{name}{required.has(name) ? " *" : ""}</label>
              {p.enum ? (
                <select id={id} className="input" value={String(values[name] ?? "")} onChange={(e) => setValues({ ...values, [name]: e.target.value })}>
                  <option value="">—</option>
                  {p.enum.map((o) => <option key={o} value={o}>{o}</option>)}
                </select>
              ) : name === "query" && tool.name === "run_cypher" ? (
                <textarea
                  id={id}
                  className="textarea mono"
                  rows={3}
                  required={required.has(name)}
                  placeholder="MATCH (e:Entity) RETURN e.label, count(*) ORDER BY count(*) DESC"
                  value={String(values[name] ?? "")}
                  onChange={(e) => setValues({ ...values, [name]: e.target.value })}
                />
              ) : (
                <input
                  id={id}
                  className="input"
                  type={p.type === "integer" || p.type === "number" ? "number" : "text"}
                  step={p.type === "number" ? "0.01" : undefined}
                  min={p.minimum}
                  max={p.maximum}
                  required={required.has(name)}
                  placeholder={p.type === "array" ? t("mcp.listPh") : undefined}
                  value={String(values[name] ?? "")}
                  onChange={(e) => setValues({ ...values, [name]: e.target.value })}
                />
              )}
            </div>
          );
        })}
      </div>
      <div className="toolbar">
        <button type="submit" className="btn btn-primary" disabled={busy}>{busy ? t("mcp.calling") : t("mcp.call")}</button>
        {result && <span className="muted">{t("mcp.wouldRead", { n: Math.ceil(result.text.length / 4) })}</span>}
      </div>
      {error && <div className="error-banner" role="alert">{error}</div>}
      {result && <pre className={`tool-result${result.isError ? " failed" : ""}`}>{result.text}</pre>}
    </form>
  );
}
