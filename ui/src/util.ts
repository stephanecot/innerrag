// Labels, colors and formatting shared by the views.
import { locale, num, num2, translate, type Key } from "./i18n";

export { num, num2 };

const LABELS = new Set(["person", "organization", "location", "event", "product", "technology"]);

export const labelName = (label: string) =>
  LABELS.has(label) ? translate(`entity.${label}` as Key) : label.charAt(0).toUpperCase() + label.slice(1);

/** CSS variable holding the legend color of an entity label. */
export const labelVar = (label: string) => `var(--c-${LABELS.has(label) ? label : "other"})`;

/** Resolved color (for canvas drawing, which cannot read CSS variables). */
export function labelColor(label: string): string {
  const name = `--c-${LABELS.has(label) ? label : "other"}`;
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || "#6b6f73";
}

/** "#1e2a33" + 0.4 → "rgba(30, 42, 51, 0.4)" (canvas-safe). */
export function withAlpha(hex: string, alpha: number): string {
  const h = hex.replace("#", "");
  const full = h.length === 3 ? h.split("").map((c) => c + c).join("") : h;
  const n = parseInt(full, 16);
  if (Number.isNaN(n)) return hex;
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

export function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** LadybugDB timestamps come as "2026-10-06 20:38:49.321" (UTC). */
export function parseDbDate(s: string): Date | null {
  if (!s) return null;
  const d = new Date(s.replace(" ", "T") + (s.endsWith("Z") ? "" : "Z"));
  return Number.isNaN(d.getTime()) ? null : d;
}

export function relativeDate(input: string | number | Date | null): string {
  if (input === null || input === "") return "";
  const d = input instanceof Date ? input : typeof input === "number" ? new Date(input) : parseDbDate(input);
  if (!d) return String(input);
  const now = new Date();
  const time = d.toLocaleTimeString(locale(), { hour: "2-digit", minute: "2-digit" });
  if (d.toDateString() === now.toDateString()) return translate("date.today", { time });
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) return translate("date.yesterday", { time });
  return d.toLocaleDateString(locale(), { day: "numeric", month: "short", year: d.getFullYear() === now.getFullYear() ? undefined : "numeric" }) + ` ${time}`;
}

export function longDate(input: string): string {
  const d = parseDbDate(input) ?? new Date(input);
  return Number.isNaN(d.getTime()) ? input : d.toLocaleDateString(locale(), { day: "numeric", month: "long", year: "numeric" });
}

export function bytes(n: number): string {
  if (n < 1024) return translate("bytes.b", { n: String(n) });
  if (n < 1024 * 1024) return translate("bytes.kb", { n: Math.round(n / 1024) });
  return translate("bytes.mb", { n: (n / 1024 / 1024).toLocaleString(locale(), { maximumFractionDigits: 1 }) });
}

const STAGES = new Set(["queued", "extracting", "embedding", "entities", "writing", "done", "failed", "cancelled"]);

/** Label of an ingestion stage. */
export const jobStage = (stage: string) => (STAGES.has(stage) ? translate(`stage.${stage}` as Key) : stage);

/** Overall progress: vectors are quick, entities take most of the time. */
export function jobPercent(job: { stage: string; done: number; total: number }): number {
  const ratio = job.total ? job.done / job.total : 0;
  const span: Record<string, [number, number]> = {
    queued: [0, 0], extracting: [0, 2], embedding: [2, 25], entities: [25, 95], writing: [95, 99], done: [100, 100], failed: [100, 100], cancelled: [100, 100],
  };
  const [from, to] = span[job.stage] ?? [0, 0];
  return Math.round(from + (to - from) * ratio);
}

/** e5 similarities: relevant passages sit around 0.83–0.90, unrelated ones around 0.75–0.78. */
export function relevance(similarity: number): { label: string; tone: "strong" | "good" | "weak" } {
  if (similarity >= 0.86) return { label: translate("relevance.strong"), tone: "strong" };
  if (similarity >= 0.82) return { label: translate("relevance.good"), tone: "good" };
  return { label: translate("relevance.weak"), tone: "weak" };
}

export const statusLabel = (status: "PUBLISHED" | "DRAFT") => translate(`status.${status}`);

/** Journal operations, in the order of the History filter. */
export const OPERATIONS = ["search", "read", "cite", "ingest", "replace", "update", "delete", "explore", "cypher", "list_documents", "stats"];
const CHANNELS = new Set(["mcp", "rest", "ui", "watch"]);

/** Label of a journal operation, or the raw name when unknown. */
export const operationLabel = (op: string) => (OPERATIONS.includes(op) ? translate(`op.${op}` as Key) : op);
/** Label of a journal channel, or the raw name when unknown. */
export const channelLabel = (channel: string) => (CHANNELS.has(channel) ? translate(`channel.${channel}` as Key) : channel);

/** Highlights each occurrence of `terms` in `text` with <mark>. */
export function splitHighlights(text: string, terms: string[]): { text: string; hit: boolean }[] {
  const clean = terms.filter((t) => t.trim().length > 1).map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  if (!clean.length) return [{ text, hit: false }];
  // Whole words only ("app" must not light up "happening"); longest names first.
  // With a capturing group, split() puts the matches at odd indexes.
  clean.sort((a, b) => b.length - a.length);
  return text
    .split(new RegExp(`(?<![\\p{L}\\p{N}_])(${clean.join("|")})(?![\\p{L}\\p{N}_])`, "giu"))
    .map((part, i) => ({ text: part, hit: i % 2 === 1 }))
    .filter((p) => p.text);
}

/** A link strength: a tiny but non-zero value reads "< 0,01" rather than a misleading 0. */
export const strengthLabel = (n: number) => (n > 0 && n < 0.005 ? `< ${num2(0.01)}` : num2(n));

/** Link colors from weak to strong (Jaccard strength, saturating at STRENGTH_FULL). */
export const STRENGTH_RAMP = ["#9fb3bd", "#e3a33b", "#b42318"];
export const STRENGTH_FULL = 0.3;

/** Color of a link strength, interpolated along STRENGTH_RAMP on a square-root scale. */
export function strengthColor(strength: number, alpha = 1): string {
  const t = Math.min(1, Math.sqrt(Math.max(0, strength) / STRENGTH_FULL));
  const seg = t < 0.5 ? 0 : 1;
  const local = t < 0.5 ? t / 0.5 : (t - 0.5) / 0.5;
  const parse = (hex: string) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
  const a = parse(STRENGTH_RAMP[seg]);
  const b = parse(STRENGTH_RAMP[seg + 1]);
  const [r, g, bl] = a.map((v, i) => Math.round(v + (b[i] - v) * local));
  return `rgba(${r}, ${g}, ${bl}, ${alpha})`;
}

export type DocKind = "pdf" | "word" | "powerpoint" | "markdown" | "html" | "text";

/** Kind of a document, from the extension of its source file. */
export function docKind(source: string): DocKind {
  const ext = source.toLowerCase().split(/[?#]/)[0].split(".").pop() ?? "";
  if (ext === "pdf") return "pdf";
  if (ext === "docx" || ext === "doc" || ext === "odt" || ext === "rtf") return "word";
  if (ext === "pptx" || ext === "ppt" || ext === "odp") return "powerpoint";
  if (ext === "md" || ext === "markdown") return "markdown";
  if (ext === "html" || ext === "htm") return "html";
  return "text";
}

const DOC_BADGE: Record<DocKind, string> = { pdf: "PDF", word: "DOC", powerpoint: "PPT", markdown: "MD", html: "WEB", text: "TXT" };

/** Badge and readable name of a document kind (props of DocTypeIcon). */
export const docKindInfo = (kind: DocKind) => ({ badge: DOC_BADGE[kind], label: translate(`kind.${kind}`) });
