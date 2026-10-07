import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import ForceGraph2D, { type ForceGraphMethods, type LinkObject, type NodeObject } from "react-force-graph-2d";
import { api, type DocumentSummary, type EntityDetail, type GraphView, type RelationDetail, type Stats } from "../api";
import { href } from "../App";
import { CloseIcon, DocTypeIcon, SearchIcon } from "../Icons";
import Figures from "../Figures";
import { translate, useT } from "../i18n";
import { cssVar, docKind, docKindInfo, labelColor, labelName, labelVar, num, num2, splitHighlights, statusLabel, STRENGTH_FULL, STRENGTH_RAMP, strengthColor, strengthLabel } from "../util";

interface NodeData {
  id: string;
  name: string;
  label: string;
  mentions: number;
  published: number;
}
type GNode = NodeObject<NodeData>;
type GLink = LinkObject<NodeData, { weight: number; strength: number }>;

/** Node radius in graph units: grows with mentions, shrinks gently when zooming in. */
const radius = (mentions: number, scale: number) =>
  Math.min(18, 4 + Math.sqrt(mentions) * 3) / Math.sqrt(Math.max(scale, 1));

const endId = (end: string | number | GNode | undefined) =>
  typeof end === "object" && end !== null ? String(end.id) : String(end);

/** Re-renders when the color theme changes, so canvas colors follow it. */
function useThemeTick() {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const bump = () => setTick((t) => t + 1);
    const observer = new MutationObserver(bump);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", bump);
    return () => {
      observer.disconnect();
      media.removeEventListener("change", bump);
    };
  }, []);
  return tick;
}

function useSize<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [size, setSize] = useState({ width: 800, height: 600 });
  useEffect(() => {
    if (!ref.current) return;
    const ro = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect;
      setSize({ width: Math.max(200, width), height: Math.max(300, height) });
    });
    ro.observe(ref.current);
    return () => ro.disconnect();
  }, []);
  return [ref, size] as const;
}

export default function MapView({ project, params }: { project: string; params: URLSearchParams }) {
  const p = useMemo(() => api.project(project), [project]);
  const [stats, setStats] = useState<Stats | null>(null);
  const [data, setData] = useState<{ nodes: GNode[]; links: GLink[] }>({ nodes: [], links: [] });
  const [limit, setLimit] = useState(150);
  const [minWeight, setMinWeight] = useState(1);
  const [drafts, setDrafts] = useState(true);
  // Map of one document (its entities, linked within it), remembered per project.
  const mapDocKey = `innerrag.mapDoc.${project}`;
  const [mapDoc, setMapDocState] = useState(() => {
    try {
      return localStorage.getItem(mapDocKey) ?? "";
    } catch {
      return "";
    }
  });
  const [docs, setDocs] = useState<DocumentSummary[]>([]);
  const [hidden, setHidden] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<string | null>(params.get("entity"));
  const [detail, setDetail] = useState<EntityDetail | null>(null);
  // Document whose passages the entity panel shows (all documents when null).
  const [docFilter, setDocFilter] = useState<string | null>(null);
  const [allNeighbours, setAllNeighbours] = useState(false);
  /** A clicked link: why are these two entities connected? */
  const [relation, setRelation] = useState<RelationDetail | null>(null);
  const [error, setError] = useState("");
  const [find, setFind] = useState("");
  const [loading, setLoading] = useState(true);
  const fg = useRef<ForceGraphMethods<GNode, GLink>>(undefined);
  const lastClick = useRef<{ id: string; at: number }>({ id: "", at: 0 });
  const fitted = useRef(false);
  const [boxRef, size] = useSize<HTMLDivElement>();
  const themeTick = useThemeTick();
  const t = useT();

  const merge = useCallback((view: GraphView, replace: boolean) => {
    setData((prev) => {
      const byId = new Map<string, GNode>(replace ? [] : prev.nodes.map((n) => [String(n.id), n]));
      for (const n of view.nodes) {
        const existing = byId.get(n.id);
        if (existing) {
          existing.mentions = n.mentions;
          existing.published = n.published_mentions;
        } else {
          byId.set(n.id, { id: n.id, name: n.name, label: n.label, mentions: n.mentions, published: n.published_mentions });
        }
      }
      const links = new Map<string, GLink>();
      if (!replace) for (const l of prev.links) links.set(`${endId(l.source)}|${endId(l.target)}`, l);
      for (const e of view.edges) {
        const k = `${e.source}|${e.target}`;
        if (!links.has(k)) links.set(k, { source: e.source, target: e.target, weight: e.weight, strength: e.strength });
      }
      return { nodes: [...byId.values()], links: [...links.values()] };
    });
  }, []);

  // Dense graphs (one big document: everything co-occurs) are thinned automatically.
  const [autoWeight, setAutoWeight] = useState<number | null>(null);
  const autoDecided = useRef(false);
  const refit = useRef(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [s, g] = await Promise.all([p.stats(), p.graph({ limit, min_weight: minWeight, include_drafts: drafts, doc: mapDoc || undefined })]);
      setStats(s);
      const decide = !autoDecided.current;
      autoDecided.current = true;
      if (decide && minWeight === 1 && g.edges.length > g.nodes.length * 4) {
        const weights = g.edges.map((e) => e.weight).sort((a, b) => b - a);
        const cut = Math.max(2, weights[Math.min(weights.length - 1, g.nodes.length * 3)]);
        if (cut > 1) {
          setAutoWeight(cut);
          setMinWeight(cut);
          return;
        }
      }
      merge(g, true);
      // A new document's map is framed while its layout spreads out (the engine-stop event does
      // not come reliably once the simulation has been reheated).
      if (refit.current) {
        refit.current = false;
        fitted.current = true;
        for (const delay of [1200, 3500, 7000]) setTimeout(() => fitRef.current(), delay);
      }
      setError("");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setLoading(false);
    }
  }, [p, limit, minWeight, drafts, mapDoc, merge]);

  useEffect(() => {
    p.documents().then(setDocs).catch(() => setDocs([]));
  }, [p]);

  // A forgotten or deleted document falls back to the whole project.
  useEffect(() => {
    if (mapDoc && docs.length && !docs.some((d) => d.id === mapDoc)) setMapDoc("");
  }, [docs, mapDoc]);

  const setMapDoc = (id: string) => {
    setMapDocState(id);
    try {
      localStorage.setItem(mapDocKey, id);
    } catch {
      /* storage unavailable */
    }
    // Each map decides its own thinning.
    autoDecided.current = false;
    refit.current = true;
    setAutoWeight(null);
    setMinWeight(1);
    setSelected(null);
  };
  const mapDocTitle = docs.find((d) => d.id === mapDoc)?.title;

  useEffect(() => {
    load();
  }, [load]);

  const expand = useCallback(
    async (id: string) => {
      try {
        merge(await p.neighbourhood(id, 25), false);
      } catch (e) {
        setError((e as Error).message);
      }
    },
    [p, merge],
  );

  // Entity coming from another view (#/carte?entity=…): bring its neighbourhood in.
  useEffect(() => {
    const id = params.get("entity");
    if (id) {
      setSelected(id);
      expand(id);
    }
  }, [params, expand]);

  useEffect(() => {
    setDocFilter(mapDoc || null);
    setAllNeighbours(false);
  }, [selected, mapDoc]);

  useEffect(() => {
    if (!selected) {
      setDetail(null);
      return;
    }
    let live = true;
    p.entity(selected, docFilter ?? undefined)
      .then((d) => live && setDetail(d))
      .catch((e) => {
        if (!live) return;
        // An entity that no longer exists (renamed or re-labelled at a re-import): close its panel.
        setError((e as Error).message);
        setSelected(null);
      });
    return () => {
      live = false;
    };
  }, [p, selected, docFilter]);

  // Escape closes the side panel.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setSelected(null);
        setRelation(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Frame the whole graph once the layout settles (and again after a resize or reload).
  // Entities without any visible link drift away from the rest: framing them would shrink the map.
  const linkedRef = useRef<Set<string>>(new Set());

  const fit = useCallback(() => {
    const linked = linkedRef.current;
    fg.current?.zoomToFit(500, 90, (n) => linked.size === 0 || linked.has(String(n.id)));
    // A handful of entities should not fill the screen.
    setTimeout(() => {
      const z = fg.current?.zoom() ?? 1;
      if (z > 3) fg.current?.zoom(3, 300);
    }, 550);
  }, []);
  // `load` is declared before `fit`: it reaches it through this ref.
  const fitRef = useRef(fit);
  fitRef.current = fit;
  useEffect(() => {
    fitted.current = false;
    const t = setTimeout(() => {
      if (!fitted.current) {
        fitted.current = true;
        fit();
      }
    }, 1500);
    return () => clearTimeout(t);
  }, [size.width, size.height, data.nodes.length, fit]);

  const visible = useMemo(() => {
    const nodes = data.nodes.filter((n) => !hidden.has(n.label));
    const ids = new Set(nodes.map((n) => String(n.id)));
    const links = data.links.filter((l) => ids.has(endId(l.source)) && ids.has(endId(l.target)));
    return { nodes, links };
  }, [data, hidden]);
  linkedRef.current = useMemo(() => {
    const ids = new Set<string>();
    for (const l of visible.links) {
      ids.add(endId(l.source));
      ids.add(endId(l.target));
    }
    return ids;
  }, [visible.links]);

  const neighbours = useMemo(() => {
    const set = new Set<string>();
    if (!selected) return set;
    for (const l of data.links) {
      const s = endId(l.source);
      const t = endId(l.target);
      if (s === selected) set.add(t);
      if (t === selected) set.add(s);
    }
    return set;
  }, [data.links, selected]);

  // Canvas cannot read CSS variables: resolve them once per theme.
  const colors = useMemo(() => {
    const cache = new Map<string, string>();
    return {
      ink: cssVar("--ink") || "#1e2a33",
      paper: cssVar("--paper") || "#f2f4ef",
      label: (label: string) => {
        let c = cache.get(label);
        if (!c) {
          c = labelColor(label);
          cache.set(label, c);
        }
        return c;
      },
      themeTick,
    };
  }, [themeTick]);
  const labelRank = useMemo(() => {
    const sorted = [...data.nodes].sort((a, b) => b.mentions - a.mentions);
    return new Map(sorted.map((n, i) => [String(n.id), i]));
  }, [data.nodes]);

  // Spread dense graphs: stronger repulsion and longer links as the graph grows.
  useEffect(() => {
    const g = fg.current;
    if (!g) return;
    const n = visible.nodes.length;
    (g.d3Force("charge") as unknown as { strength?: (v: number) => void })?.strength?.(-40 - Math.min(160, n));
    (g.d3Force("link") as unknown as { distance?: (v: number) => void })?.distance?.(30 + Math.min(70, n / 3));
    g.d3ReheatSimulation();
  }, [visible.nodes.length]);

  const maxWeight = useMemo(() => Math.max(1, ...data.links.map((l) => l.weight)), [data.links]);
  const linkWidth = (l: GLink) => 0.8 + 4 * Math.sqrt(l.strength ?? l.weight / maxWeight);

  const focus = (id: string) => {
    const node = data.nodes.find((n) => n.id === id);
    if (node && node.x !== undefined && node.y !== undefined) {
      fg.current?.centerAt(node.x, node.y, 600);
      fg.current?.zoom(2.4, 600);
    }
  };

  const select = (id: string) => {
    setSelected(id);
    if (!data.nodes.some((n) => n.id === id)) expand(id);
  };

  const onFind = async (e: React.FormEvent) => {
    e.preventDefault();
    const q = find.trim().toLowerCase();
    if (!q) return;
    const local = data.nodes.find((n) => n.name.toLowerCase() === q) ?? data.nodes.find((n) => n.name.toLowerCase().includes(q));
    if (local) {
      setSelected(String(local.id));
      focus(String(local.id));
      return;
    }
    try {
      const [hit] = await p.entities({ q, limit: 1 });
      if (hit) {
        setSelected(hit.id);
        await expand(hit.id);
      } else setError(translate("map.noMatch", { q: find }));
    } catch (err) {
      setError((err as Error).message);
    }
  };

  const drawNode = (node: GNode, ctx: CanvasRenderingContext2D, scale: number) => {
    const id = String(node.id);
    const r = radius(node.mentions, scale);
    const isSel = id === selected;
    const near = neighbours.has(id);
    const dim = selected !== null && !isSel && !near;
    const color = colors.label(node.label);
    const x = node.x ?? 0;
    const y = node.y ?? 0;
    ctx.globalAlpha = dim ? 0.3 : 1;
    if (isSel) {
      ctx.beginPath();
      ctx.arc(x, y, r + 7 / Math.sqrt(Math.max(scale, 1)), 0, 2 * Math.PI);
      ctx.strokeStyle = colors.ink;
      ctx.globalAlpha = 0.5;
      ctx.lineWidth = 1.5 / scale;
      ctx.stroke();
      ctx.globalAlpha = 1;
    }
    ctx.beginPath();
    ctx.arc(x, y, r, 0, 2 * Math.PI);
    if (node.published === 0) {
      ctx.fillStyle = colors.paper;
      ctx.fill();
      ctx.setLineDash([4 / scale, 3 / scale]);
      ctx.strokeStyle = color;
      ctx.lineWidth = 2.5 / scale;
      ctx.stroke();
      ctx.setLineDash([]);
    } else {
      ctx.fillStyle = color;
      ctx.fill();
      if (isSel) {
        ctx.strokeStyle = colors.paper;
        ctx.lineWidth = 3 / scale;
        ctx.stroke();
      }
    }
    // Labels for the most cited entities first, more as one zooms in.
    const rank = labelRank.get(id) ?? Infinity;
    if (isSel || near || rank < 25 * scale * scale) {
      const size = (isSel ? 18 : 14) / Math.max(scale, 0.6);
      ctx.font = `${isSel ? 700 : 600} ${size}px "Barlow Semi Condensed", sans-serif`;
      ctx.textBaseline = "middle";
      ctx.lineJoin = "round";
      ctx.lineWidth = (isSel ? 5 : 4) / scale;
      ctx.strokeStyle = colors.paper;
      ctx.strokeText(node.name, x + r + 5 / scale, y);
      ctx.fillStyle = colors.ink;
      ctx.fillText(node.name, x + r + 5 / scale, y);
    }
    ctx.globalAlpha = 1;
  };

  const legend = stats?.labels ?? [];

  return (
    <>
      <section className="map" aria-label={t("map.aria")} ref={boxRef}>
        <svg className="map-relief" aria-hidden="true" preserveAspectRatio="xMidYMid slice" viewBox="0 0 900 900">
          <filter id="relief" x="0" y="0" width="100%" height="100%">
            <feTurbulence type="fractalNoise" baseFrequency="0.0042" numOctaves="3" seed="11" />
            <feColorMatrix type="matrix" values="0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  1.4 0 0 0 -0.2" />
            <feComponentTransfer result="bands">
              <feFuncA type="discrete" tableValues="0 0 0 0.6 0 0 0 0.6 0 0 0 0.6 0 0 0 0.6 0 0 0 0.6 0 0 0 0.6 0 0 0 0.6 0 0 0 0.6 0 0 0 0.6" />
            </feComponentTransfer>
            <feFlood style={{ floodColor: "var(--relief)" }} />
            <feComposite in2="bands" operator="in" />
          </filter>
          <rect width="900" height="900" filter="url(#relief)" />
        </svg>

        <div className="map-canvas">
          <ForceGraph2D<NodeData, { weight: number; strength: number }>
            ref={fg}
            width={size.width}
            height={size.height}
            graphData={visible}
            backgroundColor="rgba(0,0,0,0)"
            nodeLabel={(n) => t("map.nodeTip", { name: n.name, label: labelName(n.label), passages: t("common.passages", { n: n.mentions }) })}
            nodeCanvasObject={drawNode}
            nodePointerAreaPaint={(node, color, ctx, scale) => {
              const r = radius(node.mentions, scale) + 4 / scale;
              ctx.fillStyle = color;
              ctx.beginPath();
              ctx.arc(node.x ?? 0, node.y ?? 0, r, 0, 2 * Math.PI);
              ctx.fill();
            }}
            // Thickness follows the link's strength (specificity), not its raw count.
            linkWidth={(l) => linkWidth(l)}
            // Color and thickness both show the link's strength: grey-blue (weak) to amber to red (strong).
            linkCanvasObjectMode={() => "replace"}
            linkCanvasObject={(l, ctx, scale) => {
              const s = l.source as GNode;
              const t = l.target as GNode;
              if (s?.x === undefined || s.y === undefined || t?.x === undefined || t.y === undefined) return;
              const touches = selected !== null && (String(s.id) === selected || String(t.id) === selected);
              const strength = l.strength ?? l.weight / maxWeight;
              const alpha = selected === null ? 0.45 + 0.45 * Math.min(1, Math.sqrt(strength / STRENGTH_FULL)) : touches ? 0.95 : 0.07;
              ctx.strokeStyle = strengthColor(strength, alpha);
              ctx.lineWidth = linkWidth(l) / scale;
              ctx.lineCap = "round";
              ctx.beginPath();
              ctx.moveTo(s.x, s.y);
              ctx.lineTo(t.x, t.y);
              ctx.stroke();
            }}
            linkLabel={(l) => {
              const a = data.nodes.find((n) => n.id === endId(l.source))?.name ?? endId(l.source);
              const b = data.nodes.find((n) => n.id === endId(l.target))?.name ?? endId(l.target);
              return t("map.linkTip", { a, b, shared: t("common.sharedPassages", { n: l.weight }), strength: strengthLabel(l.strength ?? 0) });
            }}
            onLinkClick={(l) => {
              setSelected(null);
              p.relation(endId(l.source), endId(l.target)).then(setRelation).catch((e) => setError((e as Error).message));
            }}
            cooldownTicks={150}
            d3VelocityDecay={0.35}
            maxZoom={6}
            onEngineStop={() => {
              if (!fitted.current) {
                fitted.current = true;
                fit();
              }
            }}
            onNodeClick={(node) => {
              setRelation(null);
              const id = String(node.id);
              const now = Date.now();
              if (lastClick.current.id === id && now - lastClick.current.at < 350) expand(id);
              lastClick.current = { id, at: now };
              setSelected(id);
            }}
            onBackgroundClick={() => {
              setSelected(null);
              setRelation(null);
            }}
          />
        </div>

        <div className="map-toolbar toolbar">
          <form className="field-box search-box" role="search" onSubmit={onFind}>
            <SearchIcon size={18} />
            <label htmlFor="find" className="sr-only">{t("map.find")}</label>
            <input
              id="find"
              type="search"
              placeholder={t("map.find")}
              value={find}
              list="entity-names"
              onChange={(e) => setFind(e.target.value)}
            />
            <datalist id="entity-names">
              {data.nodes.slice(0, 300).map((n) => <option key={String(n.id)} value={n.name} />)}
            </datalist>
          </form>
          {docs.length > 1 && (
            <label className="field-box map-doc">
              {t("map.document")}
              <select value={mapDoc} onChange={(e) => setMapDoc(e.target.value)}>
                <option value="">{t("map.allDocs")}</option>
                {docs.map((d) => <option key={d.id} value={d.id}>{d.title}</option>)}
              </select>
            </label>
          )}
          <label className="field-box">
            {t("map.minLink")}
            <select value={minWeight} onChange={(e) => setMinWeight(Number(e.target.value))}>
              {[1, 2, 3, 5].map((w) => <option key={w} value={w}>{t("common.passages", { n: w })}</option>)}
              {![1, 2, 3, 5].includes(minWeight) && <option value={minWeight}>{t("common.passages", { n: minWeight })}</option>}
            </select>
          </label>
          <label className="field-box">
            {t("map.entities")}
            <select value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
              {[50, 150, 400].map((n) => <option key={n} value={n}>{t("map.topCited", { n })}</option>)}
            </select>
          </label>
          <label className="field-box">
            <input type="checkbox" checked={drafts} onChange={(e) => setDrafts(e.target.checked)} />
            {t("map.showDrafts")}
          </label>
          <button type="button" className="btn" onClick={fit}>{t("map.showAll")}</button>
        </div>

        <aside className="cartouche" aria-label={t("map.summaryAria")}>
          <div className="cartouche-head">
            <strong>{project}</strong>
            <span className="muted" style={{ fontSize: 13 }}>{loading ? t("map.loading") : t("map.shown", { n: visible.nodes.length })}</span>
          </div>
          {mapDocTitle && <p className="map-doc-scope">{t("map.docScope", { doc: mapDocTitle })}</p>}
          {stats && (
            <>
              <dl>
                <div><dt>{t("map.documents")}</dt><dd>{num(stats.documents)}</dd></div>
                <div><dt>{t("map.passages")}</dt><dd>{num(stats.chunks)}</dd></div>
                <div><dt>{t("map.entities")}</dt><dd>{num(stats.entities)}</dd></div>
                <div><dt>{t("map.relations")}</dt><dd>{num(stats.relations)}</dd></div>
              </dl>
              <div className="muted" style={{ fontSize: 13 }}>
                {t("map.publishedDocs", { n: stats.published })}, {t("map.drafts", { n: stats.drafts })}
              </div>
            </>
          )}
          {legend.length > 0 && (
            <ul className="legend" aria-label={t("map.legendAria")}>
              {legend.map(({ label, count }) => (
                <li key={label}>
                  <button
                    type="button"
                    aria-pressed={!hidden.has(label)}
                    onClick={() =>
                      setHidden((h) => {
                        const next = new Set(h);
                        if (next.has(label)) next.delete(label);
                        else next.add(label);
                        return next;
                      })
                    }
                  >
                    <span className="dot" style={{ background: labelVar(label), width: 12, height: 12 }} />
                    {labelName(label)}
                    <span className="count">{num(count)}</span>
                  </button>
                </li>
              ))}
              <li className="legend-draft"><span className="dot draft" />{t("map.draftOnly")}</li>
            </ul>
          )}
          <figure className="legend-strength" aria-label={t("map.strengthAria")}>
            <figcaption>{t("map.strengthTitle")}</figcaption>
            <span className="legend-ramp" style={{ background: `linear-gradient(to right, ${STRENGTH_RAMP.join(", ")})` }} />
            <span className="legend-ramp-labels">
              <span>{t("map.weak")}</span>
              <span>{num2(STRENGTH_FULL / 4)}</span>
              <span>{t("map.andMore", { n: num2(STRENGTH_FULL) })}</span>
            </span>
            <span className="muted">{t("map.strengthHint")}</span>
          </figure>
          {autoWeight && minWeight === autoWeight && (
            <p className="muted" style={{ fontSize: 13 }}>
              {t("map.dense", { n: autoWeight })}{" "}
              <button type="button" className="link-button" onClick={() => { setAutoWeight(null); setMinWeight(1); }}>{t("map.showAll")}</button>
            </p>
          )}
          {error && <div className="error-banner" role="alert">{error}</div>}
        </aside>

        {!loading && stats && stats.entities === 0 && (
          <div className="empty" style={{ position: "absolute", inset: 0, justifyContent: "center", pointerEvents: "none" }}>
            <p>{t("map.empty")}</p>
            <a className="btn btn-primary" href={href("documents", { new: "1" })} style={{ pointerEvents: "auto" }}>{t("common.addDocument")}</a>
          </div>
        )}
      </section>

      {relation && !selected && (
        <aside className="side" aria-label={t("map.relationAria")}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 12 }}>
            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <span className="muted" style={{ fontSize: 14 }}>{t("map.whyLink")}</span>
              <h1 className="side-title" style={{ fontSize: 26 }}>
                <button type="button" className="link-button" style={{ color: "inherit" }} onClick={() => { setRelation(null); setSelected(relation.a.id); }}>{relation.a.name}</button>
                {" — "}
                <button type="button" className="link-button" style={{ color: "inherit" }} onClick={() => { setRelation(null); setSelected(relation.b.id); }}>{relation.b.name}</button>
              </h1>
            </div>
            <button type="button" className="icon-button" aria-label={t("common.closePanel")} onClick={() => setRelation(null)}>
              <CloseIcon />
            </button>
          </div>
          <dl className="relation-figures">
            <div><dt>{t("map.sharedLabel")}</dt><dd>{num(relation.weight)}</dd></div>
            <div><dt>{t("map.strengthLabel")}</dt><dd>{strengthLabel(relation.strength)}</dd></div>
          </dl>
          <p className="muted" style={{ fontSize: 14 }}>
            {t("map.strengthExplain", { a: relation.a.name, na: relation.a.mentions, b: relation.b.name, nb: relation.b.mentions })}
          </p>
          <section style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <h3>{t("map.citedTogether")}</h3>
            {relation.passages.map((ps) => (
              <article key={ps.chunk_id} className="passage">
                <div className="passage-head">
                  <a href={href("lire", ps.page ? { doc: ps.doc_id, page: String(ps.page) } : { doc: ps.doc_id, tab: "passages" })} style={{ fontWeight: 600 }}>
                    {ps.doc_title}, {ps.page ? t("common.page", { n: String(ps.page) }) : t("common.passage", { n: String(ps.idx + 1) })}
                  </a>
                  <span className={`status status-${ps.doc_status}`}>{statusLabel(ps.doc_status)}</span>
                </div>
                <p>
                  {splitHighlights(ps.text, [relation.a.name, relation.b.name]).map((part, i) =>
                    part.hit ? <mark key={i}>{part.text}</mark> : <span key={i}>{part.text}</span>,
                  )}
                </p>
              </article>
            ))}
            {relation.weight > relation.passages.length && (
              <p className="muted" style={{ fontSize: 14 }}>{t("map.firstOf", { n: relation.passages.length, total: relation.weight })}</p>
            )}
          </section>
        </aside>
      )}

      {selected && (
        <aside className="side" aria-label={t("map.entityAria")}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 12 }}>
            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <h1 className="side-title" style={{ fontSize: 30 }} title={detail?.name}>{detail?.name ?? "…"}</h1>
              {detail && (
                <div className="muted" style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 14 }}>
                  <span className={`dot${detail.published_mentions === 0 ? " draft" : ""}`} style={{ background: labelVar(detail.label) }} />
                  {t("map.citedIn", { label: labelName(detail.label), passages: t("common.passages", { n: detail.mentions }) })}
                  {detail.published_mentions === 0 && t("map.draftsOnly")}
                </div>
              )}
            </div>
            <button type="button" className="icon-button" aria-label={t("common.closePanel")} onClick={() => setSelected(null)}>
              <CloseIcon />
            </button>
          </div>
          <div className="toolbar">
            <button type="button" className="btn btn-primary" onClick={() => expand(selected)}>{t("map.expand")}</button>
            <button type="button" className="btn" onClick={() => focus(selected)}>{t("map.center")}</button>
          </div>
          {detail && detail.documents.length > 0 && (
            <section className="entity-docs" aria-labelledby="entity-docs-title">
              <h3 id="entity-docs-title">{t("map.inDocuments")}</h3>
              <ul>
                {detail.documents.map((d) => {
                  const kind = docKind(d.source);
                  const share = Math.round((100 * d.passages) / Math.max(1, detail.mentions));
                  const active = docFilter === d.doc_id;
                  return (
                    <li key={d.doc_id} className={active ? "active" : ""}>
                      <DocTypeIcon kind={kind} {...docKindInfo(kind)} />
                      <div className="entity-doc-main">
                        <a
                          className="entity-doc-title"
                          href={href("lire", d.pages[0] ? { doc: d.doc_id, page: String(d.pages[0]) } : { doc: d.doc_id })}
                          title={t("map.openDoc")}
                        >
                          {d.title}
                        </a>
                        <span className="entity-doc-bar" aria-hidden="true"><span style={{ width: `${share}%` }} /></span>
                        <span className="entity-doc-meta">
                          {t("map.docShare", { passages: t("common.passages", { n: d.passages }), share })}
                          {d.pages.length > 0 && (
                            <>
                              {", "}
                              {t("map.pagesList", { pages: "" })}
                              {d.pages.slice(0, 8).map((pg, i) => (
                                <span key={pg}>
                                  {i > 0 && ", "}
                                  <a href={href("lire", { doc: d.doc_id, page: String(pg) })}>{pg}</a>
                                </span>
                              ))}
                              {d.pages.length > 8 && "…"}
                            </>
                          )}
                          {d.status === "DRAFT" && ` (${statusLabel("DRAFT").toLowerCase()})`}
                        </span>
                      </div>
                      <button
                        type="button"
                        className={`btn entity-doc-filter${active ? " btn-primary" : ""}`}
                        aria-pressed={active}
                        onClick={() => setDocFilter(active ? null : d.doc_id)}
                      >
                        {active ? t("map.filtered") : t("map.filterDoc")}
                      </button>
                    </li>
                  );
                })}
              </ul>
            </section>
          )}
          {detail && detail.neighbours.length > 0 && (
            <section style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <h3>{t("map.appearsWith")}</h3>
              <ul className="neighbours">
                {detail.neighbours.slice(0, allNeighbours ? 50 : 10).map((n) => (
                  <li key={n.id}>
                    <span className="dot" style={{ background: labelVar(n.label) }} />
                    <button type="button" onClick={() => select(n.id)}>{n.name}</button>
                    <button
                      type="button"
                      className="link-score"
                      title={t("map.seeTogether")}
                      onClick={() => p.relation(selected, n.id).then((r) => { setSelected(null); setRelation(r); })}
                    >
                      {t("map.linkScore", { passages: t("common.passages", { n: n.weight }), strength: strengthLabel(n.strength) })}
                    </button>
                  </li>
                ))}
              </ul>
              {!allNeighbours && detail.neighbours.length > 10 && (
                <button type="button" className="btn" style={{ alignSelf: "flex-start" }} onClick={() => setAllNeighbours(true)}>
                  {t("map.moreNeighbours", { n: Math.min(50, detail.neighbours.length) - 10 })}
                </button>
              )}
            </section>
          )}
          {detail && (detail.images?.length ?? 0) > 0 && (
            <section style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <h3>{t("figures.title")}</h3>
              <Figures project={project} images={detail.images ?? []} />
            </section>
          )}
          {detail && detail.passages.length > 0 && (
            <section style={{ display: "flex", flexDirection: "column", gap: 12 }}>
              <div className="toolbar" style={{ justifyContent: "space-between" }}>
                <h3>
                  {docFilter
                    ? t("map.citingIn", { doc: detail.documents.find((d) => d.doc_id === docFilter)?.title ?? "" })
                    : t("map.citing")}
                </h3>
                {docFilter && <button type="button" className="btn" onClick={() => setDocFilter(null)}>{t("map.allDocs")}</button>}
              </div>
              {detail.passages.map((ps) => (
                <article key={ps.chunk_id} className="passage">
                  <div className="passage-head">
                    <a
                      href={href("lire", ps.page ? { doc: ps.doc_id, page: String(ps.page) } : { doc: ps.doc_id, tab: "passages" })}
                      style={{ fontWeight: 600 }}
                    >
                      {ps.doc_title}, {ps.page ? t("common.page", { n: String(ps.page) }) : t("common.passage", { n: String(ps.idx + 1) })}
                    </a>
                    <span className={`status status-${ps.doc_status}`}>{statusLabel(ps.doc_status)}</span>
                  </div>
                  <p>
                    {splitHighlights(ps.text, [detail.name]).map((part, i) =>
                      part.hit ? <mark key={i}>{part.text}</mark> : <span key={i}>{part.text}</span>,
                    )}
                  </p>
                </article>
              ))}
            </section>
          )}
        </aside>
      )}
    </>
  );
}
