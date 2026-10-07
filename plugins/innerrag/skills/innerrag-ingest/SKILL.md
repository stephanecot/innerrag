---
name: innerrag-ingest
description: Add or replace documents in an innerrag knowledge base — PDF, Word (.docx/.doc), PowerPoint (.pptx/.ppt), HTML, Markdown and text files, whole folders, web pages by URL, or text from the conversation. Use when the user asks to ingest, index, import, add, upload, re-index or replace documents ("ajoute ce PDF", "indexe le dossier docs", "importe la présentation", "remplace la version", "mets à jour la base"), with optional status (draft/published) and tags.
allowed-tools: Bash(python3 *innerrag.py*)
---

# Ingest documents into innerrag

`CLI = python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py"` (env: `INNERRAG_URL`, `INNERRAG_PROJECT`; `-p <project>` to switch).

## Files and folders (add or replace)

```bash
python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py" -p <project> ingest docs/ "rapport annuel.pdf" deck.pptx --tags rh,procedures --status draft
```

- Formats: `.pdf .docx .pptx .doc .ppt .md .markdown .html .txt` (plus `.rst .adoc .org`). The
  server converts everything to markdown: PDFs from their layout (headings from font sizes,
  paragraphs, code blocks, running headers removed, page numbers kept), Word headings, lists and
  tables, slides in order with speaker notes. The original file is kept for reading.
  Scanned PDFs (images only) have no text: the server refuses them (no OCR).
- Folders are walked recursively. Each document id is the file path relative to the current
  folder (or `--root`): running the same command again **replaces** documents in place
  (creator and creation date are kept).
- Title: `--title`, else the document's own title (Word/PowerPoint properties, markdown front
  matter `title`, first `# heading`), else the file name.
- Markdown front matter (`title`, `tags`, `status`) is honoured when the options are not given.
- Status: `--status draft|published`; default is the server setting (usually PUBLISHED).
  Drafts are indexed and visible in the UI but excluded from search unless asked.

## Web pages and online files

```bash
python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py" -p <project> ingest-url https://example.com/docs/install https://example.com/spec.pdf --tags web
```

- The innerrag server downloads each address (it must be reachable from the server), then
  treats it like an uploaded file: a page keeps its main content only (no menus, sidebars,
  footers, forms, tables of contents), a PDF or Word file is converted as usual.
- The document id is derived from the address (`url/example.com/docs/install`): ingesting the
  same address again **replaces** it. The source is the address; the title is the page's own
  title unless `--title` is given.

## Text from the conversation

```bash
cat note.md | python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py" ingest --stdin --title "Compte rendu du 3 octobre" --id cr-2026-10-03 --tags reunion
```

## Background processing

Ingestion is asynchronous on the server; the CLI queues the files, then waits and prints
progress on stderr (`embedding 448/1959`, `entities 224/1959`…). Local models on CPU take
roughly 0.25 s per chunk: a 700-page book (~2 000 chunks) needs about 8–10 minutes.
For large batches use `--no-wait` and check later with `$CLI jobs`. Files are processed one
at a time in submission order.

## After ingesting

Report per document: chunks, entities, status. Drafts need publishing
(`doc-set <id> --status published`, documents skill) before they show up in searches.
Ask before ingesting more than ~50 large files or anything outside the folder the user named.
