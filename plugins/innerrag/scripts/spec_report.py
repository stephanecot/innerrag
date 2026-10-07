#!/usr/bin/env python3
"""Renders a code-versus-specifications comparison as a PDF with a fixed template.

The analysis (done by the innerrag-spec-check skill) is a JSON file; this script only lays it
out, always the same way, in French or English:

  1. header: project, repository, commit, specifications compared, date;
  2. summary: compliance rate, counts per status, key points;
  3. requirements table: id, requirement, source, status, evidence;
  4. gaps in detail: non-compliant then partial requirements, by severity;
  5. code with no specification;
  6. method and limits.

Usage:
    python3 spec_report.py analysis.json -o report.pdf [--lang fr|en] [--html report.html]
    python3 spec_report.py --example > analysis.json      # the expected JSON, filled with an example

The PDF is printed by a local Chrome or Chromium (headless). Without one, the HTML is written
and the script says so. Standard library only.
"""

import argparse
import datetime as dt
import html
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

STATUSES = ["compliant", "partial", "non_compliant", "not_verifiable"]
SEVERITIES = ["blocker", "major", "minor"]
COLORS = {"compliant": "#2f7d3e", "partial": "#b7791f", "non_compliant": "#b42318", "not_verifiable": "#6b6f73"}

LABELS = {
    "fr": {
        "title": "Rapport de conformité du code aux spécifications",
        "project": "Projet innerrag",
        "repository": "Dépôt analysé",
        "commit": "Version du code",
        "specs": "Spécifications",
        "date": "Date",
        "author": "Analyse",
        "summary": "Synthèse",
        "rate": "taux de conformité",
        "rate_note": "Exigences conformes sur exigences vérifiables (hors « non vérifiable »).",
        "key_points": "Points clés",
        "requirements": "Exigences",
        "col_id": "Réf.",
        "col_req": "Exigence",
        "col_source": "Source",
        "col_status": "Statut",
        "col_evidence": "Preuves dans le code",
        "gaps": "Écarts en détail",
        "no_gaps": "Aucun écart : toutes les exigences vérifiables sont conformes.",
        "expected": "Attendu",
        "found": "Constaté",
        "recommendation": "Recommandation",
        "evidence": "Preuves",
        "severity": "Gravité",
        "unspecified": "Dans le code, absent des spécifications",
        "no_unspecified": "Rien de notable.",
        "method": "Méthode et limites",
        "limits": "Limites",
        "page": "page",
        "of": "sur",
        "none": "aucune",
        "dirty": "modifications non commitées",
        "status": {
            "compliant": "Conforme",
            "partial": "Partiel",
            "non_compliant": "Non conforme",
            "not_verifiable": "Non vérifiable",
        },
        "severity_label": {"blocker": "Bloquant", "major": "Majeur", "minor": "Mineur"},
        "count": lambda n: f"{n} exigence" + ("s" if n > 1 else ""),
    },
    "en": {
        "title": "Code compliance report against the specifications",
        "project": "innerrag project",
        "repository": "Repository",
        "commit": "Code version",
        "specs": "Specifications",
        "date": "Date",
        "author": "Analysis",
        "summary": "Summary",
        "rate": "compliance rate",
        "rate_note": "Compliant requirements out of verifiable ones (not counting “not verifiable”).",
        "key_points": "Key points",
        "requirements": "Requirements",
        "col_id": "Ref.",
        "col_req": "Requirement",
        "col_source": "Source",
        "col_status": "Status",
        "col_evidence": "Evidence in the code",
        "gaps": "Gaps in detail",
        "no_gaps": "No gap: every verifiable requirement is met.",
        "expected": "Expected",
        "found": "Found",
        "recommendation": "Recommendation",
        "evidence": "Evidence",
        "severity": "Severity",
        "unspecified": "In the code, missing from the specifications",
        "no_unspecified": "Nothing worth noting.",
        "method": "Method and limits",
        "limits": "Limits",
        "page": "page",
        "of": "of",
        "none": "none",
        "dirty": "uncommitted changes",
        "status": {
            "compliant": "Compliant",
            "partial": "Partial",
            "non_compliant": "Non-compliant",
            "not_verifiable": "Not verifiable",
        },
        "severity_label": {"blocker": "Blocker", "major": "Major", "minor": "Minor"},
        "count": lambda n: f"{n} requirement" + ("s" if n != 1 else ""),
    },
}

EXAMPLE = {
    "lang": "fr",
    "project": "sample",
    "repository": {"path": "/home/me/kanban", "branch": "main", "commit": "3f2a9c1", "dirty": False},
    "specs": [{"id": "spec-kanban", "title": "Spécification Kanban v2", "source": "spec-kanban.pdf"}],
    "generated_at": "2026-10-07T21:30:00Z",
    "author": "Claude Code (innerrag-spec-check)",
    "summary": [
        "Les cartes se déplacent entre colonnes comme spécifié.",
        "La suppression d'une carte n'est pas implémentée (exigence REQ-004).",
    ],
    "requirements": [
        {
            "id": "REQ-001",
            "title": "Trois colonnes : À faire, En cours, Terminé",
            "text": "Le tableau affiche trois colonnes : « À faire », « En cours » et « Terminé ».",
            "source": {"doc_id": "spec-kanban", "doc_title": "Spécification Kanban v2", "page": 3, "section": "2.1 Tableau"},
            "status": "compliant",
            "evidence": [{"file": "src/constants.js", "line": 5, "note": "CARD_STATUSES"}],
        },
        {
            "id": "REQ-004",
            "title": "Supprimer une carte",
            "text": "L'utilisateur peut supprimer une carte après confirmation.",
            "source": {"doc_id": "spec-kanban", "doc_title": "Spécification Kanban v2", "page": 5},
            "status": "non_compliant",
            "severity": "major",
            "found": "Aucune action de suppression de carte : seul `deleteTask` existe, pour les tâches.",
            "evidence": [{"file": "src/store/useKanban.js", "line": 42, "note": "pas d'action deleteCard"}],
            "recommendation": "Ajouter une action `deleteCard` et un bouton avec confirmation dans `Card.jsx`.",
        },
    ],
    "unspecified": [
        {"title": "Persistance locale", "note": "Les cartes sont gardées dans le localStorage quand aucune API n'est configurée.",
         "evidence": [{"file": "src/api/client.js", "line": 4}]},
    ],
    "method": "Exigences extraites des spécifications publiées dans innerrag, puis recherchées dans le code (lecture des fichiers, recherche de symboles).",
    "limits": ["Comportement vérifié par lecture du code, sans exécution ni tests."],
}


class Invalid(Exception):
    pass


def validate(data):
    """Checks the analysis against the schema; returns it normalised (sorted, defaults filled)."""
    if not isinstance(data, dict):
        raise Invalid("the root must be a JSON object")
    reqs = data.get("requirements")
    if not isinstance(reqs, list) or not reqs:
        raise Invalid("`requirements` must be a non-empty list")
    seen = set()
    for i, r in enumerate(reqs):
        where = f"requirements[{i}]"
        for key in ("id", "title", "status"):
            if not isinstance(r.get(key), str) or not r[key].strip():
                raise Invalid(f"{where}.{key} is required")
        if r["id"] in seen:
            raise Invalid(f"{where}: duplicate id {r['id']}")
        seen.add(r["id"])
        if r["status"] not in STATUSES:
            raise Invalid(f"{where}.status must be one of {', '.join(STATUSES)}")
        if r["status"] in ("partial", "non_compliant") and r.get("severity") not in SEVERITIES:
            raise Invalid(f"{where}.severity must be one of {', '.join(SEVERITIES)} for a {r['status']} requirement")
        if not isinstance(r.get("source", {}), dict):
            raise Invalid(f"{where}.source must be an object")
        if not isinstance(r.get("evidence", []), list):
            raise Invalid(f"{where}.evidence must be a list")
    natural = lambda r: [int(t) if t.isdigit() else t for t in re.split(r"(\d+)", r["id"])]
    data["requirements"] = sorted(reqs, key=natural)
    data.setdefault("specs", [])
    data.setdefault("summary", [])
    data.setdefault("unspecified", [])
    data.setdefault("limits", [])
    data.setdefault("repository", {})
    return data


def inline(text):
    """Escapes text and turns `code` spans into <code>."""
    parts = re.split(r"(`[^`]+`)", str(text or ""))
    return "".join(
        f"<code>{html.escape(p[1:-1])}</code>" if p.startswith("`") and p.endswith("`") and len(p) > 1 else html.escape(p)
        for p in parts
    )


def evidence_html(items):
    out = []
    for e in items or []:
        loc = html.escape(str(e.get("file", "")))
        if e.get("line"):
            loc += f":{int(e['line'])}"
        note = f" <span class='note'>{inline(e.get('note'))}</span>" if e.get("note") else ""
        out.append(f"<li><code>{loc}</code>{note}</li>")
    return f"<ul class='evidence'>{''.join(out)}</ul>" if out else "<span class='muted'>—</span>"


def source_html(src):
    bits = [html.escape(str(src.get("doc_title") or src.get("doc_id") or ""))]
    if src.get("section"):
        bits.append(html.escape(str(src["section"])))
    if src.get("page"):
        bits.append(f"p. {int(src['page'])}")
    return "<br>".join(b for b in bits if b) or "—"


def badge(L, status):
    return f"<span class='badge' style='background:{COLORS[status]}'>{L['status'][status]}</span>"


def render(data, lang):
    L = LABELS[lang]
    reqs = data["requirements"]
    counts = {s: sum(1 for r in reqs if r["status"] == s) for s in STATUSES}
    verifiable = len(reqs) - counts["not_verifiable"]
    rate = round(100 * counts["compliant"] / verifiable) if verifiable else 0
    repo = data["repository"]
    commit = " ".join(x for x in [repo.get("branch"), f"@ {repo['commit']}" if repo.get("commit") else ""] if x)
    if repo.get("dirty"):
        commit += f" ({L['dirty']})"
    try:
        when = dt.datetime.fromisoformat(str(data.get("generated_at", "")).replace("Z", "+00:00"))
    except ValueError:
        when = dt.datetime.now(dt.timezone.utc)
    date = when.strftime("%d/%m/%Y %H:%M") if lang == "fr" else when.strftime("%Y-%m-%d %H:%M")
    specs = "".join(
        "<li>" + html.escape(str(s.get("title") or s.get("id")))
        + (" <span class='muted'>(" + html.escape(str(s["source"])) + ")</span>" if s.get("source") else "")
        + "</li>"
        for s in data["specs"]
    ) or "<li>" + L["none"] + "</li>"

    bar = "".join(
        "<span style='flex:" + str(counts[s]) + ";background:" + COLORS[s] + "'></span>" for s in STATUSES if counts[s]
    )
    legend = "".join(
        "<li><span class='dot' style='background:" + COLORS[s] + "'></span>" + L["status"][s] + " <b>" + str(counts[s]) + "</b></li>"
        for s in STATUSES
    )
    key_points = "".join("<li>" + inline(k) + "</li>" for k in data["summary"][:6])

    def row(r):
        text = r.get("text")
        detail = "<div class='req-text'>" + inline(text) + "</div>" if text and text != r["title"] else ""
        return (
            "<tr><td class='id'>" + html.escape(r["id"]) + "</td>"
            + "<td><b>" + inline(r["title"]) + "</b>" + detail + "</td>"
            + "<td class='src'>" + source_html(r.get("source", {})) + "</td>"
            + "<td>" + badge(L, r["status"]) + "</td>"
            + "<td>" + evidence_html(r.get("evidence")) + "</td></tr>"
        )

    rows = "".join(row(r) for r in reqs)

    rank = {s: i for i, s in enumerate(SEVERITIES)}
    gaps = sorted(
        (r for r in reqs if r["status"] in ("non_compliant", "partial")),
        key=lambda r: (0 if r["status"] == "non_compliant" else 1, rank.get(r.get("severity"), 3)),
    )
    def card(r):
        sev = r.get("severity")
        source = source_html(r.get("source", {})).replace("<br>", ", ")
        recommendation = (
            "<dt>" + L["recommendation"] + "</dt><dd>" + inline(r["recommendation"]) + "</dd>" if r.get("recommendation") else ""
        )
        return (
            "<article class='gap' style='border-left-color:" + COLORS[r["status"]] + "'>"
            + "<header><span class='id'>" + html.escape(r["id"]) + "</span> <b>" + inline(r["title"]) + "</b>"
            + "<span class='right'>" + badge(L, r["status"]) + " <span class='sev sev-" + str(sev) + "'>"
            + L["severity"] + " : " + L["severity_label"].get(sev, "—") + "</span></span></header><dl>"
            + "<dt>" + L["expected"] + "</dt><dd>" + inline(r.get("text") or r["title"]) + " <span class='muted'>— " + source + "</span></dd>"
            + "<dt>" + L["found"] + "</dt><dd>" + inline(r.get("found") or r.get("gap") or "—") + "</dd>"
            + "<dt>" + L["evidence"] + "</dt><dd>" + evidence_html(r.get("evidence")) + "</dd>"
            + recommendation + "</dl></article>"
        )

    gap_cards = "".join(card(r) for r in gaps) or "<p class='muted'>" + L["no_gaps"] + "</p>"

    unspecified = "".join(
        "<li><b>" + inline(u.get("title")) + "</b> — " + inline(u.get("note")) + " " + evidence_html(u.get("evidence")) + "</li>"
        for u in data["unspecified"]
    )
    unspecified = "<ul class='unspecified'>" + unspecified + "</ul>" if unspecified else "<p class='muted'>" + L["no_unspecified"] + "</p>"
    limits = "".join("<li>" + inline(x) + "</li>" for x in data["limits"])
    limits_html = "<b>" + L["limits"] + "</b><ul>" + limits + "</ul>" if limits else ""
    key_points_html = "<b>" + L["key_points"] + "</b><ul class='keys'>" + key_points + "</ul>" if key_points else ""
    project = html.escape(str(data.get("project", "")))
    repo_path = html.escape(str(repo.get("path", "")))
    author = html.escape(str(data.get("author", "—")))
    method = inline(data.get("method", ""))
    count = L["count"](len(reqs))
    commit_html = html.escape(commit) or "—"
    title = html.escape(str(data.get("title") or L["title"]))
    footer = html.escape(str(data.get("project", ""))) + " — " + title

    return f"""<!doctype html>
<html lang="{lang}"><head><meta charset="utf-8"><title>{title}</title>
<style>
@page {{ size: A4; margin: 18mm 16mm 20mm;
  @bottom-left {{ content: "{footer}"; font: 8pt Helvetica, Arial, sans-serif; color: #6b6f73; }}
  @bottom-right {{ content: "{L['page']} " counter(page) " {L['of']} " counter(pages); font: 8pt Helvetica, Arial, sans-serif; color: #6b6f73; }} }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; font: 9.5pt/1.45 "Helvetica Neue", Helvetica, Arial, sans-serif; color: #1e2a33; }}
h1 {{ font-size: 19pt; margin: 0 0 4pt; line-height: 1.15; }}
h2 {{ font-size: 13pt; margin: 18pt 0 8pt; padding-bottom: 3pt; border-bottom: 1.5pt solid #1e2a33; break-after: avoid; }}
code {{ font: 8.5pt/1.3 Menlo, Consolas, monospace; background: #eef1ec; padding: 0 2pt; border-radius: 2pt; overflow-wrap: anywhere; }}
.muted {{ color: #6b6f73; }}
.band {{ border-top: 5pt solid #1e2a33; padding-top: 10pt; }}
.meta {{ display: grid; grid-template-columns: 34mm 1fr; gap: 3pt 10pt; margin: 10pt 0 0; }}
.meta dt {{ color: #6b6f73; }} .meta dd {{ margin: 0; }} .meta ul {{ margin: 0; padding-left: 12pt; }}
.summary {{ display: grid; grid-template-columns: 42mm 1fr; gap: 14pt; align-items: start; }}
.rate {{ font-size: 30pt; font-weight: 700; line-height: 1; }} .rate small {{ display: block; font-size: 8.5pt; font-weight: 400; color: #6b6f73; margin-top: 4pt; }}
.bar {{ display: flex; height: 9pt; border-radius: 2pt; overflow: hidden; margin: 2pt 0 6pt; }}
.legend {{ list-style: none; padding: 0; margin: 0 0 8pt; display: flex; flex-wrap: wrap; gap: 4pt 14pt; }}
.dot {{ display: inline-block; width: 7pt; height: 7pt; border-radius: 50%; margin-right: 4pt; vertical-align: 0; }}
.keys {{ margin: 0; padding-left: 12pt; }}
table {{ width: 100%; border-collapse: collapse; }}
thead {{ display: table-header-group; }}
th {{ text-align: left; font-size: 8pt; color: #6b6f73; font-weight: 600; border-bottom: 1pt solid #1e2a33; padding: 4pt; }}
td {{ vertical-align: top; padding: 5pt 4pt; border-bottom: 0.5pt solid #c6cfc3; }}
tr {{ break-inside: avoid; }}
td.id, .id {{ font-weight: 700; white-space: nowrap; }}
td.src {{ font-size: 8.5pt; color: #33424c; width: 30mm; }}
.req-text {{ color: #33424c; font-size: 8.5pt; margin-top: 2pt; }}
.badge {{ display: inline-block; color: #fff; font-size: 7.5pt; font-weight: 700; padding: 1.5pt 5pt; border-radius: 8pt; white-space: nowrap; }}
.evidence {{ margin: 0; padding-left: 10pt; }} .evidence li {{ margin: 0 0 1pt; }} .note {{ color: #52616b; font-size: 8.5pt; }}
.gap {{ border: 0.5pt solid #c6cfc3; border-left: 4pt solid; border-radius: 3pt; padding: 7pt 9pt; margin: 0 0 8pt; break-inside: avoid; }}
.gap header {{ display: flex; gap: 6pt; align-items: baseline; }} .gap .right {{ margin-left: auto; white-space: nowrap; }}
.gap dl {{ display: grid; grid-template-columns: 26mm 1fr; gap: 3pt 8pt; margin: 6pt 0 0; }} .gap dt {{ color: #6b6f73; }} .gap dd {{ margin: 0; }}
.sev {{ font-size: 8pt; color: #33424c; margin-left: 4pt; }} .sev-blocker {{ color: #b42318; font-weight: 700; }}
.unspecified {{ padding-left: 12pt; }} .unspecified li {{ margin-bottom: 4pt; }}
</style></head>
<body>
<section class="band">
  <h1>{title}</h1>
  <dl class="meta">
    <dt>{L['project']}</dt><dd><b>{project}</b></dd>
    <dt>{L['repository']}</dt><dd><code>{repo_path}</code></dd>
    <dt>{L['commit']}</dt><dd>{commit_html}</dd>
    <dt>{L['specs']}</dt><dd><ul>{specs}</ul></dd>
    <dt>{L['date']}</dt><dd>{date}</dd>
    <dt>{L['author']}</dt><dd>{author}</dd>
  </dl>
</section>

<h2>{L['summary']}</h2>
<div class="summary">
  <div class="rate">{rate} %<small>{L['rate']}<br>{count}</small></div>
  <div>
    <div class="bar">{bar}</div>
    <ul class="legend">{legend}</ul>
    {key_points_html}
  </div>
</div>
<p class="muted" style="font-size:8pt">{L['rate_note']}</p>

<h2>{L['requirements']}</h2>
<table>
  <thead><tr><th>{L['col_id']}</th><th>{L['col_req']}</th><th>{L['col_source']}</th><th>{L['col_status']}</th><th>{L['col_evidence']}</th></tr></thead>
  <tbody>{rows}</tbody>
</table>

<h2>{L['gaps']}</h2>
{gap_cards}

<h2>{L['unspecified']}</h2>
{unspecified}

<h2>{L['method']}</h2>
<p>{method}</p>
{limits_html}
</body></html>"""


def find_chrome():
    candidates = [
        os.environ.get("CHROME"),
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        shutil.which("google-chrome"),
        shutil.which("google-chrome-stable"),
        shutil.which("chromium"),
        shutil.which("chromium-browser"),
        shutil.which("msedge"),
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    ]
    return next((c for c in candidates if c and Path(c).exists()), None)


def print_pdf(html_path, pdf_path):
    chrome = find_chrome()
    if not chrome:
        return False
    pdf = Path(pdf_path).resolve()
    pdf.unlink(missing_ok=True)
    with tempfile.TemporaryDirectory() as profile:
        proc = subprocess.Popen(
            [chrome, "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
             f"--user-data-dir={profile}", "--no-pdf-header-footer", f"--print-to-pdf={pdf}",
             Path(html_path).resolve().as_uri()],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        # Chrome sometimes keeps running once the PDF is written: wait for a complete file, then stop it.
        deadline = time.monotonic() + 120
        last = -1
        while time.monotonic() < deadline:
            if proc.poll() is not None:
                break
            size = pdf.stat().st_size if pdf.exists() else -1
            if size > 0 and size == last and pdf.read_bytes()[-1024:].rstrip().endswith(b"%%EOF"):
                break
            last = size
            time.sleep(0.5)
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
    return pdf.exists() and pdf.stat().st_size > 0


def main():
    ap = argparse.ArgumentParser(description="Render a code-versus-specifications analysis as a PDF report.")
    ap.add_argument("analysis", nargs="?", help="analysis JSON file ('-' for stdin)")
    ap.add_argument("-o", "--output", help="PDF to write (default: next to the JSON, .pdf)")
    ap.add_argument("--lang", choices=["fr", "en"], help="report language (default: the JSON's `lang`, else fr)")
    ap.add_argument("--html", help="also keep the HTML here")
    ap.add_argument("--example", action="store_true", help="print an example analysis JSON and exit")
    args = ap.parse_args()

    if args.example:
        json.dump(EXAMPLE, sys.stdout, ensure_ascii=False, indent=2)
        sys.stdout.write("\n")
        return
    if not args.analysis:
        ap.error("give the analysis JSON file")
    raw = sys.stdin.read() if args.analysis == "-" else Path(args.analysis).read_text(encoding="utf-8")
    try:
        data = validate(json.loads(raw))
    except (json.JSONDecodeError, Invalid) as e:
        sys.exit(f"invalid analysis: {e}")
    lang = args.lang or data.get("lang") or "fr"
    if lang not in LABELS:
        sys.exit(f"unknown language {lang}: use fr or en")

    output = Path(args.output or (Path(args.analysis).with_suffix(".pdf") if args.analysis != "-" else "report.pdf"))
    html_path = Path(args.html) if args.html else output.with_suffix(".html")
    html_path.write_text(render(data, lang), encoding="utf-8")
    if print_pdf(html_path, output):
        if not args.html:
            html_path.unlink()
        print(output)
    else:
        print(f"No Chrome, Chromium or Edge found (set CHROME=/path/to/browser): HTML written to {html_path}", file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
