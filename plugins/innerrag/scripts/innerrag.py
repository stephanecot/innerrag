#!/usr/bin/env python3
"""Command-line client for an innerrag server (Python standard library only).

Configuration (environment):
  INNERRAG_URL      server URL (default http://localhost:8080)
  INNERRAG_PROJECT  project id (default: the server's default project)

Commands print JSON on stdout, except `search --context`/`--map`, `read` and `content`, which print Markdown.
Errors go to stderr with a non-zero exit code.
"""

import argparse
import json
import mimetypes
import os
import pathlib
import re
import sys
import time
import uuid
import urllib.error
import urllib.parse
import urllib.request

URL = os.environ.get("INNERRAG_URL", "http://localhost:8080").rstrip("/")
TEXT_SUFFIXES = {".md", ".markdown", ".txt", ".rst", ".adoc", ".org", ".text"}
DOC_SUFFIXES = TEXT_SUFFIXES | {".pdf", ".docx", ".pptx", ".doc", ".ppt", ".html", ".htm"}


def multipart(path, fields):
    """Encodes a file upload plus text fields as multipart/form-data."""
    boundary = uuid.uuid4().hex
    parts = []
    for name, value in fields.items():
        if value in (None, ""):
            continue
        parts.append(f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{value}\r\n'.encode())
    mime = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
    filename = path.name.replace('"', "")
    parts.append(
        f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{filename}"\r\n'
        f"Content-Type: {mime}\r\n\r\n".encode()
        + path.read_bytes()
        + b"\r\n"
    )
    parts.append(f"--{boundary}--\r\n".encode())
    return b"".join(parts), f"multipart/form-data; boundary={boundary}"


def call(method, path, body=None, query=None, raw=None):
    url = URL + path
    if query:
        query = {k: v for k, v in query.items() if v not in (None, "", False)}
        if query:
            url += "?" + urllib.parse.urlencode(query)
    data = json.dumps(body).encode() if body is not None else None
    if raw is not None:
        data = raw[0]
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("x-innerrag-client", "rest")
    if raw is not None:
        req.add_header("content-type", raw[1])
    elif data is not None:
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=600) as res:
            raw = res.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as e:
        try:
            message = json.loads(e.read()).get("error", e.reason)
        except Exception:
            message = e.reason
        sys.exit(f"innerrag: HTTP {e.code}: {message}")
    except urllib.error.URLError as e:
        sys.exit(f"innerrag: cannot reach {URL} ({e.reason}). Is the server running? Set INNERRAG_URL otherwise.")


def project_id(args):
    if getattr(args, "project", None):
        return args.project
    if os.environ.get("INNERRAG_PROJECT"):
        return os.environ["INNERRAG_PROJECT"]
    return call("GET", "/api/config")["default_project"]


def p(args, suffix=""):
    return f"/api/projects/{urllib.parse.quote(project_id(args), safe='')}{suffix}"


def out(value):
    json.dump(value, sys.stdout, ensure_ascii=False, indent=2)
    sys.stdout.write("\n")


def tags_arg(raw):
    return [t.strip() for t in raw.split(",") if t.strip()] if raw is not None else None


def doc_id_for(path: pathlib.Path, root: pathlib.Path) -> str:
    """Stable id from the file path relative to the root, so re-ingesting replaces."""
    rel = path.relative_to(root) if path.is_relative_to(root) else path.name
    return re.sub(r"[^A-Za-z0-9._/-]+", "-", str(rel).replace(os.sep, "/")).strip("-")


def wait_job(job, quiet=False):
    """Polls a queued ingestion until it ends; returns its report or exits on failure."""
    last = ""
    while not job.get("finished_at"):
        line = f"  {job['filename']}: {job['stage']} {job['done']}/{job['total']}" if job["total"] else f"  {job['filename']}: {job['stage']}"
        if line != last and not quiet:
            print(line, file=sys.stderr)
            last = line
        time.sleep(2)
        job = call("GET", f"/api/jobs/{job['id']}")
    if job["stage"] == "cancelled":
        sys.exit(f"innerrag: ingestion of {job['filename']} was cancelled")
    if job["stage"] != "done":
        sys.exit(f"innerrag: ingestion of {job['filename']} failed: {job.get('error')}")
    return job["report"]


# ---- commands ------------------------------------------------------------------


def cmd_health(_):
    out({"health": call("GET", "/api/health"), "config": call("GET", "/api/config")})


def cmd_projects(_):
    out(call("GET", "/api/projects"))


def cmd_project_create(args):
    out(call("POST", "/api/projects", {"id": args.id, "title": args.title, "description": args.description}))


def cmd_project_delete(args):
    call("DELETE", f"/api/projects/{urllib.parse.quote(args.id, safe='')}")
    out({"deleted": args.id})


def cmd_stats(args):
    out(call("GET", p(args, "/stats")))


def cmd_search(args):
    res = call("POST", p(args, "/search"), {
        "query": args.query,
        "k": args.k,
        "include_drafts": args.include_drafts,
        "tags": tags_arg(args.tags) or [],
        "use_graph": not args.no_graph,
        "mode": "map" if args.map else None,
        "budget": args.budget,
        "session_id": args.session or os.environ.get("INNERRAG_SESSION"),
    })
    if args.context or args.map:
        sys.stdout.write(res["context"])
        if not args.map and not args.budget:
            sys.stdout.write("\n## Sources\n")
            for n, c in enumerate(res["chunks"], 1):
                sys.stdout.write(f"[{n}] {c['doc_title']} (doc `{c['doc_id']}`, chunk `{c['id']}`, {c['doc_status']})\n")
    else:
        out(res)


def cmd_read(args):
    res = call("POST", p(args, "/passages"), {
        "ids": args.ids,
        "window": args.window,
        "session_id": args.session or os.environ.get("INNERRAG_SESSION"),
    })
    sys.stdout.write(res["text"])


def cmd_cite(args):
    call("POST", p(args, "/feedback/cite"), {"question": args.question, "chunk_ids": args.ids, "outcome": args.outcome})
    print("recorded")


def cmd_gaps(args):
    res = call("GET", p(args, "/feedback"))
    if args.json:
        out(res)
        return
    print(f"{res['searches']} searches, {res['unanswered_searches']} without a relevant passage, {res['citations']} cited answers")
    print("\nUnanswered questions:")
    for g in res["gaps"]:
        print(f"- ({g['count']}x) {g['question']}" + (f"  [also: {'; '.join(g['variants'][:3])}]" if g["variants"] else ""))
    print("\nCited answers not yet in eval.json:")
    for pr in res["proposals"]:
        where = ", ".join(f"{c['doc_title']}" + (f" p.{c['page']}" if c["page"] else "") for c in pr["passages"])
        print(f"- {pr['question']} -> {where}")


def cmd_ingest(args):
    """Ingests files (or stdin) as documents; existing ids are replaced. Waits unless --no-wait."""
    status = args.status.upper() if args.status else None
    tags = tags_arg(args.tags)
    jobs = []
    if args.stdin:
        text = sys.stdin.read()
        body = {"title": args.title or "", "text": text, "tags": tags, "status": status, "source": args.source}
        if args.id:
            jobs.append(call("PUT", p(args, f"/documents/{urllib.parse.quote(args.id, safe='')}"), body))
        else:
            jobs.append(call("POST", p(args, "/documents"), body))
    else:
        root = pathlib.Path(args.root or os.getcwd()).resolve()
        files = []
        for raw in args.paths:
            path = pathlib.Path(raw).resolve()
            if path.is_dir():
                files += sorted(f for f in path.rglob("*") if f.is_file() and f.suffix.lower() in DOC_SUFFIXES)
            elif path.is_file():
                files.append(path)
            else:
                sys.exit(f"innerrag: no such file or directory: {raw}")
        if not files:
            sys.exit("innerrag: nothing to ingest (supported: " + ", ".join(sorted(DOC_SUFFIXES)) + ")")
        if len(files) > 1 and (args.id or args.title):
            sys.exit("innerrag: --id and --title only apply to a single file")
        for f in files:
            doc_id = args.id or doc_id_for(f, root)
            fields = {
                "title": args.title,
                "tags": ",".join(tags) if tags is not None else None,
                "status": status,
                "source": args.source or str(f.relative_to(root) if f.is_relative_to(root) else f),
            }
            job = call("PUT", p(args, f"/documents/{urllib.parse.quote(doc_id, safe='')}/upload"), raw=multipart(f, fields))
            print(f"queued {doc_id} (job {job['id']})", file=sys.stderr)
            jobs.append(job)
    if args.no_wait:
        out(jobs)
        return
    reports = []
    for job in jobs:
        report = wait_job(job)
        print(f"ingested {report['id']}: {report['chunks']} chunks, {report['entities']} entities", file=sys.stderr)
        reports.append(report)
    out(reports)


def cmd_jobs(args):
    out(call("GET", "/api/jobs", query={"project": args.project or ""}))


def cmd_job_cancel(args):
    call("DELETE", f"/api/jobs/{urllib.parse.quote(args.id, safe='')}")
    out({"cancelled": args.id})


def cmd_docs(args):
    out(call("GET", p(args, "/documents"), query={"q": args.q, "status": args.status, "tag": args.tag}))


def cmd_doc(args):
    out(call("GET", p(args, f"/documents/{urllib.parse.quote(args.id, safe='')}")))


def cmd_content(args):
    """Full text of a document as Markdown (PDFs keep <!-- page N --> markers)."""
    res = call("GET", p(args, f"/documents/{urllib.parse.quote(args.id, safe='')}/content"))
    content = res.get("content") or ""
    if not content.lstrip().startswith("#"):
        sys.stdout.write(f"# {res.get('title', args.id)}\n\n")
    sys.stdout.write(content)
    sys.stdout.write("\n")


def cmd_doc_set(args):
    patch = {}
    if args.title is not None:
        patch["title"] = args.title
    if args.status is not None:
        patch["status"] = args.status.upper()
    if args.tags is not None:
        patch["tags"] = tags_arg(args.tags)
    if args.source is not None:
        patch["source"] = args.source
    if not patch:
        sys.exit("innerrag: nothing to change (use --title, --status, --tags or --source)")
    doc = call("PATCH", p(args, f"/documents/{urllib.parse.quote(args.id, safe='')}"), patch)
    doc.pop("passages", None)
    out(doc)


def cmd_doc_delete(args):
    call("DELETE", p(args, f"/documents/{urllib.parse.quote(args.id, safe='')}"))
    out({"deleted": args.id})


def cmd_tags(args):
    out(call("GET", p(args, "/tags")))


def cmd_entities(args):
    out(call("GET", p(args, "/entities"), query={"q": args.q, "label": args.label, "limit": args.limit}))


def cmd_entity(args):
    entity_id = args.id
    if ":" not in entity_id:
        hits = call("GET", p(args, "/entities"), query={"q": entity_id, "limit": 5})
        exact = [h for h in hits if h["name"].lower() == entity_id.lower()]
        if not (exact or hits):
            sys.exit(f"innerrag: no entity matches '{args.id}'")
        entity_id = (exact or hits)[0]["id"]
    out(call("GET", p(args, f"/entities/{urllib.parse.quote(entity_id, safe='')}")))


def cmd_cypher(args):
    query = args.query if args.query != "-" else sys.stdin.read()
    out(call("POST", p(args, "/cypher"), {"query": query}))


def cmd_eval(args):
    """Runs the project's reference questions (eval.json) and prints the metrics."""
    thresholds = [float(t) for t in args.sweep.split(",")] if args.sweep else []
    rep = call("POST", p(args, "/eval/run"), {"k": args.k, "min_scores": thresholds})
    for i, r in enumerate(rep["runs"]):
        best = " <- best" if i == rep["best"] and len(rep["runs"]) > 1 else ""
        rejected = "-" if r["rejected_out_of_scope"] is None else f"{r['rejected_out_of_scope']:.0%}"
        print(
            f"seuil {r['min_score']:.2f}: hit@1 {r['hit_at_1']:.0%}  hit@3 {r['hit_at_3']:.0%}  hit@{rep['k']} {r['hit_at_k']:.0%}"
            f"  MRR {r['mrr']:.2f}  hors sujet rejetées {rejected}  global {r['overall']:.0%}  ({r['avg_millis']:.0f} ms){best}",
            file=sys.stderr,
        )
    out(rep)


def cmd_history(args):
    res = call("GET", "/api/history", query={
        "hours": args.hours, "channel": args.channel, "operation": args.operation,
        "project": args.project or "", "errors": "true" if args.errors else None, "limit": args.limit,
    })
    res.pop("series", None)
    out(res)


def main():
    ap = argparse.ArgumentParser(prog="innerrag", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--project", "-p", help="project id (default: $INNERRAG_PROJECT or the server default)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    sub.add_parser("health", help="server status and configuration").set_defaults(fn=cmd_health)
    sub.add_parser("projects", help="list projects").set_defaults(fn=cmd_projects)
    s = sub.add_parser("project-create", help="create a project")
    s.add_argument("id")
    s.add_argument("--title")
    s.add_argument("--description", default="")
    s.set_defaults(fn=cmd_project_create)
    s = sub.add_parser("project-delete", help="delete a project and its database (irreversible)")
    s.add_argument("id")
    s.set_defaults(fn=cmd_project_delete)
    sub.add_parser("stats", help="counts for the project").set_defaults(fn=cmd_stats)

    s = sub.add_parser("search", help="Graph RAG retrieval")
    s.add_argument("query")
    s.add_argument("-k", type=int, default=8)
    s.add_argument("--tags", help="comma-separated tag filter")
    s.add_argument("--include-drafts", action="store_true")
    s.add_argument("--no-graph", action="store_true", help="plain vector search, for comparison")
    s.add_argument("--context", action="store_true", help="print the markdown context instead of JSON")
    s.add_argument("--map", action="store_true", help="one line per passage (about 3x fewer tokens); unfold with `read`")
    s.add_argument("--budget", type=int, help="maximum context size in tokens")
    s.add_argument("--session", help="session id: passages already sent are not repeated (or INNERRAG_SESSION)")
    s.set_defaults(fn=cmd_search)

    s = sub.add_parser("read", help="full text of passages by chunk id, with --window neighbours")
    s.add_argument("ids", nargs="+")
    s.add_argument("--window", type=int, default=0)
    s.add_argument("--session")
    s.set_defaults(fn=cmd_read)

    s = sub.add_parser("cite", help="report the passages an answer used (feeds eval proposals and the gap report)")
    s.add_argument("question")
    s.add_argument("ids", nargs="*", help="chunk ids used")
    s.add_argument("--outcome", choices=["answered", "partial", "not_found"], default="answered")
    s.set_defaults(fn=cmd_cite)

    s = sub.add_parser("gaps", help="unanswered questions and cited answers awaiting validation")
    s.add_argument("--json", action="store_true")
    s.set_defaults(fn=cmd_gaps)

    s = sub.add_parser("ingest", help="add or replace documents (pdf, docx, pptx, doc, ppt, md, txt) from files, folders or stdin")
    s.add_argument("paths", nargs="*")
    s.add_argument("--no-wait", action="store_true", help="queue and return the jobs without waiting")
    s.add_argument("--stdin", action="store_true", help="read one text/markdown document from stdin")
    s.add_argument("--id", help="document id (single document)")
    s.add_argument("--title", help="document title (single document)")
    s.add_argument("--tags", help="comma-separated tags")
    s.add_argument("--status", choices=["draft", "published", "DRAFT", "PUBLISHED"])
    s.add_argument("--source")
    s.add_argument("--root", help="base folder for ids derived from paths (default: cwd)")
    s.set_defaults(fn=cmd_ingest)

    sub.add_parser("jobs", help="recent background ingestions (all projects, or --project)").set_defaults(fn=cmd_jobs)
    s = sub.add_parser("job-cancel", help="cancel a queued or running ingestion")
    s.add_argument("id")
    s.set_defaults(fn=cmd_job_cancel)

    s = sub.add_parser("docs", help="list documents")
    s.add_argument("--q")
    s.add_argument("--status", choices=["draft", "published", "DRAFT", "PUBLISHED"])
    s.add_argument("--tag")
    s.set_defaults(fn=cmd_docs)
    s = sub.add_parser("doc", help="show a document with its passages")
    s.add_argument("id")
    s.set_defaults(fn=cmd_doc)

    s = sub.add_parser("content", help="full text of a document as Markdown, with page markers for PDFs")
    s.add_argument("id")
    s.set_defaults(fn=cmd_content)
    s = sub.add_parser("doc-set", help="change title, status, tags or source (no re-indexing)")
    s.add_argument("id")
    s.add_argument("--title")
    s.add_argument("--status", choices=["draft", "published", "DRAFT", "PUBLISHED"])
    s.add_argument("--tags", help="comma-separated; replaces the tag list ('' clears it)")
    s.add_argument("--source")
    s.set_defaults(fn=cmd_doc_set)
    s = sub.add_parser("doc-delete", help="delete a document")
    s.add_argument("id")
    s.set_defaults(fn=cmd_doc_delete)
    sub.add_parser("tags", help="tags with document counts").set_defaults(fn=cmd_tags)

    s = sub.add_parser("entities", help="list entities")
    s.add_argument("--q")
    s.add_argument("--label")
    s.add_argument("--limit", type=int, default=50)
    s.set_defaults(fn=cmd_entities)
    s = sub.add_parser("entity", help="an entity, its neighbours and passages (name or label:name id)")
    s.add_argument("id")
    s.set_defaults(fn=cmd_entity)
    s = sub.add_parser("cypher", help="read-only Cypher query ('-' reads stdin)")
    s.add_argument("query")
    s.set_defaults(fn=cmd_cypher)

    s = sub.add_parser("eval", help="run the reference questions (eval.json) and print retrieval metrics")
    s.add_argument("-k", type=int, default=8)
    s.add_argument("--sweep", help="comma-separated thresholds to compare, e.g. 0.75,0.78,0.8,0.82,0.85")
    s.set_defaults(fn=cmd_eval)

    s = sub.add_parser("history", help="call history and context consumption")
    s.add_argument("--hours", type=int, default=24)
    s.add_argument("--channel", choices=["mcp", "rest", "ui"])
    s.add_argument("--operation")
    s.add_argument("--errors", action="store_true")
    s.add_argument("--limit", type=int, default=50)
    s.set_defaults(fn=cmd_history)

    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
