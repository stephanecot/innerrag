import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { api, type Job, type Project, type ServerConfig } from "./api";
import {
  ChevronIcon, ConsoleIcon, DocIcon, FolderIcon, HistoryIcon, Logo, MapIcon, MoonIcon, SearchIcon, SunIcon,
} from "./Icons";
import { JOB_STAGE, jobPercent } from "./util";
import MapView from "./views/MapView";
import DocumentsView from "./views/DocumentsView";
import SearchView from "./views/SearchView";
import HistoryView from "./views/HistoryView";
import CypherView from "./views/CypherView";
import ProjectsView from "./views/ProjectsView";
import ReaderView from "./views/ReaderView";

export type Route = "carte" | "documents" | "lire" | "recherche" | "historique" | "cypher" | "projets";

export interface Location {
  route: Route;
  params: URLSearchParams;
}

const ROUTES: Route[] = ["carte", "documents", "lire", "recherche", "historique", "cypher", "projets"];

function readLocation(): Location {
  const [path, query = ""] = window.location.hash.replace(/^#\/?/, "").split("?");
  const route = ROUTES.includes(path as Route) ? (path as Route) : "carte";
  return { route, params: new URLSearchParams(query) };
}

export function href(route: Route, params: Record<string, string> = {}) {
  const q = new URLSearchParams(params).toString();
  return `#/${route}${q ? `?${q}` : ""}`;
}

export function go(route: Route, params: Record<string, string> = {}) {
  window.location.hash = href(route, params);
}

const PROJECT_STORAGE = "innerrag.project";
const THEME_STORAGE = "innerrag.theme";

function stored(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* storage unavailable */
  }
}

const NAV: { route: Route; label: string; icon: () => ReactElement }[] = [
  { route: "carte", label: "Carte", icon: MapIcon },
  { route: "documents", label: "Documents", icon: DocIcon },
  { route: "recherche", label: "Recherche", icon: () => <SearchIcon /> },
  { route: "historique", label: "Historique", icon: HistoryIcon },
  { route: "cypher", label: "Console Cypher", icon: ConsoleIcon },
];

export default function App() {
  const [location, setLocation] = useState<Location>(readLocation);
  const [projects, setProjects] = useState<Project[]>([]);
  const [config, setConfig] = useState<ServerConfig | null>(null);
  const [project, setProjectState] = useState<string>(() => stored(PROJECT_STORAGE));
  const [loadError, setLoadError] = useState("");
  const [theme, setTheme] = useState<string>(() => stored(THEME_STORAGE));

  useEffect(() => {
    const onHash = () => setLocation(readLocation());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  useEffect(() => {
    if (theme) document.documentElement.dataset.theme = theme;
    else delete document.documentElement.dataset.theme;
  }, [theme]);

  const refreshProjects = useCallback(async () => {
    try {
      const [list, cfg] = await Promise.all([api.projects(), api.config()]);
      setProjects(list);
      setConfig(cfg);
      setLoadError("");
      setProjectState((current) => {
        if (list.some((p) => p.id === current)) return current;
        const fallback = list.find((p) => p.id === cfg.default_project)?.id ?? list[0]?.id ?? "";
        store(PROJECT_STORAGE, fallback);
        return fallback;
      });
    } catch (e) {
      setLoadError((e as Error).message);
    }
  }, []);

  useEffect(() => {
    refreshProjects();
  }, [refreshProjects]);

  const setProject = useCallback((id: string) => {
    store(PROJECT_STORAGE, id);
    setProjectState(id);
  }, []);

  const toggleTheme = () => {
    const dark = theme ? theme === "dark" : window.matchMedia("(prefers-color-scheme: dark)").matches;
    const next = dark ? "light" : "dark";
    store(THEME_STORAGE, next);
    setTheme(next);
  };
  const isDark = theme ? theme === "dark" : window.matchMedia?.("(prefers-color-scheme: dark)").matches;

  const current = projects.find((p) => p.id === project);
  const view = useMemo(() => {
    if (location.route === "projets") {
      return <ProjectsView projects={projects} current={project} onChanged={refreshProjects} onOpen={setProject} />;
    }
    if (location.route === "historique") return <HistoryView projects={projects} current={project} />;
    if (!project) {
      return (
        <div className="page">
          <div className="empty">
            <p>{loadError ? `Le serveur ne répond pas : ${loadError}` : "Aucun projet pour l'instant."}</p>
            <a className="btn btn-primary" href={href("projets")}>Créer un projet</a>
          </div>
        </div>
      );
    }
    const key = `${project}:${location.route}`;
    switch (location.route) {
      case "documents":
        return <DocumentsView key={key} project={project} config={config} params={location.params} />;
      case "lire":
        return <ReaderView key={`${key}:${location.params.get("doc")}`} project={project} params={location.params} />;
      case "recherche":
        return <SearchView key={key} project={project} />;
      case "cypher":
        return <CypherView key={key} project={project} />;
      default:
        return <MapView key={key} project={project} params={location.params} />;
    }
  }, [location, project, projects, config, loadError, refreshProjects, setProject]);

  return (
    <div className="app">
      <nav className="rail" aria-label="Navigation principale">
        <div className="brand"><Logo />innerrag</div>
        <ProjectSwitcher projects={projects} current={current} onPick={setProject} />
        <IngestionIndicator
          onOpen={(job) => {
            setProject(job.project);
            go("documents");
          }}
        />
        <div className="nav">
          {NAV.map(({ route, label, icon: Icon }) => (
            <a key={route} href={href(route)} aria-current={location.route === route || (route === "documents" && location.route === "lire") ? "page" : undefined}>
              <Icon />
              {label}
            </a>
          ))}
        </div>
        <div className="rail-foot">
          <div className="nav">
            <a href={href("projets")} aria-current={location.route === "projets" ? "page" : undefined}>
              <FolderIcon />
              Gérer les projets
            </a>
          </div>
          <div className="rail-user">
            <span>
              {config ? `Connecté en tant que ${config.user}` : ""}
              {config && <><br />Calcul : {config.models.device === "cuda" ? "GPU NVIDIA" : "CPU"}</>}
            </span>
            <button
              type="button"
              className="rail-icon-button"
              onClick={toggleTheme}
              aria-label={isDark ? "Passer au thème clair" : "Passer au thème sombre"}
            >
              {isDark ? <SunIcon /> : <MoonIcon />}
            </button>
          </div>
        </div>
      </nav>
      <main className="content">{view}</main>
    </div>
  );
}

/** Ingestions of every project, visible from any page. */
function IngestionIndicator({ onOpen }: { onOpen: (job: Job) => void }) {
  const [jobs, setJobs] = useState<Job[]>([]);
  useEffect(() => {
    let live = true;
    let timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      let active = false;
      try {
        // Queue order: the running job first, then the next ones.
        const list = (await api.jobs()).filter((j) => !j.finished_at).sort((a, b) => a.created_at - b.created_at);
        active = list.length > 0;
        if (live) setJobs(list);
      } catch {
        /* keep the last state */
      }
      if (live) timer = setTimeout(tick, active ? 1500 : 5000);
    };
    tick();
    window.addEventListener("innerrag:job-queued", tick);
    return () => {
      live = false;
      clearTimeout(timer);
      window.removeEventListener("innerrag:job-queued", tick);
    };
  }, []);
  if (!jobs.length) return null;
  return (
    <section className="rail-jobs" aria-label="Ingestions en cours" aria-live="polite">
      <span className="rail-label">{jobs.length > 1 ? `${jobs.length} ingestions en cours` : "Ingestion en cours"}</span>
      {jobs.slice(0, 2).map((j) => (
        <div key={j.id} className="rail-job-wrap">
          <button type="button" className="rail-job" onClick={() => onOpen(j)}>
            <span className="rail-job-name">{j.filename}</span>
            <span className="rail-job-meta">{j.project}, {JOB_STAGE[j.stage].toLowerCase()} {j.stage === "queued" ? "" : `${jobPercent(j)} %`}</span>
            <span className="rail-job-track"><span style={{ width: `${jobPercent(j)}%` }} /></span>
          </button>
          <button
            type="button"
            className="rail-job-cancel"
            aria-label={`Annuler l'ingestion de ${j.filename}`}
            title="Annuler cette ingestion"
            onClick={async () => {
              await api.cancelJob(j.id).catch(() => undefined);
              window.dispatchEvent(new Event("innerrag:job-queued"));
            }}
          >
            ×
          </button>
        </div>
      ))}
      {jobs.length > 2 && <span className="rail-label">et {jobs.length - 2} autres en attente</span>}
    </section>
  );
}

function ProjectSwitcher({
  projects, current, onPick,
}: { projects: Project[]; current?: Project; onPick: (id: string) => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);

  return (
    <div className="project-switch" ref={ref}>
      <span className="rail-label" id="project-label">Projet</span>
      <button
        type="button"
        className="project-button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-labelledby="project-label"
        onClick={() => setOpen((o) => !o)}
      >
        <span>{current?.id ?? "Aucun projet"}</span>
        <ChevronIcon />
      </button>
      {open && (
        <ul className="project-menu" role="listbox" aria-labelledby="project-label">
          {projects.map((p) => (
            <li key={p.id} role="option" aria-selected={p.id === current?.id}>
              <button
                type="button"
                aria-current={p.id === current?.id}
                onClick={() => {
                  onPick(p.id);
                  setOpen(false);
                }}
              >
                <strong>{p.title}</strong>
                <small>{p.id}</small>
              </button>
            </li>
          ))}
          <li>
            <button type="button" onClick={() => { setOpen(false); go("projets"); }}>
              <strong>Gérer les projets</strong>
            </button>
          </li>
        </ul>
      )}
    </div>
  );
}
