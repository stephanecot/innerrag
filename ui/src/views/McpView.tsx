import { useEffect, useState } from "react";
import { mcp, type McpResult, type McpTool } from "../api";
import { href } from "../App";
import { num } from "../util";

/** What each tool is for, in plain words. The server's own (English) description is what agents read. */
const GUIDE: Record<string, { short: string; what: string; when: string }> = {
  search_knowledge: {
    short: "Trouver les passages qui répondent à une question",
    what: "Le point d'entrée de presque toute question. Combine la recherche par le sens, par les mots-clés et par le graphe d'entités, puis renvoie les passages au-dessus du seuil, avec leur document, leur page et les entités liées.",
    when: "Dès qu'une question porte sur le contenu des documents.",
  },
  explore_entity: {
    short: "Voisins et passages d'une entité",
    what: "La fiche d'une entité : les entités qui apparaissent avec elle, les plus liées d'abord, et les passages qui la citent.",
    when: "Pour élargir à partir d'un nom repéré dans un résultat de recherche.",
  },
  explore_relation: {
    short: "Pourquoi deux entités sont liées",
    what: "Explique le lien entre deux entités : le nombre de passages qui les citent ensemble, la force du lien de 0 à 1 et ces passages.",
    when: "Pour répondre à « quel rapport entre A et B ? ».",
  },
  graph_stats: {
    short: "Chiffres du projet",
    what: "Les chiffres du projet : documents publiés et brouillons, passages, entités, relations et entités par type.",
    when: "Pour se situer avant d'explorer une base inconnue.",
  },
  run_cypher: {
    short: "Requête libre sur le graphe",
    what: "Une requête Cypher en lecture seule sur le graphe. Le schéma est donné dans la description de l'outil.",
    when: "Pour les questions de structure : compter, lister, croiser documents et entités.",
  },
  list_documents: {
    short: "Documents du projet",
    what: "Les documents du projet avec leur statut, leurs étiquettes et leur créateur, filtrables par statut ou étiquette.",
    when: "Pour savoir ce que contient la base, ou retrouver l'identifiant d'un document.",
  },
  ingest_document: {
    short: "Ajouter un texte",
    what: "Ajoute un texte : découpage en passages, embeddings, extraction des entités. Un identifiant existant remplace le document. L'ingestion se fait en arrière-plan, sauf avec wait.",
    when: "Quand l'agent doit enregistrer une note, un compte rendu ou une page qu'il a rédigée.",
  },
  ingest_file: {
    short: "Ajouter un fichier",
    what: "Ajoute un fichier PDF, Word, PowerPoint, Markdown ou texte, envoyé en base64. Le texte est extrait sur le serveur.",
    when: "Quand l'agent a un fichier sous la main, par exemple dans le dépôt qu'il modifie.",
  },
  ingestion_status: {
    short: "Suivre une ingestion",
    what: "L'avancement des ingestions : un job précis, ou les derniers jobs du projet.",
    when: "Après une ingestion lancée sans wait.",
  },
  list_projects: {
    short: "Projets du serveur",
    what: "Les projets du serveur. Utile sur l'adresse /mcp sans projet, où chaque outil accepte un argument project.",
    when: "Quand l'agent doit choisir dans quelle base chercher.",
  },
};

const GROUPS: { title: string; intro: string; tools: string[] }[] = [
  { title: "Chercher", intro: "Ce que l'agent appelle le plus souvent.", tools: ["search_knowledge"] },
  {
    title: "Explorer le graphe",
    intro: "Pour suivre les liens entre entités au-delà des passages trouvés.",
    tools: ["explore_entity", "explore_relation", "graph_stats", "run_cypher"],
  },
  {
    title: "Gérer les documents",
    intro: "Lister la base, et l'alimenter depuis l'agent.",
    tools: ["list_documents", "ingest_document", "ingest_file", "ingestion_status"],
  },
  { title: "Projets", intro: "", tools: ["list_projects"] },
];

/** Tools that change the base: documented here, not tried from this page. */
const WRITES = new Set(["ingest_document", "ingest_file"]);

const TYPE_LABEL: Record<string, string> = {
  string: "texte",
  integer: "entier",
  number: "nombre",
  boolean: "oui / non",
  array: "liste",
};

export default function McpView({ project, params }: { project: string; params: URLSearchParams }) {
  const [tools, setTools] = useState<McpTool[] | null>(null);
  const [error, setError] = useState("");
  const endpoint = `${window.location.origin}/mcp/${project}`;

  useEffect(() => {
    mcp.tools(project).then(setTools).catch((e) => setError((e as Error).message));
  }, [project]);

  const byName = new Map((tools ?? []).map((t) => [t.name, t]));
  const grouped = new Set(GROUPS.flatMap((g) => g.tools));
  const others = (tools ?? []).filter((t) => !grouped.has(t.name)).map((t) => t.name);
  const groups = (others.length ? [...GROUPS, { title: "Autres outils", intro: "", tools: others }] : GROUPS)
    .map((g) => ({ ...g, tools: g.tools.filter((n) => byName.has(n)) }))
    .filter((g) => g.tools.length);
  const selected = byName.get(params.get("tool") ?? "") ?? byName.get(groups[0]?.tools[0] ?? "");

  return (
    <section className="page mcp-page" aria-labelledby="mcp-title">
      <div className="page-head">
        <div>
          <h1 id="mcp-title">Outils MCP</h1>
          <p>
            Ce que voit un agent connecté à innerrag : le serveur lui envoie ces outils et leur description, l'agent choisit seul
            quand les appeler. Chaque appel apparaît dans l'historique.
          </p>
        </div>
      </div>

      <div className="mcp-connect">
        <Copyable label="Adresse du projet" value={endpoint} />
        <Copyable label="Claude Code" value={`claude mcp add --transport http innerrag ${endpoint}`} />
      </div>

      {error && <div className="error-banner" role="alert">Impossible de lire la liste des outils : {error}</div>}

      {tools && (
        <div className="mcp-layout">
          <nav className="panel mcp-list" aria-label="Outils">
            {groups.map((g) => (
              <div key={g.title} className="mcp-list-group">
                <span className="mcp-list-title">{g.title}</span>
                {g.tools.map((name) => (
                  <a
                    key={name}
                    href={href("mcp", { tool: name })}
                    aria-current={selected?.name === name ? "true" : undefined}
                  >
                    <span className="mono">{name}</span>
                    <span className="mcp-list-short">
                      {GUIDE[name]?.short ?? byName.get(name)?.description.split(". ")[0]}
                      {WRITES.has(name) && <span className="tool-kind writes">écriture</span>}
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
        {copied ? "Copié" : "Copier"}
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
  const guide = GUIDE[tool.name];
  const { props, required } = params(tool);
  const writes = WRITES.has(tool.name);

  return (
    <article className="panel tool-detail" aria-labelledby="tool-title">
      <header className="tool-head">
        <h2 id="tool-title" className="mono">{tool.name}</h2>
        <span className={`tool-kind${writes ? " writes" : ""}`}>{writes ? "modifie la base" : "lecture seule"}</span>
      </header>
      <p>{guide?.what ?? tool.description}</p>
      {guide && <p className="muted">Quand : {guide.when.charAt(0).toLowerCase() + guide.when.slice(1)}</p>}

      {props.length > 0 && (
        <dl className="tool-params">
          {props.map(([name, p]) => (
            <div key={name}>
              <dt>
                <span className="mono">{name}</span>
                <span className="muted">
                  {p.enum ? p.enum.join(" ou ") : TYPE_LABEL[p.type ?? ""] ?? p.type ?? ""}
                  {p.minimum !== undefined && p.maximum !== undefined && `, ${p.minimum} à ${p.maximum}`}
                </span>
                {required.has(name) && <span className="tool-required">obligatoire</span>}
              </dt>
              {p.description && <dd>{p.description}</dd>}
            </div>
          ))}
        </dl>
      )}

      <details className="tool-raw">
        <summary>Description exacte envoyée à l'agent</summary>
        <p>{tool.description}</p>
      </details>

      {writes ? (
        <p className="note">Cet outil modifie la base : il n'est pas proposé à l'essai ici. Pour ajouter un document, passez par la page Documents.</p>
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
      <h3>Essayer</h3>
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
                  placeholder={p.type === "array" ? "valeurs séparées par des virgules" : undefined}
                  value={String(values[name] ?? "")}
                  onChange={(e) => setValues({ ...values, [name]: e.target.value })}
                />
              )}
            </div>
          );
        })}
      </div>
      <div className="toolbar">
        <button type="submit" className="btn btn-primary" disabled={busy}>{busy ? "Appel en cours…" : "Appeler l'outil"}</button>
        {result && <span className="muted">L'agent lirait ≈ {num(Math.ceil(result.text.length / 4))} tokens.</span>}
      </div>
      {error && <div className="error-banner" role="alert">{error}</div>}
      {result && <pre className={`tool-result${result.isError ? " failed" : ""}`}>{result.text}</pre>}
    </form>
  );
}
