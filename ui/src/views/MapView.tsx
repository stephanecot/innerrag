import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import ForceGraph2D, { type ForceGraphMethods, type LinkObject, type NodeObject } from "react-force-graph-2d";
import { api, type EntityDetail, type GraphView, type RelationDetail, type Stats } from "../api";
import { href } from "../App";
import { CloseIcon, SearchIcon } from "../Icons";
import { cssVar, labelColor, labelName, labelVar, num, plural, splitHighlights, STATUS_LABEL, strengthLabel, withAlpha } from "../util";

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
  const [hidden, setHidden] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<string | null>(params.get("entity"));
  const [detail, setDetail] = useState<EntityDetail | null>(null);
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

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [s, g] = await Promise.all([p.stats(), p.graph({ limit, min_weight: minWeight, include_drafts: drafts })]);
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
      setError("");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setLoading(false);
    }
  }, [p, limit, minWeight, drafts, merge]);

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
    if (!selected) {
      setDetail(null);
      return;
    }
    let live = true;
    p.entity(selected)
      .then((d) => live && setDetail(d))
      .catch((e) => live && setError((e as Error).message));
    return () => {
      live = false;
    };
  }, [p, selected]);

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
  const fit = useCallback(() => {
    fg.current?.zoomToFit(500, 90);
    // A handful of entities should not fill the screen.
    setTimeout(() => {
      const z = fg.current?.zoom() ?? 1;
      if (z > 3) fg.current?.zoom(3, 300);
    }, 550);
  }, []);
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
      } else setError(`Aucune entité ne contient « ${find} ».`);
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
      <section className="map" aria-label="Carte des entités" ref={boxRef}>
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
            nodeLabel={(n) => `${n.name} (${labelName(n.label)}, ${plural(n.mentions, "passage", "passages")})`}
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
            // Each link blends the colors of its two ends; strong links are more opaque.
            linkCanvasObjectMode={() => "replace"}
            linkCanvasObject={(l, ctx, scale) => {
              const s = l.source as GNode;
              const t = l.target as GNode;
              if (s?.x === undefined || s.y === undefined || t?.x === undefined || t.y === undefined) return;
              const touches = selected !== null && (String(s.id) === selected || String(t.id) === selected);
              const force = Math.sqrt(l.strength ?? l.weight / maxWeight);
              const alpha = selected === null ? 0.3 + 0.55 * force : touches ? 0.9 : 0.07;
              const gradient = ctx.createLinearGradient(s.x, s.y, t.x, t.y);
              gradient.addColorStop(0, withAlpha(colors.label(s.label), alpha));
              gradient.addColorStop(1, withAlpha(colors.label(t.label), alpha));
              ctx.strokeStyle = gradient;
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
              return `${a} — ${b} : ${plural(l.weight, "passage en commun", "passages en commun")}, force ${strengthLabel(l.strength ?? 0)}`;
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
            <label htmlFor="find" className="sr-only">Trouver une entité</label>
            <input
              id="find"
              type="search"
              placeholder="Trouver une entité"
              value={find}
              list="entity-names"
              onChange={(e) => setFind(e.target.value)}
            />
            <datalist id="entity-names">
              {data.nodes.slice(0, 300).map((n) => <option key={String(n.id)} value={n.name} />)}
            </datalist>
          </form>
          <label className="field-box">
            Lien minimum
            <select value={minWeight} onChange={(e) => setMinWeight(Number(e.target.value))}>
              <option value={1}>1 passage</option>
              <option value={2}>2 passages</option>
              <option value={3}>3 passages</option>
              <option value={5}>5 passages</option>
              {![1, 2, 3, 5].includes(minWeight) && <option value={minWeight}>{minWeight} passages</option>}
            </select>
          </label>
          <label className="field-box">
            Entités
            <select value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
              <option value={50}>50 plus citées</option>
              <option value={150}>150 plus citées</option>
              <option value={400}>400 plus citées</option>
            </select>
          </label>
          <label className="field-box">
            <input type="checkbox" checked={drafts} onChange={(e) => setDrafts(e.target.checked)} />
            Afficher les brouillons
          </label>
          <button type="button" className="btn" onClick={fit}>Tout afficher</button>
        </div>

        <aside className="cartouche" aria-label="Cartouche du projet">
          <div className="cartouche-head">
            <strong>{project}</strong>
            <span className="muted" style={{ fontSize: 13 }}>{loading ? "chargement…" : `${visible.nodes.length} affichées`}</span>
          </div>
          {stats && (
            <>
              <dl>
                <div><dt>Documents</dt><dd>{num(stats.documents)}</dd></div>
                <div><dt>Passages</dt><dd>{num(stats.chunks)}</dd></div>
                <div><dt>Entités</dt><dd>{num(stats.entities)}</dd></div>
                <div><dt>Relations</dt><dd>{num(stats.relations)}</dd></div>
              </dl>
              <div className="muted" style={{ fontSize: 13 }}>
                {plural(stats.published, "document publié", "documents publiés")}, {plural(stats.drafts, "brouillon", "brouillons")}
              </div>
            </>
          )}
          {legend.length > 0 && (
            <ul className="legend" aria-label="Types d'entités : cliquez pour masquer ou afficher">
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
              <li className="legend-draft"><span className="dot draft" />Cité seulement dans des brouillons</li>
            </ul>
          )}
          {autoWeight && minWeight === autoWeight && (
            <p className="muted" style={{ fontSize: 13 }}>
              Graphe dense : liens de moins de {autoWeight} passages en commun masqués.{" "}
              <button type="button" className="link-button" onClick={() => { setAutoWeight(null); setMinWeight(1); }}>Tout afficher</button>
            </p>
          )}
          {error && <div className="error-banner" role="alert">{error}</div>}
        </aside>

        {!loading && stats && stats.entities === 0 && (
          <div className="empty" style={{ position: "absolute", inset: 0, justifyContent: "center", pointerEvents: "none" }}>
            <p>La carte est vide. Ajoutez un document : ses entités et leurs liens apparaîtront ici.</p>
            <a className="btn btn-primary" href={href("documents", { new: "1" })} style={{ pointerEvents: "auto" }}>Ajouter un document</a>
          </div>
        )}
      </section>

      {relation && !selected && (
        <aside className="side" aria-label="Lien entre deux entités">
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 12 }}>
            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <span className="muted" style={{ fontSize: 14 }}>Pourquoi ce lien ?</span>
              <h1 className="side-title" style={{ fontSize: 26 }}>
                <button type="button" className="link-button" style={{ color: "inherit" }} onClick={() => { setRelation(null); setSelected(relation.a.id); }}>{relation.a.name}</button>
                {" — "}
                <button type="button" className="link-button" style={{ color: "inherit" }} onClick={() => { setRelation(null); setSelected(relation.b.id); }}>{relation.b.name}</button>
              </h1>
            </div>
            <button type="button" className="icon-button" aria-label="Fermer le panneau" onClick={() => setRelation(null)}>
              <CloseIcon />
            </button>
          </div>
          <dl className="relation-figures">
            <div><dt>passages en commun</dt><dd>{num(relation.weight)}</dd></div>
            <div><dt>force du lien</dt><dd>{strengthLabel(relation.strength)}</dd></div>
          </dl>
          <p className="muted" style={{ fontSize: 14 }}>
            La force est la part des passages citant {relation.a.name} ({num(relation.a.mentions)}) ou {relation.b.name} ({num(relation.b.mentions)})
            qui citent les deux. Proche de 1 : elles vont toujours ensemble. Proche de 0 : l'une est citée partout, le lien est peu spécifique.
          </p>
          <section style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <h3>Passages qui les citent ensemble</h3>
            {relation.passages.map((ps) => (
              <article key={ps.chunk_id} className="passage">
                <div className="passage-head">
                  <a href={href("lire", ps.page ? { doc: ps.doc_id, page: String(ps.page) } : { doc: ps.doc_id, tab: "passages" })} style={{ fontWeight: 600 }}>
                    {ps.doc_title}, {ps.page ? `page ${ps.page}` : `passage ${ps.idx + 1}`}
                  </a>
                  <span className={`status status-${ps.doc_status}`}>{STATUS_LABEL[ps.doc_status]}</span>
                </div>
                <p>
                  {splitHighlights(ps.text, [relation.a.name, relation.b.name]).map((part, i) =>
                    part.hit ? <mark key={i}>{part.text}</mark> : <span key={i}>{part.text}</span>,
                  )}
                </p>
              </article>
            ))}
            {relation.weight > relation.passages.length && (
              <p className="muted" style={{ fontSize: 14 }}>Les {relation.passages.length} premiers sur {num(relation.weight)}.</p>
            )}
          </section>
        </aside>
      )}

      {selected && (
        <aside className="side" aria-label="Entité sélectionnée">
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 12 }}>
            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <h1 className="side-title" style={{ fontSize: 30 }} title={detail?.name}>{detail?.name ?? "…"}</h1>
              {detail && (
                <div className="muted" style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 14 }}>
                  <span className={`dot${detail.published_mentions === 0 ? " draft" : ""}`} style={{ background: labelVar(detail.label) }} />
                  {labelName(detail.label)}, citée dans {plural(detail.mentions, "passage", "passages")}
                  {detail.published_mentions === 0 && " (brouillons uniquement)"}
                </div>
              )}
            </div>
            <button type="button" className="icon-button" aria-label="Fermer le panneau" onClick={() => setSelected(null)}>
              <CloseIcon />
            </button>
          </div>
          <div className="toolbar">
            <button type="button" className="btn btn-primary" onClick={() => expand(selected)}>Déplier le voisinage</button>
            <button type="button" className="btn" onClick={() => focus(selected)}>Centrer sur la carte</button>
          </div>
          {detail && detail.neighbours.length > 0 && (
            <section style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <h3>Apparaît avec</h3>
              <ul className="neighbours">
                {detail.neighbours.slice(0, 20).map((n) => (
                  <li key={n.id}>
                    <span className="dot" style={{ background: labelVar(n.label) }} />
                    <button type="button" onClick={() => select(n.id)}>{n.name}</button>
                    <button
                      type="button"
                      className="link-score"
                      title="Voir les passages qui les citent ensemble"
                      onClick={() => p.relation(selected, n.id).then((r) => { setSelected(null); setRelation(r); })}
                    >
                      {plural(n.weight, "passage", "passages")}, force {strengthLabel(n.strength)}
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          )}
          {detail && detail.passages.length > 0 && (
            <section style={{ display: "flex", flexDirection: "column", gap: 12 }}>
              <h3>Passages qui la citent</h3>
              {detail.passages.map((ps) => (
                <article key={ps.chunk_id} className="passage">
                  <div className="passage-head">
                    <a
                      href={href("lire", ps.page ? { doc: ps.doc_id, page: String(ps.page) } : { doc: ps.doc_id, tab: "passages" })}
                      style={{ fontWeight: 600 }}
                    >
                      {ps.doc_title}, {ps.page ? `page ${ps.page}` : `passage ${ps.idx + 1}`}
                    </a>
                    <span className={`status status-${ps.doc_status}`}>{STATUS_LABEL[ps.doc_status]}</span>
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
