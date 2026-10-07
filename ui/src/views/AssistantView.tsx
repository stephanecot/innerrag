import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, type DocumentSummary } from "../api";
import { DEFAULT_BRIDGE, health, send, type BridgeEvent, type BridgeHealth } from "../chat";
import RichText from "../RichText";
import { locale, translate, useT, type Key, type T } from "../i18n";
import { num } from "../util";

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

// Model names are not translated; the empty value is the bridge's default.
const MODELS: { value: string; label: string; key?: Key }[] = [
  { value: "", label: "", key: "chat.modelDefault" },
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

const conversationTitle = (c: Conversation) => c.exchanges[0]?.question ?? translate("chat.newConversation");
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

/** What a tool call did, in words. */
function toolLabel(t: T, name: string, input: Record<string, unknown>): string {
  const quote = (v: unknown) => (typeof v === "string" && v ? ` ${t("common.quoted", { text: v })}` : "");
  switch (name) {
    case "search_knowledge":
      return t(input.mode === "map" ? "tool.map" : "tool.search", { q: quote(input.query) });
    case "explore_entity":
      return t("tool.entity", { q: quote(input.name) });
    case "explore_relation":
      return t("tool.relation", { a: quote(input.a), b: quote(input.b) });
    case "list_documents":
      return t("tool.listDocs");
    case "read_passages":
      return (Array.isArray(input.ids) ? t("tool.read", { n: input.ids.length }) : t("tool.readSome")) + (input.window ? t("tool.withContext") : "");
    case "cite_sources":
      return input.outcome === "not_found" ? t("tool.notFound") : t("tool.cite", { n: Array.isArray(input.chunk_ids) ? input.chunk_ids.length : 0 });
    case "graph_stats":
      return t("tool.stats");
    case "run_cypher":
      return t("tool.cypher");
    case "list_projects":
      return t("tool.projects");
    case "ingestion_status":
      return t("tool.ingestions");
    default:
      return name;
  }
}

/** The short note an agent may write after reporting its sources ("sources recorded"): not for the reader. */
function isCitationNote(parts: Part[], k: number): boolean {
  const prev = parts[k - 1];
  const part = parts[k];
  return prev?.kind === "tool" && prev.name === "cite_sources" && part.kind === "text" && part.text.trim().length < 240 && k === parts.length - 1;
}

const tokens = (text: string) => Math.ceil(text.length / 4);


export default function AssistantView({ project }: { project: string }) {
  const storageKey = `innerrag.chats.${project}`;
  const [bridge, setBridge] = useState(() => load<string>(BRIDGE_STORAGE, "") || DEFAULT_BRIDGE);
  const [bridgeDraft, setBridgeDraft] = useState(bridge);
  const [status, setStatus] = useState<BridgeHealth | null>(null);
  const t = useT();
  /** Why the bridge is unavailable: Claude Code is down, or no bridge answers. */
  const [offline, setOffline] = useState<"" | "claude" | "bridge">("");
  const [checking, setChecking] = useState(true);
  const [saved, setSaved] = useState<Conversation[]>(() => loadConversations(project));
  const [conversation, setConversation] = useState<Conversation>(() => loadConversations(project)[0] ?? fresh());
  const [model, setModel] = useState(() => load<string>(MODEL_STORAGE, ""));
  const [strict, setStrict] = useState(() => load<boolean>(STRICT_STORAGE, true));
  const [confirmClear, setConfirmClear] = useState(false);
  const [draft, setDraft] = useState("");
  /** Seeds of the starter questions, drawn from the graph; the sentences follow the language. */
  const [seeds, setSeeds] = useState<{ top?: string; link?: [string, string] } | null>(null);
  const [docs, setDocs] = useState<DocumentSummary[]>([]);
  const abort = useRef<AbortController | null>(null);
  const sending = useRef(false);
  const endRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const running = conversation.exchanges.at(-1)?.status === "running";

  const check = useCallback(async () => {
    setChecking(true);
    try {
      const h = await health(bridge, AbortSignal.timeout(4000));
      setStatus(h);
      setOffline(h.ok ? "" : "claude");
    } catch {
      setStatus(null);
      setOffline("bridge");
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
  const citedDocs = useMemo(() => docs.map((d) => ({ id: d.id, title: d.title })), [docs]);

  // Questions to start with, drawn from the project's own graph.
  useEffect(() => {
    api.project(project).graph({ limit: 40 }).then((g) => {
      const byId = new Map(g.nodes.map((n) => [n.id, n]));
      const top = [...g.nodes].sort((a, b) => b.mentions - a.mentions)[0];
      const link = [...g.edges].filter((e) => e.weight >= 3).sort((a, b) => b.strength - a.strength)[0];
      setSeeds({
        top: top?.name,
        link: link && byId.has(link.source) && byId.has(link.target) ? [byId.get(link.source)!.name, byId.get(link.target)!.name] : undefined,
      });
    }).catch(() => setSeeds(null));
  }, [project]);
  const suggestions = seeds
    ? [
        t("chat.suggestTopics"),
        ...(seeds.top ? [t("chat.suggestEntity", { name: seeds.top })] : []),
        ...(seeds.link ? [t("chat.suggestLink", { a: seeds.link[0], b: seeds.link[1] })] : []),
      ]
    : [];

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

  // Follow the answer while it is written, as long as the reader stays at the bottom: scrolling
  // up stops following, coming back down resumes it.
  const stick = useRef(true);
  useEffect(() => {
    const onScroll = () => {
      stick.current = window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 160;
    };
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, []);
  useEffect(() => {
    if (stick.current) endRef.current?.scrollIntoView({ block: "end" });
  }, [conversation]);

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
          error: e.ok ? undefined : e.error ?? translate("chat.noAnswer"),
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
    // `running` comes from the last render: a second send in the same tick (double click, Enter
    // plus submit) would pass it, the ref does not.
    if (!message || running || sending.current) return;
    sending.current = true;
    stick.current = true;
    setDraft("");
    setConversation((c) => ({ ...c, at: Date.now(), exchanges: [...c.exchanges, { question: message, parts: [], status: "running", model: model || undefined, strict }] }));
    const ctrl = new AbortController();
    abort.current = ctrl;
    try {
      await send(bridge, { project, message, session: conversation.session, model: model || undefined, strict, conversation: conversation.id }, onEvent, ctrl.signal);
      update((x) => (x.status === "running" ? { ...x, status: "error", error: translate("chat.cut") } : x));
    } catch (err) {
      if (ctrl.signal.aborted) {
        update((x) => ({ ...x, status: "stopped" }));
      } else {
        update((x) => ({ ...x, status: "error", error: (err as Error).message === "Failed to fetch" ? translate("chat.bridgeGone", { bridge }) : (err as Error).message }));
        check();
      }
    } finally {
      abort.current = null;
      sending.current = false;
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
          <h1 id="chat-title">{t("chat.title")}</h1>
          <p>{t.rich("chat.intro", { project: <strong>{project}</strong> })}</p>
        </div>
      </div>

      <div className="chat-layout">
      <div className="chat-main">

      {!ready && !checking && (
        <section className="panel bridge-setup" aria-labelledby="bridge-title">
          <h2 id="bridge-title">{t("chat.setupTitle")}</h2>
          <p>
            {offline === "claude" ? t("chat.offlineClaude") : offline === "bridge" ? t("chat.offlineBridge", { bridge }) : ""} {t("chat.setupText")}
          </p>
          <pre className="code">{`python3 scripts/chat-bridge.py          # macOS, Linux
py scripts\\chat-bridge.py              # Windows`}</pre>
          <p className="muted">
            {t.rich("chat.setupNote", {
              model: <span className="mono">--model sonnet</span>,
              innerrag: <span className="mono">--innerrag http://localhost:18080</span>,
              writes: <span className="mono">--allow-writes</span>,
            })}
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
              {t("chat.bridgeAddress")}
              <input type="text" value={bridgeDraft} onChange={(e) => setBridgeDraft(e.target.value)} style={{ flex: 1 }} />
            </label>
            <button type="submit" className="btn btn-primary">{t("chat.retry")}</button>
          </form>
        </section>
      )}

      <div className="chat-thread">
        {empty && ready && (
          <div className="chat-start">
            <p className="muted">{t("chat.start")}</p>
            <div className="toolbar">
              {suggestions.map((s) => (
                <button key={s} type="button" className="btn chat-suggestion" onClick={() => ask(s)}>{s}</button>
              ))}
            </div>
          </div>
        )}

        {conversation.exchanges.map((x, i) => (
          <article key={i} className="chat-exchange" aria-label={t("chat.questionN", { n: String(i + 1) })}>
            <p className="chat-question">{x.question}</p>
            <div className="chat-answer">
              {x.parts.some((p) => p.kind === "tool") && (
                <ol className="chat-steps" aria-label={t("chat.stepsAria")}>
                  {x.parts.filter((p): p is Extract<Part, { kind: "tool" }> => p.kind === "tool").map((p) => (
                    <li key={p.id}>
                      <details>
                        <summary>
                          <span className={`chat-step-dot${p.result === undefined ? " busy" : p.error ? " failed" : ""}`} aria-hidden="true" />
                          <span className="chat-step-label">{toolLabel(t, p.name, p.input)}</span>
                          <span className="chat-step-size">
                            {p.result === undefined ? t("chat.stepRunning") : p.error ? t("chat.stepError") : t("chat.stepTokens", { n: p.tokens ?? tokens(p.result) })}
                          </span>
                        </summary>
                        {p.result !== undefined && <pre className="chat-step-result">{p.result}</pre>}
                      </details>
                    </li>
                  ))}
                </ol>
              )}
              {x.parts.filter((p, k) => p.kind === "text" && !isCitationNote(x.parts, k)).map((p, j) => (
                <div key={j} className="chat-text"><RichText text={(p as { text: string }).text} docs={citedDocs} /></div>
              ))}
              {x.status === "running" && (
                <p className="chat-wait" role="status">
                  {x.parts.at(-1)?.kind === "tool" && (x.parts.at(-1) as { result?: string }).result === undefined
                    ? t("chat.querying")
                    : x.parts.some((p) => p.kind === "text") ? t("chat.writing") : t("chat.thinking")}
                </p>
              )}
              {x.status === "stopped" && <p className="muted">{t("chat.stopped")}</p>}
              {x.status === "error" && <div className="error-banner" role="alert">{x.error}</div>}
              {x.meta && (
                <p className="chat-meta">
                  {t("chat.meta", {
                    s: num(Math.round(x.meta.duration_ms / 100) / 10),
                    turns: t("chat.turns", { n: x.meta.turns }),
                    read: x.meta.input_tokens,
                    written: x.meta.output_tokens,
                  })}
                  {x.model && `, ${MODELS.find((m) => m.value === x.model)?.label ?? x.model}`}
                  {x.strict === false && t("chat.general")}
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
        <label htmlFor="chat-input" className="sr-only">{t("chat.yourQuestion")}</label>
        <textarea
          id="chat-input"
          ref={inputRef}
          className="textarea"
          rows={2}
          placeholder={ready ? t("chat.placeholder") : t("chat.placeholderOff")}
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
            {t("chat.enterHint")}
            {totals.input > 0 && t("chat.totals", { read: totals.input, written: totals.output })}
          </span>
          {running ? (
            <button type="button" className="btn btn-danger" onClick={() => abort.current?.abort()}>{t("chat.stop")}</button>
          ) : (
            <button type="submit" className="btn btn-primary" disabled={!ready || !draft.trim()}>{t("chat.send")}</button>
          )}
        </div>
      </form>
      </div>

      <aside className="chat-side" aria-label={t("chat.sideAria")}>
        <section className="panel chat-side-box">
          <span className={`bridge-status${ready ? " on" : ""}`} role="status">
            {checking ? t("chat.looking") : ready ? t("chat.connected", { version: status?.claude?.replace(" (Claude Code)", "") ?? "" }) : t("chat.bridgeOff")}
          </span>
          <label className="field">
            <span className="label">{t("chat.model")}</span>
            <select
              className="input"
              value={model}
              onChange={(e) => {
                setModel(e.target.value);
                save(MODEL_STORAGE, JSON.stringify(e.target.value));
              }}
            >
              {MODELS.map((m) => <option key={m.value} value={m.value}>{m.key ? t(m.key) : m.label}</option>)}
            </select>
            <span className="hint">{t("chat.modelHint")}</span>
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
              <strong>{t("chat.strict")}</strong>
              <span className="hint">
                {strict
                  ? t("chat.strictOn")
                  : t("chat.strictOff")}
              </span>
            </span>
          </label>
        </section>

        <section className="panel chat-side-box" aria-labelledby="chats-title">
          <div className="chat-side-head">
            <h2 id="chats-title">{t("chat.conversations")}</h2>
            <button type="button" className="btn" onClick={restart} disabled={running || empty}>{t("chat.new")}</button>
          </div>
          {saved.length === 0 ? (
            <p className="muted">{t("chat.keptHere")}</p>
          ) : (
            <ul className="chat-list">
              {saved.map((c) => (
                <li key={c.id} className={c.id === conversation.id ? "current" : ""}>
                  <button type="button" className="chat-list-open" onClick={() => open(c)} disabled={running} title={conversationTitle(c)}>
                    <span className="chat-list-title">{conversationTitle(c)}</span>
                    <span className="chat-list-meta">
                      {new Date(c.at).toLocaleString(locale(), { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" })},{" "}
                      {t("chat.questions", { n: c.exchanges.length })}
                    </span>
                  </button>
                  <button type="button" className="chat-list-delete" aria-label={t("chat.deleteConv", { title: conversationTitle(c) })} onClick={() => remove(c.id)} disabled={running}>×</button>
                </li>
              ))}
            </ul>
          )}
          {saved.length > 0 && (confirmClear ? (
            <div className="chat-clear">
              <span>{t("chat.clearConfirm", { n: saved.length })}</span>
              <div className="toolbar">
                <button type="button" className="btn" onClick={() => setConfirmClear(false)}>{t("common.cancel")}</button>
                <button type="button" className="btn btn-danger-solid" onClick={clearAll}>{t("chat.clearAll")}</button>
              </div>
            </div>
          ) : (
            <button type="button" className="btn btn-danger" onClick={() => setConfirmClear(true)} disabled={running}>{t("chat.clearConvs")}</button>
          ))}
        </section>
      </aside>
      </div>
    </section>
  );
}
