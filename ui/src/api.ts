// Typed client for the innerrag REST API.

export type Status = "DRAFT" | "PUBLISHED";

export interface Project {
  id: string;
  title: string;
  description: string;
  created_at: string;
  embedding_model: string;
  embedding_dim: number;
  size_bytes: number;
}

export interface Stats {
  documents: number;
  published: number;
  drafts: number;
  chunks: number;
  entities: number;
  mentions: number;
  relations: number;
  labels: { label: string; count: number }[];
}

export interface DocumentSummary {
  id: string;
  title: string;
  source: string;
  status: Status;
  creator: string;
  tags: string[];
  created_at: string;
  updated_at: string;
  chunks: number;
  entities: number;
}

export interface ChunkView {
  id: string;
  idx: number;
  text: string;
  entities: string[];
  page: number | null;
}

export interface PassagePage {
  total: number;
  offset: number;
  passages: ChunkView[];
}

export interface DocumentContent {
  id: string;
  title: string;
  content: string;
  metadata: unknown;
}

export interface DocumentWithChunks extends DocumentSummary {
  metadata: unknown;
  /** First passages only; see `passages()`. */
  passages: ChunkView[];
  passage_count: number;
}

export interface EntitySummary {
  id: string;
  name: string;
  label: string;
  mentions: number;
  published_mentions: number;
}

export interface EntityDetail extends EntitySummary {
  neighbours: (EntitySummary & { weight: number })[];
  passages: { chunk_id: string; doc_id: string; doc_title: string; doc_status: Status; idx: number; text: string; page: number | null }[];
}

export interface GraphView {
  nodes: EntitySummary[];
  edges: { source: string; target: string; weight: number }[];
}

export interface IngestReport {
  id: string;
  title: string;
  status: Status;
  tags: string[];
  replaced: boolean;
  chunks: number;
  entities: number;
  new_entities: number;
  relations: number;
  millis: number;
}

export type JobStage = "queued" | "extracting" | "embedding" | "entities" | "writing" | "done" | "failed" | "cancelled";

export interface Job {
  id: string;
  project: string;
  filename: string;
  document_id: string;
  stage: JobStage;
  done: number;
  total: number;
  created_at: number;
  finished_at: number | null;
  report: IngestReport | null;
  error: string | null;
}

export interface FileInfo {
  name: string;
  format: string;
  format_label: string;
  size: number;
  pages: number | null;
  /** Set when the original file is kept on the server. */
  stored?: string;
}

export interface SearchResponse {
  query: string;
  chunks: {
    id: string;
    doc_id: string;
    doc_title: string;
    doc_status: Status;
    idx: number;
    page: number | null;
    text: string;
    score: number;
    similarity: number;
    graph_score: number | null;
    graph_boost: number;
    via_graph: boolean;
    entities: string[];
  }[];
  entities: { id: string; name: string; label: string; score: number; via: "query" | "vector" | "neighbour" }[];
  relations: { source: string; target: string; weight: number }[];
  context: string;
  min_score: number;
  below_threshold: number;
  best_similarity: number | null;
  millis: number;
}

export interface CypherResult {
  columns: string[];
  rows: unknown[][];
  truncated: boolean;
}

export interface CallRecord {
  at: number;
  channel: "mcp" | "rest" | "ui";
  project: string;
  operation: string;
  detail: string;
  result: string;
  context_chars: number;
  context_tokens: number;
  duration_ms: number;
  ok: boolean;
  error?: string;
}

export interface HistoryView {
  totals: { calls: number; context_tokens: number; median_ms: number; errors: number };
  series: { at: number; calls: number; mcp_tokens: number; other_tokens: number }[];
  calls: CallRecord[];
}

export interface ServerConfig {
  user: string;
  min_score: number;
  max_upload_mb: number;
  formats: string;
  default_project: string;
  default_status: Status;
  ner_labels: string[];
  embedding_dim: number;
  models: { embedding: string; ner: string; device: "cuda" | "cpu" };
  lbug_version: string;
}

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set("x-innerrag-client", "ui");
  if (init.body && !(init.body instanceof FormData)) headers.set("content-type", "application/json");
  const res = await fetch(path, { ...init, headers });
  if (!res.ok) {
    let message = `${res.status} ${res.statusText}`;
    try {
      const body = await res.json();
      if (body?.error) message = body.error;
    } catch {
      /* not JSON */
    }
    throw new ApiError(res.status, message);
  }
  if (res.status === 204) return undefined as T;
  return res.json() as Promise<T>;
}

const enc = encodeURIComponent;

function form(file: File, fields: Record<string, string | undefined>): FormData {
  const data = new FormData();
  for (const [k, v] of Object.entries(fields)) if (v !== undefined && v !== "") data.append(k, v);
  data.append("file", file, file.name);
  return data;
}
const qs = (params: Record<string, string | number | boolean | undefined>) => {
  const entries = Object.entries(params).filter(([, v]) => v !== undefined && v !== "");
  return entries.length ? "?" + new URLSearchParams(entries.map(([k, v]) => [k, String(v)])).toString() : "";
};

export const api = {
  health: () => request<{ status: string; version: string }>("/api/health"),
  config: () => request<ServerConfig>("/api/config"),
  jobs: (project = "") => request<Job[]>(`/api/jobs${qs({ project })}`),
  job: (id: string) => request<Job>(`/api/jobs/${enc(id)}`),
  cancelJob: (id: string) => request<void>(`/api/jobs/${enc(id)}`, { method: "DELETE" }),
  history: (p: { hours?: number; channel?: string; operation?: string; project?: string; errors?: boolean }) =>
    request<HistoryView>(`/api/history${qs(p)}`),

  projects: () => request<Project[]>("/api/projects"),
  createProject: (body: { id: string; title?: string; description?: string }) =>
    request<Project>("/api/projects", { method: "POST", body: JSON.stringify(body) }),
  updateProject: (id: string, body: { title?: string; description?: string }) =>
    request<Project>(`/api/projects/${enc(id)}`, { method: "PATCH", body: JSON.stringify(body) }),
  deleteProject: (id: string) => request<void>(`/api/projects/${enc(id)}`, { method: "DELETE" }),

  project: (p: string) => {
    const base = `/api/projects/${enc(p)}`;
    return {
      stats: () => request<Stats>(`${base}/stats`),
      documents: (f: { q?: string; status?: string; tag?: string } = {}) =>
        request<DocumentSummary[]>(`${base}/documents${qs(f)}`),
      document: (id: string) => request<DocumentWithChunks>(`${base}/documents/${enc(id)}`),
      content: (id: string) => request<DocumentContent>(`${base}/documents/${enc(id)}/content`),
      passages: (id: string, offset: number, limit: number) =>
        request<PassagePage>(`${base}/documents/${enc(id)}/passages${qs({ offset, limit })}`),
      /** The original file, opened by the browser (PDF viewer understands #page=N). */
      fileUrl: (id: string) => `${base}/documents/${enc(id)}/file`,
      /** Ingestion is asynchronous: these return the queued job. */
      createDocument: (body: object) =>
        request<Job>(`${base}/documents`, { method: "POST", body: JSON.stringify(body) }),
      replaceDocument: (id: string, body: object) =>
        request<Job>(`${base}/documents/${enc(id)}`, { method: "PUT", body: JSON.stringify(body) }),
      uploadDocument: (file: File, fields: Record<string, string | undefined>) =>
        request<Job>(`${base}/documents/upload`, { method: "POST", body: form(file, fields) }),
      uploadReplace: (id: string, file: File, fields: Record<string, string | undefined>) =>
        request<Job>(`${base}/documents/${enc(id)}/upload`, { method: "PUT", body: form(file, fields) }),
      patchDocument: (id: string, body: object) =>
        request<DocumentWithChunks>(`${base}/documents/${enc(id)}`, { method: "PATCH", body: JSON.stringify(body) }),
      deleteDocument: (id: string) => request<void>(`${base}/documents/${enc(id)}`, { method: "DELETE" }),
      tags: () => request<{ tag: string; count: number }[]>(`${base}/tags`),
      entities: (f: { q?: string; label?: string; limit?: number }) =>
        request<EntitySummary[]>(`${base}/entities${qs(f)}`),
      entity: (id: string) => request<EntityDetail>(`${base}/entities/${enc(id)}`),
      graph: (f: { limit?: number; min_weight?: number; label?: string; include_drafts?: boolean }) =>
        request<GraphView>(`${base}/graph${qs(f)}`),
      neighbourhood: (id: string, limit = 25) =>
        request<GraphView>(`${base}/graph/neighbourhood/${enc(id)}${qs({ limit })}`),
      search: (body: { query: string; k?: number; use_graph?: boolean; include_drafts?: boolean; tags?: string[]; min_score?: number }) =>
        request<SearchResponse>(`${base}/search`, { method: "POST", body: JSON.stringify(body) }),
      cypher: (query: string) =>
        request<CypherResult>(`${base}/cypher`, { method: "POST", body: JSON.stringify({ query }) }),
    };
  },
};

export type ProjectApi = ReturnType<typeof api.project>;
