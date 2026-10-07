# innerrag — serveur Graph RAG (Rust + LadybugDB), 100 % local dans Docker

## Contexte

Objectif : un serveur Graph RAG autonome, sans aucune dépense externe (pas d'API payante, pas de service tiers au runtime).
Il ingère des documents texte (FR/EN), en tire un graphe de connaissances stocké dans **LadybugDB** (fork de Kuzu, embarqué),
et renvoie à un agent client (Claude, etc.) le **contexte pertinent** (chunks + sous-graphe). C'est l'agent qui rédige la réponse.
Tout tourne dans une seule image Docker, qui contient les modèles ONNX et une interface **React** pour consulter l'état du graphe.

Décisions validées :
- Extraction d'entités : **GLiNER local** ; relations = co-occurrence dans un même chunk
- Interfaces : **REST + MCP**
- Pas de LLM génératif : on renvoie seulement le contexte
- Multilingue FR/EN
- UI React incluse dans l'image

Projet vide (seuls `Cargo.toml`, `build.rs`, `.gitignore` et `git init` existent déjà). Rust n'est pas installé sur la machine : tout se compile dans Docker. Node 22 est disponible en local pour l'UI.

## Briques techniques (vérifiées)

| Brique | Choix | Notes |
|---|---|---|
| Base graphe | crate `lbug = "=0.21.2"` | télécharge `liblbug` statique pré-compilé (release GitHub v0.21.2, linux x86_64/aarch64). `Database` est Send+Sync, `Connection<'a>` aussi. On utilise `Box::leak` pour obtenir `Connection<'static>`. |
| Index vectoriel | extension `vector` de LadybugDB (HNSW, cosine) | elle n'est pas intégrée au binaire : on la télécharge **au build Docker** (`INSTALL vector`, dans `$HOME/.lbdb/extension/0.21.0/linux_*/`), puis `LOAD vector` hors-ligne au runtime. Il faut `-rdynamic` (`build.rs`). |
| Embeddings | `Xenova/multilingual-e5-small`, `onnx/model_quantized.onnx` (~118 Mo, 384 dim) | préfixes `query: ` / `passage: `, mean pooling + normalisation L2, code maison sur `ort` + `tokenizers` |
| NER | `onnx-community/gliner_multi-v2.1`, `onnx/model_fp16.onnx` (~580 Mo ; la version int8 dégrade fortement les scores) | crate `gline-rs 1.1` (lib `gliner`), mode span, zero-shot avec des labels configurables |
| Runtime ONNX | `ort =2.0.0-rc.9` (version imposée par gline-rs) + feature **`load-dynamic`** | `libonnxruntime.so` 1.20.1 (release Microsoft) est copiée dans l'image et pointée par `ORT_DYLIB_PATH`. Le chargement dynamique évite les collisions de symboles (re2, abseil…) avec `liblbug.a`. |
| HTTP | `axum 0.8`, `tower-http` (fichiers statiques, CORS, traces) | |
| MCP | JSON-RPC 2.0 « streamable HTTP » écrit à la main sur `POST /mcp` (réponses `application/json`) | `initialize`, `ping`, `tools/list`, `tools/call` |
| UI | Vite + React + TypeScript + `react-force-graph-2d` | compilée dans un stage Node, servie par axum sur `/` |

Le choix des modèles se fait par `ARG` Docker : on peut passer aux variantes fp32 si la qualité ne suffit pas.

## Modèle de graphe (LadybugDB)

```cypher
CREATE NODE TABLE Document(id STRING PRIMARY KEY, title STRING, source STRING, metadata STRING, created_at TIMESTAMP)
CREATE NODE TABLE Chunk(id STRING PRIMARY KEY, doc_id STRING, idx INT64, text STRING, embedding FLOAT[384])
CREATE NODE TABLE Entity(id STRING PRIMARY KEY, name STRING, label STRING, embedding FLOAT[384])
CREATE REL TABLE HAS_CHUNK(FROM Document TO Chunk)
CREATE REL TABLE NEXT(FROM Chunk TO Chunk)
CREATE REL TABLE MENTIONS(FROM Chunk TO Entity, score DOUBLE, surface STRING)
CREATE REL TABLE RELATED(FROM Entity TO Entity, weight INT64)   -- sens canonique a.id < b.id
CALL CREATE_VECTOR_INDEX('Chunk','chunk_vec','embedding', metric:='cosine')
CALL CREATE_VECTOR_INDEX('Entity','entity_vec','embedding', metric:='cosine')
```
- `Entity.id` = `label:nom_normalisé` (minuscules, espaces compactés, ponctuation de bord retirée). Le nom affiché est la première forme rencontrée.
- Les écritures passent par des `UNWIND $rows` (listes de STRUCT en paramètre) dans une transaction explicite par document, et non une requête par ligne (trop lent d'après la doc lbug).
- Les entités nouvelles sont créées par `CREATE` avec leur embedding. On évite `SET` sur une propriété indexée.

## Pipelines

**Ingestion** (`POST /api/documents`, outil MCP `ingest_document`) :
1. Découpage en phrases, regroupées en chunks d'environ 1000 caractères avec 150 de chevauchement. Une phrase trop longue est coupée au mot.
2. Embedding des chunks (`passage: `), par lots de 16.
3. NER GLiNER par lots de 8. Labels par défaut : `person, organization, location, event, product, technology`, seuil 0.5. Les deux sont surchargeables par variable d'environnement ou par requête.
4. Dédoublonnage des entités par chunk, puis embedding (`query: nom`) des seules entités nouvelles.
5. Transaction : Document, Chunks, HAS_CHUNK, NEXT, nouvelles entités, MENTIONS, puis `MERGE RELATED` avec des poids pré-agrégés (`ON MATCH SET weight = weight + r.w`).
6. Si l'id du document existe déjà, on le supprime d'abord (ré-ingestion idempotente).

**Suppression** : on décrémente `RELATED` par paires agrégées, on supprime les arêtes de poids ≤ 0, puis les chunks et le document (`DETACH DELETE`), puis les entités orphelines.

**Recherche locale Graph RAG** (`POST /api/search`, outil MCP `search_knowledge`) :
1. Embedding de la question (`query: `).
2. Vecteur → top-2k chunks (`QUERY_VECTOR_INDEX`).
3. Entités « graines » : vecteur sur `Entity` (sous un seuil de distance) et NER sur la question (correspondance exacte d'id).
4. Expansion : chunks qui mentionnent les graines et voisins à 1 saut via `RELATED`, triés par poids.
5. Fusion RRF des classements vecteur et graphe, puis top-k.
6. Réponse : chunks (texte, doc, scores, entités), entités, relations, et un champ `context` en markdown prêt pour un LLM.

## Fichiers à créer

```
Cargo.toml, build.rs                 (déjà présents)
src/main.rs        CLI : `serve` (défaut) | `install-extensions` (utilisé au build Docker)
src/config.rs      variables d'env INNERRAG_* (bind, db path, models dir, ui dir, buffer pool, labels, seuils, chunking, threads)
src/db.rs          Graph : ouverture, LOAD vector, schéma idempotent, writer Mutex<Connection>, lecteurs, helpers Value<->JSON
src/embed.rs       Embedder e5 (ort + tokenizers, padding/troncature 512, mean pooling)
src/ner.rs         wrapper GLiNER<SpanMode> (Mutex), lots, normalisation des entités
src/chunk.rs       découpage en chunks (+ tests unitaires)
src/ingest.rs      pipeline d'ingestion et suppression
src/search.rs      recherche hybride vecteur + graphe, construction du contexte
src/api.rs         routes REST
src/mcp.rs         endpoint MCP JSON-RPC
ui/                Vite/React/TS
Dockerfile         multi-stage : ui (node:22) | models (téléchargement HF + onnxruntime) | builder (rust:1-bookworm, cmake, g++, libssl-dev) | runtime (debian:bookworm-slim)
docker-compose.yml volume /data, port 8080
README.md          (FR) usage, API, MCP, configuration
```

### API REST
`GET /api/health` · `GET /api/stats` · `GET|POST /api/documents` · `GET|DELETE /api/documents/:id` ·
`GET /api/entities?q=&label=&limit=` · `GET /api/entities/:id` (voisins + chunks) ·
`GET /api/graph?limit=&min_weight=&label=` (top entités + arêtes entre elles) · `POST /api/search` ·
`POST /api/cypher` (lecture seule, vérifiée par `PreparedStatement::is_read_only`) · `GET /api/config`

### Outils MCP
`search_knowledge`, `ingest_document`, `explore_entity`, `list_documents`, `graph_stats`, `run_cypher` (lecture seule)

### UI React (servie sur `/`)
- **Vue d'ensemble** : compteurs (documents, chunks, entités, relations), répartition par label, modèles chargés
- **Graphe** : graphe de forces des entités, coloré par label, avec filtres label et poids min. Un clic ouvre un panneau latéral (voisins, extraits) ; un double-clic déplie le voisinage.
- **Documents** : liste, ajout (texte collé ou fichier .txt/.md lu côté navigateur), suppression, lecture des chunks
- **Recherche** : banc d'essai de la recherche RAG (scores vecteur/graphe, sous-graphe trouvé)
- **Cypher** : console en lecture seule avec résultats en tableau
- Thème clair/sombre, saisie de la clé API si le serveur répond 401

## Dockerfile (grandes lignes)
- `models` : `curl` des fichiers HF et de `onnxruntime-linux-{x64,aarch64}-1.20.1.tgz` selon `TARGETARCH`
- `builder` : `LBUG_VERSION=0.21.2` ; dépendances mises en cache (main factice) puis `cargo build --release`
- `runtime` : `libssl3 ca-certificates libatomic1 libgomp1`, utilisateur `app`, binaire + `/models` + `/app/ui` + `libonnxruntime.so` ; `RUN innerrag install-extensions` (réseau au build seulement) ; `VOLUME /data` ; `EXPOSE 8080`
- Image visée : environ 1 Go

## Risques et plans B
- **Index vectoriel non mis à jour à l'insertion** (doc LadybugDB muette sur ce point) → test dès le premier build. Plan B : reconstruire l'index après chaque ingestion (`DROP_VECTOR_INDEX`/`CREATE_VECTOR_INDEX`), ou recherche cosinus en mémoire.
- **`DETACH DELETE` refusé sur un nœud indexé** → même plan B (on retire l'index, on supprime, on le recrée).
- **ort rc.9 + tokenizers 0.21** : versions figées par gline-rs ; si conflit de compilation, on utilise directement la version de `tokenizers` résolue par gline-rs.
- **Paramètres STRUCT dans UNWIND** : si le binding Rust les gère mal, on passe des listes parallèles de primitives, ou un `COPY` depuis un CSV/Parquet temporaire.

## Vérification
1. `cargo test` (chunking, normalisation) dans le stage builder : `docker build --target builder`
2. `docker compose up --build`, puis `curl /api/health` et `/api/stats`
3. Ingérer 2–3 textes FR/EN (ex. articles Wikipédia collés) et vérifier `/api/stats`, `/api/graph` (entités et relations non vides)
4. `POST /api/search` sur une question qui demande un saut (entité A → B) : le chunk de B doit remonter grâce au graphe
5. Supprimer un document : les compteurs et les orphelins sont nettoyés
6. Redémarrer le conteneur avec le réseau coupé (`--network none`) : le service démarre (LOAD vector hors-ligne) et les données persistent dans le volume
7. MCP : `claude mcp add --transport http innerrag http://localhost:8080/mcp`, puis appeler `search_knowledge` depuis Claude Code
8. UI : ouvrir `http://localhost:8080`, parcourir les 5 vues (vérification via Chrome) en clair et en sombre

## Évolutions par rapport au plan initial

- **Debian trixie** au lieu de bookworm : les en-têtes C++ de LadybugDB exigent GCC ≥ 13 (`<format>`).
- **GLiNER fp16** au lieu d'int8 : l'int8 fait chuter les scores (0,2 à 0,5 contre 0,85 à 0,99).
- Les risques d'index vectoriel sont levés (testés sur 0,21.2) : insertion et `DETACH DELETE` mettent l'index à jour.
- **Projets** : une base LadybugDB par dossier `data/projects/<id>/`, avec un checkpoint après chaque écriture pour pouvoir la commiter dans git. `project.json` retient le modèle d'embedding utilisé.
- **Documents** : statut DRAFT/PUBLISHED (les brouillons sont indexés mais exclus de la recherche par défaut), créateur, tags, PUT (remplacement) et PATCH (métadonnées).
- **MCP** : `/mcp` (projet par défaut, argument `project`) et `/mcp/{project}`.
- **Historique des appels** (`data/history/calls.jsonl`) : canal, opération, durée, tokens de contexte renvoyés.
- **UI** conçue dans Claude Design (6 écrans), puis implémentée en React.
- **Plugin Claude Code** (marketplace dans le dépôt) : MCP et 6 skills appuyés sur un CLI Python.
- **Scripts de build** `scripts/build.sh` et `scripts/build.ps1`.
- Pas d'authentification (mono-utilisateur) ; compose publie le port sur 127.0.0.1 seulement.
- **Formats** : PDF (`pdftotext`), DOCX et PPTX (lus directement en Rust : titres, listes, tableaux, diapositives dans l'ordre avec leurs notes), DOC et PPT (`catdoc`/`catppt`), Markdown (front-matter), HTML et texte.
- **Découpage guidé par les titres** : chaque passage est préfixé par son fil de titres.
- **Ingestion asynchrone par défaut** : file de jobs (un document à la fois), progression par étape, `?wait=true` pour le mode synchrone, outils MCP `ingest_file` et `ingestion_status`, indicateur global dans l'UI. Mesure : livre PDF de 728 pages = 1 959 passages, environ 9 minutes sur 8 cœurs arm64. La vectorisation prend environ 25 s ; l'extraction d'entités (GLiNER) fait l'essentiel du temps.
