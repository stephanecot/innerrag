// Rich rendering of the assistant's answers: GitHub-flavoured Markdown, inline HTML and SVG, and
// ```html / ```svg / ```mermaid blocks shown as a preview with their source. Everything goes through
// DOMPurify: no script, no event handler, no form or frame can come out of an answer.
import DOMPurify, { type Config } from "dompurify";
import { marked, type Tokens } from "marked";
import { useEffect, useMemo, useRef, useState } from "react";
import { href } from "./App";
import { useT } from "./i18n";
import { imageUrl } from "./markdown";

export interface CitedDocument {
  id: string;
  title: string;
}

const escapeHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** Turns "Pro Android 5, page 486" into a link to the reader, outside code and existing links. */
function linkCitations(md: string, docs: CitedDocument[]): string {
  const titled = docs.filter((d) => d.title.trim().length >= 3).sort((a, b) => b.title.length - a.title.length);
  if (!titled.length) return md;
  const byTitle = new Map(titled.map((d) => [d.title.toLowerCase(), d.id]));
  const cite = new RegExp(`(${titled.map((d) => escapeRe(d.title)).join("|")})(\\*?,?\\s*(?:pages?|p\\.)\\s*(\\d+))?`, "gi");
  // Code fences, inline code and existing links are left untouched.
  const protectedParts = /(```[\s\S]*?```|`[^`\n]+`|\[[^\]]*\]\([^)]*\))/g;
  return md
    .split(protectedParts)
    .map((part, i) => {
      if (i % 2 === 1) return part;
      return part.replace(cite, (m, title: string, _rest: string, page?: string) => {
        const doc = byTitle.get(title.toLowerCase());
        if (!doc) return m;
        return `[${m}](${href("lire", page ? { doc, page } : { doc })})`;
      });
    })
    .join("");
}

const PURIFY: Config = {
  USE_PROFILES: { html: true, svg: true, svgFilters: true },
  FORBID_TAGS: ["style", "form", "input", "button", "textarea", "select", "iframe", "frame", "object", "embed", "link", "meta", "base"],
  ADD_ATTR: ["target"],
};

let hooked = false;
function purify(html: string): string {
  if (!hooked) {
    hooked = true;
    // External links open in a new tab, without access to this page.
    DOMPurify.addHook("afterSanitizeAttributes", (node) => {
      if (node.tagName === "A" && /^https?:/i.test(node.getAttribute("href") ?? "")) {
        node.setAttribute("target", "_blank");
        node.setAttribute("rel", "noopener noreferrer");
      }
    });
  }
  return String(DOMPurify.sanitize(html, PURIFY));
}

function renderMarkdown(md: string, labels: { preview: string; source: string }): string {
  const renderer = new marked.Renderer();
  renderer.code = ({ text, lang }: Tokens.Code) => {
    const language = (lang ?? "").trim().split(/\s+/)[0].toLowerCase();
    const source = `<pre class="rich-code"><code>${escapeHtml(text)}</code></pre>`;
    const sourceBox = `<details class="rich-source"><summary>${labels.source}</summary>${source}</details>`;
    // Agents also label an SVG drawing `xml`, or not at all.
    const svg = language === "svg" || ((language === "xml" || !language) && /^\s*(<\?xml[^>]*>\s*)?<svg[\s>]/i.test(text));
    if (svg || language === "html") {
      // The preview is sanitized again with the whole answer; the source stays one click away.
      return `<figure class="rich-preview" aria-label="${labels.preview}">${text}</figure>${sourceBox}`;
    }
    if (language === "mermaid") {
      // Drawn once mounted (see MermaidDiagrams); until then, and if it fails, the source shows.
      return `<figure class="rich-preview rich-mermaid" aria-label="${labels.preview}"><pre class="rich-mermaid-src">${escapeHtml(text)}</pre></figure>${sourceBox}`;
    }
    return language ? source.replace("<code>", `<code class="language-${escapeHtml(language)}">`) : source;
  };
  return marked.parse(md, { gfm: true, breaks: false, renderer, async: false }) as string;
}

let mermaidCount = 0;

/** Mermaid colors taken from the interface's own tokens, so a diagram reads in the light and the dark theme. */
function mermaidTheme() {
  const css = getComputedStyle(document.documentElement);
  const v = (name: string) => css.getPropertyValue(name).trim();
  return {
    fontFamily: v("--font"),
    background: v("--surface"),
    textColor: v("--ink"),
    titleColor: v("--ink"),
    lineColor: v("--ink-soft"),
    primaryColor: v("--mark"),
    primaryTextColor: v("--ink"),
    primaryBorderColor: v("--link"),
    secondaryColor: v("--sunken"),
    secondaryTextColor: v("--ink"),
    secondaryBorderColor: v("--rule"),
    tertiaryColor: v("--paper"),
    tertiaryTextColor: v("--ink"),
    tertiaryBorderColor: v("--rule"),
    mainBkg: v("--mark"),
    nodeBorder: v("--link"),
    clusterBkg: v("--paper"),
    clusterBorder: v("--rule"),
    edgeLabelBackground: v("--surface"),
    actorBkg: v("--mark"),
    actorBorder: v("--link"),
    actorTextColor: v("--ink"),
    actorLineColor: v("--ink-soft"),
    signalColor: v("--ink"),
    signalTextColor: v("--ink"),
    labelBoxBkgColor: v("--sunken"),
    labelBoxBorderColor: v("--rule"),
    labelTextColor: v("--ink"),
    loopTextColor: v("--ink"),
    noteBkgColor: v("--warn-bg"),
    noteTextColor: v("--warn-ink"),
    noteBorderColor: v("--rule"),
  };
}

/** Draws the ```mermaid blocks of a rendered answer (all of them again with `redraw`, after a theme
 *  change); the library is loaded on first use only. The source stays in the figure, hidden. */
async function drawMermaid(root: HTMLElement, failed: string, redraw = false) {
  const figures = [...root.querySelectorAll<HTMLElement>(redraw ? ".rich-mermaid" : ".rich-mermaid:not([data-drawn])")];
  if (!figures.length) return;
  const { default: mermaid } = await import("mermaid");
  // strict: labels are sanitized and click handlers disabled; SVG text labels, no HTML inside the drawing.
  mermaid.initialize({ startOnLoad: false, securityLevel: "strict", theme: "base", themeVariables: mermaidTheme(), htmlLabels: false, flowchart: { htmlLabels: false } });
  for (const figure of figures) {
    figure.dataset.drawn = "1";
    const src = figure.querySelector(".rich-mermaid-src")?.textContent ?? "";
    try {
      const { svg } = await mermaid.render(`mermaid-${++mermaidCount}`, src);
      let box = figure.querySelector<HTMLElement>(".rich-mermaid-svg");
      if (!box) {
        box = document.createElement("div");
        box.className = "rich-mermaid-svg";
        figure.append(box);
      }
      box.innerHTML = String(DOMPurify.sanitize(svg, { USE_PROFILES: { svg: true, svgFilters: true }, ADD_TAGS: ["style"] }));
    } catch {
      if (!figure.classList.contains("failed")) {
        figure.classList.add("failed");
        figure.insertAdjacentHTML("afterbegin", `<p class="rich-mermaid-error">${escapeHtml(failed)}</p>`);
      }
    }
  }
}

const ICON_ZOOM = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M14 4h6v6M10 20H4v-6M20 4l-7 7M4 20l7-7"/></svg>';
const ICON_DOWNLOAD = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M12 4v11M7 10l5 5 5-5M5 20h14"/></svg>';

/** Adds "enlarge" and "download" buttons to the figures, diagrams and previews of a rendered answer. */
function decorate(root: HTMLElement, labels: { zoom: string; download: string }) {
  const button = (action: string, label: string, icon: string) =>
    `<button type="button" class="rich-action" data-action="${action}" aria-label="${escapeHtml(label)}" title="${escapeHtml(label)}">${icon}</button>`;
  for (const figure of root.querySelectorAll<HTMLElement>(".rich-preview:not([data-actions])")) {
    if (figure.classList.contains("rich-mermaid") && !figure.querySelector(".rich-mermaid-svg")) continue;
    figure.dataset.actions = "1";
    // An HTML mock-up can be enlarged; only a drawing is downloaded.
    const svg = !!figure.querySelector(":scope > svg, :scope > .rich-mermaid-svg > svg");
    figure.insertAdjacentHTML("afterbegin", `<div class="rich-actions">${button("zoom", labels.zoom, ICON_ZOOM)}${svg ? button("download", labels.download, ICON_DOWNLOAD) : ""}</div>`);
  }
  for (const img of root.querySelectorAll<HTMLImageElement>("img:not([data-actions])")) {
    img.dataset.actions = "1";
    if (img.closest("a, .rich-preview")) continue;
    const box = document.createElement("span");
    box.className = "rich-media";
    img.replaceWith(box);
    box.append(img);
    box.insertAdjacentHTML("beforeend", `<span class="rich-actions">${button("zoom", labels.zoom, ICON_ZOOM)}${button("download", labels.download, ICON_DOWNLOAD)}</span>`);
  }
}

/** A drawing as a standalone SVG file. */
function svgMarkup(svg: SVGSVGElement): string {
  const copy = svg.cloneNode(true) as SVGSVGElement;
  copy.setAttribute("xmlns", "http://www.w3.org/2000/svg");
  const box = svg.getBoundingClientRect();
  if (!copy.getAttribute("width") || copy.getAttribute("width")?.endsWith("%")) copy.setAttribute("width", String(Math.round(box.width)));
  if (!copy.getAttribute("height") || copy.getAttribute("height")?.endsWith("%")) copy.setAttribute("height", String(Math.round(box.height)));
  copy.style.removeProperty("max-width");
  return new XMLSerializer().serializeToString(copy);
}

/** The drawing rasterized at twice its size, on the page's background (for slides and documents). */
async function svgToPng(svg: SVGSVGElement): Promise<Blob> {
  const img = new Image();
  img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svgMarkup(svg))}`;
  await img.decode();
  const box = svg.getBoundingClientRect();
  const scale = 2;
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(box.width * scale));
  canvas.height = Math.max(1, Math.round(box.height * scale));
  const ctx = canvas.getContext("2d")!;
  ctx.fillStyle = getComputedStyle(document.documentElement).getPropertyValue("--surface").trim() || "#fff";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
  return new Promise((resolve, reject) => canvas.toBlob((b) => (b ? resolve(b) : reject(new Error("png"))), "image/png"));
}

function saveFile(url: string, name: string) {
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.append(a);
  a.click();
  a.remove();
}

function saveBlob(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  saveFile(url, name);
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

/** A file name from an image's caption, else the last part of its address. */
const imageName = (img: { src: string; alt: string }) => {
  const file = img.src.split("/").pop() ?? "image";
  const ext = /\.[a-z0-9]{3,4}$/i.exec(file)?.[0] ?? "";
  const caption = img.alt.replace(/[\\/:*?"<>|]+/g, " ").replace(/\s+/g, " ").trim().slice(0, 60).trim();
  return caption ? `${caption}${ext}` : file;
};

type Zoomed = { kind: "img"; src: string; alt: string } | { kind: "figure"; host: HTMLElement; svg: boolean };

/** A figure or diagram shown as large as the window allows, with its downloads. */
function Lightbox({ zoomed, onClose }: { zoomed: Zoomed; onClose: () => void }) {
  const t = useT();
  const dialog = useRef<HTMLDialogElement>(null);
  const body = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!dialog.current?.open) dialog.current?.showModal();
    if (zoomed.kind === "figure" && body.current) {
      const copy = zoomed.host.cloneNode(true) as HTMLElement;
      copy.querySelector(".rich-actions")?.remove();
      body.current.replaceChildren(...copy.childNodes);
    }
  }, [zoomed]);
  const svg = () => (zoomed.kind === "figure" ? zoomed.host.querySelector<SVGSVGElement>(":scope > svg, :scope > .rich-mermaid-svg > svg") : null);
  return (
    <dialog ref={dialog} className="lightbox" aria-label={t("rich.zoomed")} onClose={onClose} onClick={(e) => e.target === dialog.current && dialog.current?.close()}>
      <div className="lightbox-bar">
        {zoomed.kind === "img" ? (
          <button type="button" className="btn" onClick={() => saveFile(zoomed.src, imageName(zoomed))}>{t("rich.download")}</button>
        ) : zoomed.svg && (
          <>
            <button type="button" className="btn" onClick={() => { const s = svg(); if (s) saveBlob(new Blob([svgMarkup(s)], { type: "image/svg+xml" }), "diagram.svg"); }}>{t("rich.downloadSvg")}</button>
            <button type="button" className="btn" onClick={() => { const s = svg(); if (s) svgToPng(s).then((b) => saveBlob(b, "diagram.png")).catch(() => undefined); }}>{t("rich.downloadPng")}</button>
          </>
        )}
        <button type="button" className="btn" onClick={() => dialog.current?.close()} autoFocus>{t("rich.close")}</button>
      </div>
      <div ref={body} className={`lightbox-body${zoomed.kind === "figure" && !zoomed.host.classList.contains("rich-mermaid") ? " light" : ""}`}>
        {zoomed.kind === "img" && <img src={zoomed.src} alt={zoomed.alt} />}
      </div>
      {zoomed.kind === "img" && zoomed.alt && <p className="lightbox-caption">{zoomed.alt}</p>}
    </dialog>
  );
}

/** `streaming`: the text is still being written, so its diagrams are not drawn yet (half a diagram does not parse). */
export default function RichText({ text, docs = [], project, streaming = false }: { text: string; docs?: CitedDocument[]; project?: string; streaming?: boolean }) {
  const t = useT();
  const ref = useRef<HTMLDivElement>(null);
  const [zoomed, setZoomed] = useState<Zoomed | null>(null);
  const html = useMemo(() => {
    // Figures referenced the way documents store them are served by the project.
    const withImages = project ? text.replace(/innerrag-image:([0-9a-f]{16}\.(?:png|jpg|gif|webp))/g, (_, file: string) => imageUrl(project, `innerrag-image:${file}`)) : text;
    return purify(renderMarkdown(linkCitations(withImages, docs), { preview: t("rich.preview"), source: t("rich.source") }));
  }, [text, docs, t, project]);
  useEffect(() => {
    const root = ref.current;
    if (!root || streaming) return;
    let live = true;
    drawMermaid(root, t("rich.diagramFailed")).finally(() => {
      if (live) decorate(root, { zoom: t("rich.zoom"), download: t("rich.download") });
    });
    // Diagrams take the theme's colors: draw them again when it changes.
    const redraw = () => drawMermaid(root, t("rich.diagramFailed"), true);
    const watcher = new MutationObserver(redraw);
    watcher.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const scheme = window.matchMedia("(prefers-color-scheme: dark)");
    scheme.addEventListener("change", redraw);
    return () => {
      live = false;
      watcher.disconnect();
      scheme.removeEventListener("change", redraw);
    };
  }, [html, t, streaming]);

  // The buttons are added to the sanitized HTML after rendering, so clicks are caught here.
  const onClick = (e: React.MouseEvent) => {
    const target = e.target as HTMLElement;
    const action = target.closest<HTMLButtonElement>("button[data-action]");
    const img = target.closest<HTMLElement>(".rich-media")?.querySelector("img");
    if (img && (action || target === img)) {
      if (action?.dataset.action === "download") saveFile(img.src, imageName(img));
      else setZoomed({ kind: "img", src: img.src, alt: img.alt });
      return;
    }
    const figure = action?.closest<HTMLElement>(".rich-preview");
    if (!figure) return;
    const svg = figure.querySelector<SVGSVGElement>(":scope > svg, :scope > .rich-mermaid-svg > svg");
    if (action?.dataset.action === "download" && svg) {
      svgToPng(svg).then((b) => saveBlob(b, "diagram.png")).catch(() => saveBlob(new Blob([svgMarkup(svg)], { type: "image/svg+xml" }), "diagram.svg"));
    } else {
      setZoomed({ kind: "figure", host: figure, svg: !!svg });
    }
  };

  return (
    <>
      <div ref={ref} className="rich" onClick={onClick} dangerouslySetInnerHTML={{ __html: html }} />
      {zoomed && <Lightbox zoomed={zoomed} onClose={() => setZoomed(null)} />}
    </>
  );
}
