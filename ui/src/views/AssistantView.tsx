import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, type DocumentSummary } from "../api";
import { href } from "../App";
import { DEFAULT_BRIDGE, health, send, type BridgeEvent, type BridgeHealth } from "../chat";
import { Blocks, parse, type Linker } from "../markdown";
import { num, plural } from "../util";

type Part =
  | { kind: "text"; text: string }
  | { kind: "tool"; id: string; name: string; input: Record<string, unknown>; result?: string; error?: boolean; tokens?: number };

interface Exchange {
  question: string;
  parts: Part[];
  status: "running" | "done" | "stopped" | "error";
  error?: string;
  meta?: { duration_ms: number; turns: number; input_tokens: number; output_tokens: number };
  /** Model that answered, and whether it had to stick to the documents. */
  model?: string;
  strict?: boolean;
}

interface Conversation {
  id: string;
  at: number;
  session: string | null;
  exchanges: Exchange[];
}

const BRIDGE_STORAGE = "innerrag.chatBridge";
const MODEL_STORAGE = "innerrag.chatModel";
const STRICT_STORAGE = "innerrag.chatStrict";
/** Conversations kept per project in this browser. */
const KEPT_CONVERSATIONS = 30;

const MODELS = [
  { value: "", label: "Celui de Claude Code" },
  { value: "opus", label: "Opus" },
  { value: "sonnet", label: "Sonnet" },
  { value: "haiku", label: "Haiku" },
];

const fresh = (): Conversation => ({ id: `c${Date.now().toString(36)}`, at: Date.now(), session: null, exchanges: [] });

/** Conversations of a project, most recent first; the single conversation of earlier versions is kept. */
function loadConversations(project: string): Conversation[] {
  const list = load<Conversation[]>(`innerrag.chats.${project}`, []);
  const legacy = load<{ session: string | null; exchanges: Exchange[] } | null>(`innerrag.chat.${project}`, null);
  if (legacy?.exchanges?.length && !list.length) return [{ ...fresh(), id: "c-legacy", session: legacy.session, exchanges: legacy.exchanges }];
  return list;
}

const conversationTitle = (c: Conversation) => c.exchanges[0]?.question ?? "Nouvelle conversation";
/** Tool results kept in the browser are cut to this length. */
const KEPT_RESULT = 6000;

function load<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

function save(key: string, value: unknown) {
  try {
    localStorage.setItem(key, typeof value === "string" ? value : JSON.stringify(value));
  } catch {
    /* storage unavailable or full */
  }
}

const quote = (v: unknown) => (typeof v === "string" && v ? ` « ${v} »` : "");

/** What a tool call did, in words. */
function toolLabel(name: string, input: Record<string, unknown>): string {
  switch (name) {
    case "search_knowledge":
      return `${input.mode === "map" ? "Survole" : "Recherche"}${quote(input.query)}`;
    case "explore_entity":
      return `Explore l'entité${quote(input.name)}`;
    case "explore_relation":
      return `Examine le lien${quote(input.a)} et${quote(input.b)}`;
    case "list_documents":
      return "Liste les documents";
    case "docs_for":
      return `Cherche la doc de${quote(input.target)}`;
    case "doc_drift":
      return "Cherche la doc périmée";
    case "read_passages":
      return `Lit ${Array.isArray(input.ids) ? input.ids.length : ""} passage${Array.isArray(input.ids) && input.ids.length > 1 ? "s" : ""}${input.window ? " avec leur contexte" : ""}`;
    case "cite_sources":
      return input.outcome === "not_found" ? "Signale une question sans réponse" : `Note les sources de sa réponse (${Array.isArray(input.chunk_ids) ? input.chunk_ids.length : 0})`;
    case "graph_stats":
      return "Consulte les chiffres de la base";
    case "run_cypher":
      return "Interroge le graphe en Cypher";
    case "list_projects":
      return "Liste les projets";
    case "ingestion_status":
      return "Vérifie les ingestions";
    default:
      return name;
  }
}

const tokens = (text: string) => Math.ceil(text.length / 4);

/** Links each cited document title, with its page when one follows ("Pro Android 5, page 486"), to the reader. */
function citationLinker(docs: DocumentSummary[]): Linker | undefined {
  const titled = docs.filter((d) => d.title.trim().length >= 3).sort((a, b) => b.title.length - a.title.length);
  if (!titled.length) return undefined;
  const escape = (t: string) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const byTitle = new Map(titled.map((d) => [d.title.toLowerCase(), d.id]));
  const re = new RegExp(`(${titled.map((d) => escape(d.title)).join("|")})(,?\\s*(?:page|p\\.)\\s*(\\d+))?`, "gi");
  return (text, key) => {
    const out: ReactNode[] = [];
    let last = 0;
    let n = 0;
    for (const m of text.matchAll(re)) {
      const doc = byTitle.get(m[1].toLowerCase());
      if (!doc || m.index === undefined) continue;
      if (m.index > last) out.push(text.slice(last, m.index));
      const params: Record<string, string> = m[3] ? { doc, page: m[3] } : { doc };
      out.push(<a key={`${key}-${n++}`} href={href("lire", params)}>{m[0]}</a>);
      last = m.index + m[0].length;
    }
    if (last < text.length) out.push(text.slice(last));
    return out;
  };
}

export default function AssistantView({ project }: { project: string }) {
  const storageKey = `innerrag.chats.${project}`;
  const [bridge, setBridge] = useState(() => load<string>(BRIDGE_STORAGE, "") || DEFAULT_BRIDGE);
  const [bridgeDraft, setBridgeDraft] = useState(bridge);
  const [status, setStatus] = useState<BridgeHealth | null>(null);
  const [offline, setOffline] = useState("");
  const [checking, setChecking] = useState(true);
  const [saved, setSaved] = useState<Conversation[]>(() => loadConversations(project));
  const [conversation, setConversation] = useState<Conversation>(() => loadConversations(project)[0] ?? fresh());
  const [model, setModel] = useState(() => load<string>(MODEL_STORAGE, ""));
  const [strict, setStrict] = useState(() => load<boolean>(STRICT_STORAGE, true));
  const [confirmClear, setConfirmClear] = useState(false);
  const [draft, setDraft] = useState("");
  const [suggestions, setSuggestions] = useState<string[]>([]);
  const [docs, setDocs] = useState<DocumentSummary[]>([]);
  const abort = useRef<AbortController | null>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const running = conversation.exchanges.at(-1)?.status === "running";

  const check = useCallback(async () => {
    setChecking(true);
    try {
      const h = await health(bridge, AbortSignal.timeout(4000));
      setStatus(h);
      setOffline(h.ok ? "" : "Claude Code ne répond pas sur cette machine.");
    } catch {
      setStatus(null);
      setOffline(`Aucun pont à l'adresse ${bridge}.`);
    } finally {
      setChecking(false);
    }
  }, [bridge]);

  useEffect(() => {
    check();
  }, [check]);

  useEffect(() => {
    api.project(project).documents().then(setDocs).catch(() => setDocs([]));
  }, [project]);
  const linker = useMemo(() => citationLinker(docs), [docs]);

  // Questions to start with, drawn from the project's own graph.
  useEffect(() => {
    api.project(project).graph({ limit: 40 }).then((g) => {
      const byId = new Map(g.nodes.map((n) => [n.id, n]));
      const top = [...g.nodes].sort((a, b) => b.mentions - a.mentions)[0];
      const link = [...g.edges].filter((e) => e.weight >= 3).sort((a, b) => b.strength - a.strength)[0];
      const out = ["Quels sont les grands sujets de cette base ?"];
      if (top) out.push(`Que dit la base sur ${top.name} ?`);
      if (link && byId.has(link.source) && byId.has(link.target)) {
        out.push(`Quel rapport entre ${byId.get(link.source)!.name} et ${byId.get(link.target)!.name} ?`);
      }
      setSuggestions(out);
    }).catch(() => setSuggestions([]));
  }, [project]);

  // Keep the conversations across page changes; never store a half-finished answer as running.
  useEffect(() => {
    if (running || !conversation.exchanges.length) return;
    const trimmed: Conversation = {
      ...conversation,
      exchanges: conversation.exchanges.map((x) => ({
        ...x,
        parts: x.parts.map((p) => (p.kind === "tool" && p.result && p.result.length > KEPT_RESULT ? { ...p, result: `${p.result.slice(0, KEPT_RESULT)}\n…` } : p)),
      })),
    };
    setSaved((list) => {
      const next = [trimmed, ...list.filter((c) => c.id !== trimmed.id)].sort((a, b) => b.at - a.at).slice(0, KEPT_CONVERSATIONS);
      save(storageKey, next);
      return next;
    });
  }, [conversation, running, storageKey]);

  const persist = (list: Conversation[]) => {
    setSaved(list);
    save(storageKey, list);
    try {
      localStorage.removeItem(`innerrag.chat.${project}`);
    } catch {
      /* storage unavailable */
    }
  };

  useEffect(() => () => abort.current?.abort(), []);

  // Follow the answer while it is written, unless the reader scrolled up.
  useEffect(() => {
    const el = endRef.current;
    if (!el) return;
    const near = window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 240;
    if (near || running) el.scrollIntoView({ block: "end" });
  }, [conversation, running]);

  const update = (fn: (x: Exchange) => Exchange) =>
    setConversation((c) => ({ ...c, exchanges: [...c.exchanges.slice(0, -1), fn(c.exchanges[c.exchanges.length - 1])] }));

  const onEvent = (e: BridgeEvent) => {
    switch (e.type) {
      case "session":
        setConversation((c) => ({ ...c, session: e.id }));
        break;
      case "text":
        update((x) => {
          const last = x.parts.at(-1);
          const parts = last?.kind === "text"
            ? [...x.parts.slice(0, -1), { ...last, text: last.text + e.delta }]
            : [...x.parts, { kind: "text" as const, text: e.delta }];
          return { ...x, parts };
        });
        break;
      case "tool":
        update((x) => ({ ...x, parts: [...x.parts, { kind: "tool", id: e.id, name: e.name, input: e.input }] }));
        break;
      case "tool_result":
        update((x) => ({
          ...x,
          parts: x.parts.map((p) => (p.kind === "tool" && p.id === e.id ? { ...p, result: e.text, error: e.is_error, tokens: tokens(e.text) } : p)),
        }));
        break;
      case "done":
        setConversation((c) => ({ ...c, session: e.session ?? c.session }));
        update((x) => ({
          ...x,
          status: e.ok ? "done" : "error",
          error: e.ok ? undefined : e.error ?? "Claude n'a pas pu répondre.",
          meta: { duration_ms: e.duration_ms, turns: e.turns, input_tokens: e.input_tokens, output_tokens: e.output_tokens },
        }));
        break;
      case "error":
        update((x) => ({ ...x, status: "error", error: e.message }));
        break;
    }
  };

  const ask = async (question: string) => {
    const message = question.trim();
    if (!message || running) return;
    setDraft("");
    setConversation((c) => ({ ...c, at: Date.now(), exchanges: [...c.exchanges, { question: message, parts: [], status: "running", model: model || undefined, strict }] }));
    const ctrl = new AbortController();
    abort.current = ctrl;
    try {
      await send(bridge, { project, message, session: conversation.session, model: model || undefined, strict, conversation: conversation.id }, onEvent, ctrl.signal);
      update((x) => (x.status === "running" ? { ...x, status: "error", error: "Le pont a coupé la réponse." } : x));
    } catch (err) {
      if (ctrl.signal.aborted) {
        update((x) => ({ ...x, status: "stopped" }));
      } else {
        update((x) => ({ ...x, status: "error", error: (err as Error).message === "Failed to fetch" ? `Le pont ne répond plus (${bridge}).` : (err as Error).message }));
        check();
      }
    } finally {
      abort.current = null;
      inputRef.current?.focus();
    }
  };

  const restart = () => {
    abort.current?.abort();
    setConversation(fresh());
    inputRef.current?.focus();
  };

  const open = (c: Conversation) => {
    if (running) return;
    setConversation(c);
  };

  const remove = (id: string) => {
    persist(saved.filter((c) => c.id !== id));
    if (conversation.id === id) restart();
  };

  const clearAll = () => {
    persist([]);
    setConfirmClear(false);
    restart();
  };

  const totals = useMemo(() => {
    const done = conversation.exchanges.filter((x) => x.meta);
    return {
      input: done.reduce((s, x) => s + (x.meta?.input_tokens ?? 0), 0),
      output: done.reduce((s, x) => s + (x.meta?.output_tokens ?? 0), 0),
    };
  }, [conversation.exchanges]);

  const ready = !!status?.ok;
  const empty = conversation.exchanges.length === 0;

  return (
    <section className="page chat-page" aria-labelledby="chat-title">
      <div className="page-head">
        <div>
          <h1 id="chat-title">Assistant</h1>
          <p>
            Le Claude Code de cette machine répond à partir du projet <strong>{project}</strong> : il interroge la base par MCP,
            puis cite ses sources. Il utilise votre abonnement Claude, sans clé d'API.
          </p>
        </div>
      </div>

      <div className="chat-layout">
      <div className="chat-main">

      {!ready && !checking && (
        <section className="panel bridge-setup" aria-labelledby="bridge-title">
          <h2 id="bridge-title">Démarrer le pont vers Claude Code</h2>
          <p>
            {offline} Le serveur innerrag tourne dans Docker et ne peut pas lancer le Claude Code de votre poste : un petit
            script fait le lien. Lancez-le depuis le dossier d'innerrag, sur la machine où vous êtes connecté à Claude Code.
          </p>
          <pre className="code">{`python3 scripts/chat-bridge.py          # macOS, Linux
py scripts\\chat-bridge.py              # Windows`}</pre>
          <p className="muted">
            Il écoute sur 127.0.0.1:18765 et ne répond qu'à cette interface. Claude n'y a accès qu'aux outils de lecture
            d'innerrag : ni terminal, ni fichiers, ni ingestion. Options : <span className="mono">--model sonnet</span>,{" "}
            <span className="mono">--innerrag http://localhost:18080</span>, <span className="mono">--allow-writes</span>.
          </p>
          <form
            className="toolbar"
            onSubmit={(e) => {
              e.preventDefault();
              const url = bridgeDraft.trim().replace(/\/+$/, "") || DEFAULT_BRIDGE;
              save(BRIDGE_STORAGE, JSON.stringify(url));
              setBridge(url);
              if (url === bridge) check();
            }}
          >
            <label className="field-box" style={{ flex: "1 1 280px" }}>
              Adresse du pont
              <input type="text" value={bridgeDraft} onChange={(e) => setBridgeDraft(e.target.value)} style={{ flex: 1 }} />
            </label>
            <button type="submit" className="btn btn-primary">Réessayer</button>
          </form>
        </section>
      )}

      <div className="chat-thread">
        {empty && ready && (
          <div className="chat-start">
            <p className="muted">Pour commencer :</p>
            <div className="toolbar">
              {suggestions.map((s) => (
                <button key={s} type="button" className="btn chat-suggestion" onClick={() => ask(s)}>{s}</button>
              ))}
            </div>
          </div>
        )}

        {conversation.exchanges.map((x, i) => (
          <article key={i} className="chat-exchange" aria-label={`Question ${i + 1}`}>
            <p className="chat-question">{x.question}</p>
            <div className="chat-answer">
              {x.parts.some((p) => p.kind === "tool") && (
                <ol className="chat-steps" aria-label="Appels à innerrag">
                  {x.parts.filter((p): p is Extract<Part, { kind: "tool" }> => p.kind === "tool").map((p) => (
                    <li key={p.id}>
                      <details>
                        <summary>
                          <span className={`chat-step-dot${p.result === undefined ? " busy" : p.error ? " failed" : ""}`} aria-hidden="true" />
                          <span className="chat-step-label">{toolLabel(p.name, p.input)}</span>
                          <span className="chat-step-size">
                            {p.result === undefined ? "en cours" : p.error ? "erreur" : `≈ ${num(p.tokens ?? tokens(p.result))} tokens`}
                          </span>
                        </summary>
                        {p.result !== undefined && <pre className="chat-step-result">{p.result}</pre>}
                      </details>
                    </li>
                  ))}
                </ol>
              )}
              {x.parts.filter((p) => p.kind === "text").map((p, j) => (
                <div key={j} className="chat-text"><Blocks blocks={parse((p as { text: string }).text)} linker={linker} /></div>
              ))}
              {x.status === "running" && (
                <p className="chat-wait" role="status">
                  {x.parts.at(-1)?.kind === "tool" && (x.parts.at(-1) as { result?: string }).result === undefined
                    ? "Interroge innerrag…"
                    : x.parts.some((p) => p.kind === "text") ? "Rédige…" : "Réfléchit…"}
                </p>
              )}
              {x.status === "stopped" && <p className="muted">Réponse arrêtée.</p>}
              {x.status === "error" && <div className="error-banner" role="alert">{x.error}</div>}
              {x.meta && (
                <p className="chat-meta">
                  {num(Math.round(x.meta.duration_ms / 100) / 10)} s, {plural(x.meta.turns, "tour", "tours")},{" "}
                  {num(x.meta.input_tokens)} tokens lus, {num(x.meta.output_tokens)} écrits
                  {x.model && `, ${MODELS.find((m) => m.value === x.model)?.label ?? x.model}`}
                  {x.strict === false && ", connaissances générales permises"}
                </p>
              )}
            </div>
          </article>
        ))}
        <div ref={endRef} />
      </div>

      <form
        className="chat-composer"
        onSubmit={(e) => {
          e.preventDefault();
          ask(draft);
        }}
      >
        <label htmlFor="chat-input" className="sr-only">Votre question</label>
        <textarea
          id="chat-input"
          ref={inputRef}
          className="textarea"
          rows={2}
          placeholder={ready ? "Posez une question sur les documents du projet" : "Démarrez le pont pour discuter avec Claude"}
          value={draft}
          disabled={!ready}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              ask(draft);
            }
          }}
        />
        <div className="chat-composer-foot">
          <span className="muted">
            Entrée pour envoyer, Maj+Entrée pour aller à la ligne.
            {totals.input > 0 && ` Cette conversation : ${num(totals.input)} tokens lus, ${num(totals.output)} écrits.`}
          </span>
          {running ? (
            <button type="button" className="btn btn-danger" onClick={() => abort.current?.abort()}>Arrêter</button>
          ) : (
            <button type="submit" className="btn btn-primary" disabled={!ready || !draft.trim()}>Envoyer</button>
          )}
        </div>
      </form>
      </div>

      <aside className="chat-side" aria-label="Réglages et conversations">
        <section className="panel chat-side-box">
          <span className={`bridge-status${ready ? " on" : ""}`} role="status">
            {checking ? "Recherche du pont…" : ready ? `Claude Code ${status?.claude?.replace(" (Claude Code)", "") ?? ""} connecté` : "Pont arrêté"}
          </span>
          <label className="field">
            <span className="label">Modèle</span>
            <select
              className="input"
              value={model}
              onChange={(e) => {
                setModel(e.target.value);
                save(MODEL_STORAGE, JSON.stringify(e.target.value));
              }}
            >
              {MODELS.map((m) => <option key={m.value} value={m.value}>{m.label}</option>)}
            </select>
            <span className="hint">Changeable à tout moment, même au milieu d'une conversation.</span>
          </label>
          <label className="chat-switch">
            <input
              type="checkbox"
              role="switch"
              checked={strict}
              onChange={(e) => {
                setStrict(e.target.checked);
                save(STRICT_STORAGE, JSON.stringify(e.target.checked));
              }}
            />
            <span>
              <strong>Documents seulement</strong>
              <span className="hint">
                {strict
                  ? "Claude ne répond qu'à partir des passages trouvés et dit quand la base ne contient pas la réponse."
                  : "Claude peut compléter avec ses connaissances, dans une partie « Hors documents » séparée."}
              </span>
            </span>
          </label>
        </section>

        <section className="panel chat-side-box" aria-labelledby="chats-title">
          <div className="chat-side-head">
            <h2 id="chats-title">Conversations</h2>
            <button type="button" className="btn" onClick={restart} disabled={running || empty}>Nouvelle</button>
          </div>
          {saved.length === 0 ? (
            <p className="muted">Elles sont gardées dans ce navigateur, par projet.</p>
          ) : (
            <ul className="chat-list">
              {saved.map((c) => (
                <li key={c.id} className={c.id === conversation.id ? "current" : ""}>
                  <button type="button" className="chat-list-open" onClick={() => open(c)} disabled={running} title={conversationTitle(c)}>
                    <span className="chat-list-title">{conversationTitle(c)}</span>
                    <span className="chat-list-meta">
                      {new Date(c.at).toLocaleString("fr-FR", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" })},{" "}
                      {plural(c.exchanges.length, "question", "questions")}
                    </span>
                  </button>
                  <button type="button" className="chat-list-delete" aria-label={`Supprimer la conversation « ${conversationTitle(c)} »`} onClick={() => remove(c.id)} disabled={running}>×</button>
                </li>
              ))}
            </ul>
          )}
          {saved.length > 0 && (confirmClear ? (
            <div className="chat-clear">
              <span>Effacer les {saved.length} conversations de ce projet ?</span>
              <div className="toolbar">
                <button type="button" className="btn" onClick={() => setConfirmClear(false)}>Annuler</button>
                <button type="button" className="btn btn-danger-solid" onClick={clearAll}>Tout effacer</button>
              </div>
            </div>
          ) : (
            <button type="button" className="btn btn-danger" onClick={() => setConfirmClear(true)} disabled={running}>Effacer les conversations</button>
          ))}
        </section>
      </aside>
      </div>
    </section>
  );
}
