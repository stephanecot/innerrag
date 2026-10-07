---
name: innerrag-explore
description: Explore the innerrag knowledge graph — entities (people, organizations, places, products, technologies…), what they co-occur with, the passages that mention them, and read-only Cypher queries on the LadybugDB graph. Use for "qui est lié à", "quelles entités", "montre le graphe autour de", relationship questions, or graph statistics.
allowed-tools: Bash(python3 *innerrag.py*)
---

# Explore the innerrag graph

`CLI = python3 "${CLAUDE_SKILL_DIR}/../../scripts/innerrag.py"` (env: `INNERRAG_URL`, `INNERRAG_PROJECT`; `-p <project>`).

| Task | Command |
|---|---|
| Find entities | `$CLI entities --q "curie" [--label person] [--limit 20]` |
| One entity: neighbours (by co-occurrence weight) and passages | `$CLI entity "Marie Curie"` or `$CLI entity "person:marie curie"` |
| Counts and labels | `$CLI stats` |
| Read-only Cypher | `$CLI cypher "MATCH (e:Entity) RETURN e.label, count(*)"` |

Graph schema (per project):
```
(:Document {id, title, source, metadata, status, creator, tags, created_at, updated_at})
  -[:HAS_CHUNK]->(:Chunk {id, doc_id, idx, text, embedding})
  -[:MENTIONS {score, surface}]->(:Entity {id, name, label, embedding})
(:Chunk)-[:NEXT]->(:Chunk)
(:Entity)-[:RELATED {weight}]->(:Entity)   // co-occurrence in the same passage; weight = passages
```

Notes:
- Relations are co-occurrences, not typed facts: "A ×3 B" means they appear together in 3
  passages. Read the passages before asserting what the link is.
- Never return `embedding` columns (huge). Writes, COPY, LOAD, INSTALL, ATTACH, EXPORT are refused.
- Useful queries: strongest relations
  `MATCH (a:Entity)-[r:RELATED]->(b:Entity) RETURN a.name, b.name, r.weight ORDER BY r.weight DESC LIMIT 20`;
  documents of a tag `MATCH (d:Document) WHERE list_contains(d.tags, 'rh') RETURN d.id, d.title, d.status`.
