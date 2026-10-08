import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, type DocumentSummary } from "../api";
import { href } from "../App";
import { DEFAULT_BRIDGE, health, send, type Agent, type BridgeEvent, type BridgeHealth } from "../chat";
import { DocIcon, PlusIcon, SearchIcon } from "../Icons";
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
  meta?: { duration_ms: number; turns: number; input_tokens: number; output_tokens: number; premium_requests?: number | null };
  /** Model that answered, and whether it had to stick to the documents. */
  model?: string;
  strict?: boolean;
}

interface Conversation {
  id: string;
  at: number;
  session: string | null;
  exchanges: Exchange[];
  /** The agent that holds the session (Claude Code when absent, as in earlier versions). */
  agent?: Agent;
}

const BRIDGE_STORAGE = "innerrag.chatBridge";
const MODEL_STORAGE = "innerrag.chatModel";
const COPILOT_MODEL_STORAGE = "innerrag.chatModelCopilot";
const AGENT_STORAGE = "innerrag.chatAgent";
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

// Copilot's catalogue depends on the subscription; a model the account lacks is refused with a clear error.
const COPILOT_MODELS: { value: string; label: string; key?: Key }[] = [
  { value: "", label: "", key: "chat.modelDefaultCopilot" },
  { value: "gpt-4.1", label: "GPT-4.1" },
  { value: "gpt-5-mini", label: "GPT-5 mini" },
  { value: "gpt-5", label: "GPT-5" },
  { value: "claude-sonnet-4.5", label: "Claude Sonnet 4.5" },
  { value: "claude-haiku-4.5", label: "Claude Haiku 4.5" },
];

const AGENT_NAME: Record<Agent, Key> = { claude: "chat.agentClaude", copilot: "chat.agentCopilot" };

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

interface PassageInfo {
  title?: string;
  heading?: string;
  page?: number;
}

/** What the tool results say about each passage id (`<doc id>#<index>`): its document's title, its
 *  section and its page. Search lines read "`id` Title › Section (page 4, score 0.91)", or "`id`
 *  Title (passage 3, score 0.91)" followed by the passage, whose first line is its section when it
 *  holds " › "; read_passages groups passages under "## Title" with "`id`, page 4" lines. */
function passageInfo(parts: Part[]): Map<string, PassageInfo> {
  const info = new Map<string, PassageInfo>();
  for (const p of parts) {
    if (p.kind !== "tool" || !p.result) continue;
    const lines = p.result.split("\n");
    let group: string | undefined;
    lines.forEach((line, i) => {
      const title = /^## (.+)$/.exec(line);
      if (title) {
        group = title[1].trim();
        return;
      }
      const m = /`([^`\n]+#\d+)`(.*)$/.exec(line);
      if (!m) return;
      const [, id, rest] = m;
      const cur = info.get(id) ?? {};
      const page = /\bpage (\d+)/.exec(rest);
      if (page) cur.page = Number(page[1]);
      const head = /^\s+(.+?)\s+\((?:passage|page) \d+/.exec(rest);
      if (head) {
        const [docTitle, ...path] = head[1].split(" › ");
        cur.title ??= docTitle;
        if (path.length) cur.heading ??= path.join(" › ");
      } else if (group) {
        cur.title ??= group;
      }
      const next = lines[i + 1] ?? "";
      if (!cur.heading && next.includes(" › ") && !next.includes("`")) cur.heading = next.trim();
      info.set(id, cur);
    });
  }
  return info;
}

interface SourceDoc {
  doc: string;
  title: string;
  passages: { id: string; idx: number; heading?: string; page?: number }[];
}

/** The passages an answer relied on, by document in the order the agent cited them: the ids given to
 *  cite_sources, or else the passages it read. */
function sourcesOf(parts: Part[], docs: { id: string; title: string }[], fetched: Map<string, PassageInfo>): { cited: boolean; docs: SourceDoc[] } {
  const ids = (name: string, field: string) =>
    parts.flatMap((p) => (p.kind === "tool" && p.name === name && Array.isArray(p.input[field]) ? (p.input[field] as unknown[]) : []))
      .filter((v): v is string => typeof v === "string" && /#\d+$/.test(v));
  const cited = ids("cite_sources", "chunk_ids");
  const chosen = cited.length ? cited : ids("read_passages", "ids");
  const info = passageInfo(parts);
  const titles = new Map(docs.map((d) => [d.id, d.title]));
  const byDoc = new Map<string, SourceDoc>();
  for (const id of new Set(chosen)) {
    const cut = id.lastIndexOf("#");
    const doc = id.slice(0, cut);
    const i = { ...fetched.get(id), ...info.get(id) };
    const entry = byDoc.get(doc) ?? { doc, title: titles.get(doc) ?? i.title ?? doc.replace(/^watch\//, ""), passages: [] };
    entry.passages.push({ id, idx: Number(id.slice(cut + 1)), heading: i.heading, page: i.page });
    byDoc.set(doc, entry);
  }
  for (const d of byDoc.values()) d.passages.sort((a, b) => a.idx - b.idx);
  return { cited: cited.length > 0, docs: [...byDoc.values()] };
}

/** The documents behind an answer, each with the sections and passages (or pages) it drew on. */
/** Sections and pages of passages the tool results did not describe, read from the server once. */
const fetchedPassages = new Map<string, PassageInfo>();

async function fetchPassages(project: string, doc: string, idxs: number[]) {
  const ranges: [number, number][] = [];
  for (const i of [...idxs].sort((a, b) => a - b)) {
    const last = ranges.at(-1);
    // Neighbours are read in one request; distant passages in their own.
    if (last && i - last[1] <= 8) last[1] = i;
    else ranges.push([i, i]);
  }
  await Promise.all(ranges.map(async ([from, to]) => {
    const page = await api.project(project).passages(doc, from, to - from + 1);
    for (const c of page.passages) {
      const first = c.text.split("\n", 1)[0];
      fetchedPassages.set(`${project}|${c.id}`, { heading: first.includes(" › ") ? first : undefined, page: c.page ?? undefined });
    }
  }));
}

function Sources({ parts, docs, project, t }: { parts: Part[]; docs: { id: string; title: string }[]; project: string; t: T }) {
  const [fetched, setFetched] = useState(() => new Map<string, PassageInfo>());
  const { cited, docs: sources } = useMemo(() => sourcesOf(parts, docs, fetched), [parts, docs, fetched]);
  useEffect(() => {
    const missing = sources.flatMap((d) => d.passages.filter((p) => !p.heading && !fetched.has(p.id)).map((p) => ({ doc: d.doc, ...p })));
    if (!missing.length) return;
    let live = true;
    const byDoc = new Map<string, number[]>();
    for (const m of missing) if (!fetchedPassages.has(`${project}|${m.id}`)) byDoc.set(m.doc, [...(byDoc.get(m.doc) ?? []), m.idx]);
    Promise.all([...byDoc].map(([doc, idxs]) => fetchPassages(project, doc, idxs).catch(() => undefined))).then(() => {
      if (!live) return;
      const next = new Map(fetched);
      // Asked once: an id the server did not return stays without a section rather than being asked again.
      for (const m of missing) next.set(m.id, fetchedPassages.get(`${project}|${m.id}`) ?? {});
      setFetched(next);
    });
    return () => {
      live = false;
    };
  }, [sources, fetched, project]);
  if (!sources.length) return null;
  const passages = sources.reduce((n, d) => n + d.passages.length, 0);
  return (
    // Folded by default: the line already says how many documents back the answer, and which.
    <details className="chat-sources">
      <summary className="chat-sources-head">
        <strong>{cited ? t("chat.sources") : t("chat.sourcesRead")}</strong>
        <span className="chat-sources-count">{t("chat.docs", { n: sources.length })}, {t("common.passages", { n: passages })}</span>
        <span className="chat-sources-titles">{sources.map((d) => d.title).join(" · ")}</span>
      </summary>
      <ul className="chat-source-list">
        {sources.map((d) => {
          // Passages of one section are listed together, by page when the document has pages.
          const sections = new Map<string, typeof d.passages>();
          for (const p of d.passages) sections.set(p.heading ?? "", [...(sections.get(p.heading ?? "") ?? []), p]);
          const firstPage = d.passages.find((p) => p.page)?.page;
          return (
            <li key={d.doc} className="chat-source">
              <a className="chat-source-doc" href={href("lire", firstPage ? { doc: d.doc, page: String(firstPage) } : { doc: d.doc })} title={t("chat.openDoc", { title: d.title })}>
                <DocIcon />
                <span className="chat-source-title">{d.title}</span>
              </a>
              <ul className="chat-source-sections">
                {[...sections].map(([heading, ps]) => {
                  const pages = [...new Set(ps.map((p) => p.page).filter((p): p is number => !!p))];
                  const where = pages.length
                    ? t("chat.pageNumbers", { n: pages.length, list: pages.join(", ") })
                    : t("chat.passageNumbers", { n: ps.length, list: ps.map((p) => p.idx + 1).join(", ") });
                  return (
                    <li key={heading}>
                      {heading && <span className="chat-source-heading" title={heading}>{heading.split(" › ").at(-1)}</span>}
                      <a className="chat-source-where" href={href("lire", pages.length ? { doc: d.doc, page: String(pages[0]) } : { doc: d.doc, tab: "passages" })}>{where}</a>
                    </li>
                  );
                })}
              </ul>
            </li>
          );
        })}
      </ul>
    </details>
  );
}

/** Conversations by age, for the list: today, yesterday, the last 7 days, older. */
function ageGroup(at: number): "chat.today" | "chat.yesterday" | "chat.lastWeek" | "chat.older" {
  const day = new Date();
  day.setHours(0, 0, 0, 0);
  const start = day.getTime();
  if (at >= start) return "chat.today";
  if (at >= start - 86_400_000) return "chat.yesterday";
  if (at >= start - 7 * 86_400_000) return "chat.lastWeek";
  return "chat.older";
}

const LIST_STORAGE = "innerrag.chatList";


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
  const [agentChoice, setAgentChoice] = useState<Agent>(() => (load<string>(AGENT_STORAGE, "claude") === "copilot" ? "copilot" : "claude"));
  const [claudeModel, setClaudeModel] = useState(() => load<string>(MODEL_STORAGE, ""));
  const [copilotModel, setCopilotModel] = useState(() => load<string>(COPILOT_MODEL_STORAGE, ""));
  const [strict, setStrict] = useState(() => load<boolean>(STRICT_STORAGE, true));
  const [confirmClear, setConfirmClear] = useState(false);
  // The list of conversations: shown by default where there is room for it beside the thread.
  const [listOpen, setListOpen] = useState(() => load<boolean | null>(LIST_STORAGE, null) ?? window.matchMedia("(min-width: 1100px)").matches);
  const [query, setQuery] = useState("");
  const [draft, setDraft] = useState("");
  /** Seeds of the starter questions, drawn from the graph; the sentences follow the language. */
  const [seeds, setSeeds] = useState<{ top?: string; link?: [string, string] } | null>(null);
  const [docs, setDocs] = useState<DocumentSummary[]>([]);
  const abort = useRef<AbortController | null>(null);
  const sending = useRef(false);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const running = conversation.exchanges.at(-1)?.status === "running";
  // A conversation stays with the agent that started it; a new one uses the current choice.
  const agent: Agent = conversation.exchanges.length ? conversation.agent ?? "claude" : agentChoice;
  const model = agent === "copilot" ? copilotModel : claudeModel;
  const models = agent === "copilot" ? COPILOT_MODELS : MODELS;
  const agentVersion = (a: Agent) => (a === "claude" ? status?.claude : status?.copilot) ?? null;

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
  // To the very bottom of the page: the composer sits there, sticky, below the end of the thread
  // (bringing the end of the thread into view instead scrolled the page back up under it).
  useEffect(() => {
    if (stick.current) window.scrollTo({ top: document.documentElement.scrollHeight });
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
          meta: { duration_ms: e.duration_ms, turns: e.turns, input_tokens: e.input_tokens, output_tokens: e.output_tokens, premium_requests: e.premium_requests },
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
    setConversation((c) => ({ ...c, at: Date.now(), exchanges: [...c.exchanges, { question: message, parts: [], status: "running", model: model || undefined, strict }], agent }));
    const ctrl = new AbortController();
    abort.current = ctrl;
    try {
      await send(bridge, { project, message, session: conversation.session, model: model || undefined, strict, conversation: conversation.id, agent }, onEvent, ctrl.signal);
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

  // The list, filtered by the search (questions and answers), grouped by age.
  const groups = useMemo(() => {
    const q = query.trim().toLowerCase();
    const shown = q
      ? saved.filter((c) => c.exchanges.some((x) => x.question.toLowerCase().includes(q) || x.parts.some((p) => p.kind === "text" && p.text.toLowerCase().includes(q))))
      : saved;
    const out: { key: ReturnType<typeof ageGroup>; items: Conversation[] }[] = [];
    for (const c of shown) {
      const key = ageGroup(c.at);
      const last = out.at(-1);
      if (last?.key === key) last.items.push(c);
      else out.push({ key, items: [c] });
    }
    return out;
  }, [saved, query]);

  const toggleList = () => {
    setListOpen((o) => {
      save(LIST_STORAGE, JSON.stringify(!o));
      return !o;
    });
  };

  return (
    <section className="page chat-page" aria-labelledby="chat-title">
      <div className="page-head">
        <div>
          <h1 id="chat-title">{t("chat.title")}</h1>
          <p>{t.rich("chat.intro", { project: <strong>{project}</strong> })}</p>
        </div>
        <div className="toolbar chat-head-actions">
          <button type="button" className="btn" aria-expanded={listOpen} aria-controls="chat-convs" onClick={toggleList}>
            {listOpen ? t("chat.hideConvs") : t("chat.showConvs", { n: saved.length })}
          </button>
          <button type="button" className="btn btn-primary" onClick={restart} disabled={running || empty}>
            <PlusIcon /> {t("chat.newConversation")}
          </button>
        </div>
      </div>

      <div className={`chat-layout${listOpen ? " with-list" : ""}`}>
      {listOpen && (
        <aside id="chat-convs" className="panel chat-convs" aria-labelledby="chats-title">
          <h2 id="chats-title" className="sr-only">{t("chat.conversations")}</h2>
          {saved.length > 0 && (
            <label className="field-box chat-convs-search">
              <SearchIcon size={16} />
              <span className="sr-only">{t("chat.searchConvsLabel")}</span>
              <input type="search" value={query} placeholder={t("chat.searchConvs")} onChange={(e) => setQuery(e.target.value)} />
            </label>
          )}
          {saved.length === 0 ? (
            <p className="muted">{t("chat.keptHere")}</p>
          ) : groups.length === 0 ? (
            <p className="muted">{t("chat.noMatch")}</p>
          ) : (
            <div className="chat-convs-scroll">
              {groups.map((g) => (
                <section key={g.key} className="chat-convs-group" aria-label={t(g.key)}>
                  <h3>{t(g.key)}</h3>
                  <ul className="chat-list">
                    {g.items.map((c) => (
                      <li key={c.id} className={c.id === conversation.id ? "current" : ""}>
                        <button type="button" className="chat-list-open" onClick={() => open(c)} disabled={running} title={conversationTitle(c)} aria-current={c.id === conversation.id || undefined}>
                          <span className="chat-list-title">{conversationTitle(c)}</span>
                          <span className="chat-list-meta">
                            {new Date(c.at).toLocaleString(locale(), g.key === "chat.today" || g.key === "chat.yesterday" ? { hour: "2-digit", minute: "2-digit" } : { day: "numeric", month: "short" })}
                            {" · "}
                            {t("chat.questions", { n: c.exchanges.length })}
                          </span>
                        </button>
                        <button type="button" className="chat-list-delete" aria-label={t("chat.deleteConv", { title: conversationTitle(c) })} onClick={() => remove(c.id)} disabled={running}>×</button>
                      </li>
                    ))}
                  </ul>
                </section>
              ))}
            </div>
          )}
          {saved.length > 0 && (
            <div className="chat-convs-foot">
              <p className="muted">{t("chat.keptMax", { n: KEPT_CONVERSATIONS })}</p>
              {confirmClear ? (
                <div className="chat-clear">
                  <span>{t("chat.clearConfirm", { n: saved.length })}</span>
                  <div className="toolbar">
                    <button type="button" className="btn" onClick={() => setConfirmClear(false)}>{t("common.cancel")}</button>
                    <button type="button" className="btn btn-danger-solid" onClick={clearAll}>{t("chat.clearAll")}</button>
                  </div>
                </div>
              ) : (
                <button type="button" className="btn btn-danger" onClick={() => setConfirmClear(true)} disabled={running}>{t("chat.clearConvs")}</button>
              )}
            </div>
          )}
        </aside>
      )}

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
              innerrag: <span className="mono">--innerrag {window.location.origin}</span>,
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
                <div key={j} className="chat-text"><RichText text={(p as { text: string }).text} docs={citedDocs} project={project} streaming={x.status === "running"} /></div>
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
              {x.status !== "running" && <Sources parts={x.parts} docs={citedDocs} project={project} t={t} />}
              {x.meta && (
                <p className="chat-meta">
                  {x.meta.input_tokens > 0
                    ? t("chat.meta", {
                        s: num(Math.round(x.meta.duration_ms / 100) / 10),
                        turns: t("chat.turns", { n: x.meta.turns }),
                        read: x.meta.input_tokens,
                        written: x.meta.output_tokens,
                      })
                    : t("chat.metaWritten", {
                        s: num(Math.round(x.meta.duration_ms / 100) / 10),
                        turns: t("chat.turns", { n: x.meta.turns }),
                        written: x.meta.output_tokens,
                      })}
                  {x.meta.premium_requests != null && `, ${t("chat.premium", { n: x.meta.premium_requests })}`}
                  {x.model && `, ${[...MODELS, ...COPILOT_MODELS].find((m) => m.value === x.model)?.label ?? x.model}`}
                  {x.strict === false && t("chat.general")}
                </p>
              )}
            </div>
          </article>
        ))}
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
        <div className="chat-controls" role="group" aria-label={t("chat.sideAria")}>
          <span className={`bridge-status${ready ? " on" : ""}`} role="status">
            {checking
              ? t("chat.looking")
              : ready && agentVersion(agent)
                ? t("chat.connected", {
                    agent: t(AGENT_NAME[agent]),
                    version: (agentVersion(agent) ?? "").replace(" (Claude Code)", "").replace("GitHub Copilot CLI ", "").replace(/\.$/, ""),
                  })
                : t("chat.bridgeOff")}
          </span>
          <label className="field-box" title={t("chat.agentHint")}>
            {t("chat.agent")}
            <select
              value={agent}
              disabled={running}
              onChange={(e) => {
                const next = e.target.value as Agent;
                setAgentChoice(next);
                save(AGENT_STORAGE, JSON.stringify(next));
                // A session belongs to one agent: switching starts a new conversation.
                if (conversation.exchanges.length && next !== agent) restart();
              }}
            >
              {(["claude", "copilot"] as Agent[]).map((a) => (
                <option key={a} value={a} disabled={!!status && !agentVersion(a)}>
                  {status && !agentVersion(a) ? t("chat.agentMissing", { agent: t(AGENT_NAME[a]) }) : t(AGENT_NAME[a])}
                </option>
              ))}
            </select>
          </label>
          <label className="field-box" title={t("chat.modelHint")}>
            {t("chat.model")}
            <select
              value={model}
              onChange={(e) => {
                if (agent === "copilot") {
                  setCopilotModel(e.target.value);
                  save(COPILOT_MODEL_STORAGE, JSON.stringify(e.target.value));
                } else {
                  setClaudeModel(e.target.value);
                  save(MODEL_STORAGE, JSON.stringify(e.target.value));
                }
              }}
            >
              {models.map((m) => <option key={m.value} value={m.value}>{m.key ? t(m.key) : m.label}</option>)}
            </select>
          </label>
          <label className="chat-switch" title={strict ? t("chat.strictOn") : t("chat.strictOff")}>
            <input
              type="checkbox"
              role="switch"
              checked={strict}
              aria-describedby="chat-strict-hint"
              onChange={(e) => {
                setStrict(e.target.checked);
                save(STRICT_STORAGE, JSON.stringify(e.target.checked));
              }}
            />
            <strong>{t("chat.strict")}</strong>
            <span id="chat-strict-hint" className="sr-only">{strict ? t("chat.strictOn") : t("chat.strictOff")}</span>
          </label>
        </div>
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
      </div>
    </section>
  );
}
