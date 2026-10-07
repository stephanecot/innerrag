---
name: innerrag-spec-check
description: Compare a codebase with the specifications stored in an innerrag knowledge base (LLD, functional specs, requirements) and produce a PDF compliance report with a fixed template, in French or English. Use when the user asks "compare le code aux specs", "vérifie la conformité du code au LLD", "écarts entre le code et la spécification", "rapport de conformité", "check the code against the spec", "spec compliance report", or wants to know what a spec requires that the code does not do.
allowed-tools: Bash(python3 *innerrag.py*), Bash(python3 *spec_report.py*), Bash(git rev-parse*), Bash(git status*), Bash(git log*), Read, Grep, Glob, Write
---

# Compare a codebase with innerrag specifications

`CLI = python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py"` (env: `INNERRAG_URL`, `INNERRAG_PROJECT`; `-p <project>` to switch).
`REPORT = python3 "${CLAUDE_SKILL_DIR}/../../scripts/spec_report.py"`

The report always has the same layout, produced by `spec_report.py` from a JSON analysis. **Never write
the HTML or PDF yourself**: your job is the analysis, the script does the layout.

## 1. Agree on the scope

Settle these four points, asking only for what you cannot infer:

- **Specifications**: by default the project's documents tagged `spec` (`$CLI docs --tag spec`). Otherwise
  the documents the user names: find their ids with `$CLI docs --q "<words of the title>"`.
- **Codebase**: the current directory unless another path is given.
- **Language of the report**: French if the user writes in French, English if in English, unless they ask
  otherwise. It applies to the labels (`--lang`) and to every text you write in the JSON.
- **Output**: `./rapport-conformite-<date>.pdf` (fr) or `./spec-compliance-<date>.pdf` (en) unless a path
  is given. Keep the JSON next to it (same name, `.json`): it lets the user re-render the PDF in the other
  language.

## 2. Read the specifications in full

```bash
$CLI content <doc id>        # Markdown; PDFs keep <!-- page N --> markers
```

Read each specification entirely: do not rely on search snippets for this step. Note each requirement's
section heading and page (from the `<!-- page N -->` marker above it).

## 3. Extract the requirements

A requirement is one verifiable statement of what the system must do or hold: a business rule, a data
field and its constraints, a screen element, an interface or API, a validation, an error case, a
permission, a calculation, an integration, a non-functional constraint that the code can show.

- Leave out context, objectives, ROI, contacts, planning and document history.
- One requirement per statement; split "and" lists only when the parts can be met separately.
- Number them `REQ-001`, `REQ-002`… in the order of the specification. With several specifications,
  continue the numbering.
- `title`: a short label (under 12 words), in the report language.
- `text`: the requirement as the specification states it (quote, or close paraphrase in its language).
- `source`: `doc_id`, `doc_title`, and `section` and/or `page` when known.
- More than about 80 requirements: group closely related statements (for example the fields of one
  table) into one requirement each, and list the members in `text`.

## 4. Check each requirement in the code

Look for evidence before judging: Grep/Glob for the identifiers, field names, routes, messages, table
names and constants the requirement implies, in the source language of the code (often English even
for a French spec); then Read the relevant code to see what it really does.

| `status` | When |
|---|---|
| `compliant` | Implemented as specified. Give the evidence. |
| `partial` | Implemented, but with differences or missing parts. Say which in `found`. |
| `non_compliant` | Absent, or the code contradicts the specification. |
| `not_verifiable` | The code alone cannot tell (runtime data, external system, configuration not in the repository, wording of a document…). Say why in `found`. |

For `partial` and `non_compliant`, set `severity`:

- `blocker`: wrong data or results, security, or a feature unusable;
- `major`: a functional gap;
- `minor`: naming, wording, presentation, a missing secondary check.

Rules:

- **Evidence is file paths relative to the repository root, with line numbers** (`{"file", "line", "note"}`).
  Only cite what you have read. Never invent a file, a line or a behaviour.
- When nothing is found, say where you looked in `found` ("no route, handler or test mentions …").
- `found` describes what the code does; `recommendation` (for gaps) says what to change, concretely.
- When a requirement is ambiguous, pick the most reasonable reading and say so in `found`.

Other specifications in innerrag can clarify a term: `$CLI search "<term>" --map -k 5`.

## 5. Note what the code does beyond the specifications

`unspecified`: notable features, endpoints or rules present in the code but in no specification (at most
about 10, the most significant). Ignore plumbing, tooling and tests.

## 6. Write the analysis JSON

`$REPORT --example` prints the exact shape, filled with an example. Fill in:

- `lang`: `fr` or `en`;
- `project`: the innerrag project;
- `repository`: `path` (absolute), `branch` (`git rev-parse --abbrev-ref HEAD`), `commit`
  (`git rev-parse --short HEAD`), `dirty` (true when `git status --porcelain` prints anything);
  leave branch and commit out when the folder is not a git repository;
- `specs`: `id`, `title`, `source` of each specification used;
- `generated_at`: now, ISO 8601;
- `author`: `Claude Code (innerrag-spec-check)`;
- `summary`: 3 to 6 key points for a reader in a hurry, most important first;
- `requirements`, `unspecified`;
- `method`: one or two sentences on how you checked;
- `limits`: what you could not check (no execution, missing modules, assumptions).

Write it with the Write tool, then render:

```bash
$REPORT <analysis.json> -o <report.pdf> --lang fr|en
```

If the script exits with code 2, no Chrome, Chromium or Edge was found: give the user the HTML path it
printed (it prints to PDF from any browser), or ask them to set `CHROME=/path/to/browser`.

## 7. Report back

Give the path of the PDF (and of the JSON), the compliance rate, the counts per status and the three
most serious gaps, in two or three sentences.
