# innerrag

Serveur **Graph RAG** 100 % local, écrit en Rust, construit sur **LadybugDB** (base graphe embarquée, fork de Kuzu). Tout tient dans une image Docker, modèles compris : aucun appel à une API externe, aucune dépense par requête.

- **Formats** : PDF, Word (.docx, .doc), PowerPoint (.pptx, .ppt), Markdown (front-matter compris), HTML et texte.
- **Ingestion asynchrone** : extraction du texte, découpage qui suit les titres, embeddings multilingues (e5-small) et extraction d'entités zero-shot (GLiNER). Les entités citées dans un même passage sont reliées. La progression est suivie par job.
- **Recherche** : recherche vectorielle fusionnée (RRF) avec l'expansion par le graphe d'entités. Le serveur renvoie un contexte prêt pour un agent ; il ne génère pas de réponse.
- **Projets** cloisonnés : une base par projet, dans un dossier versionnable dans git.
- **Documents** : statut `DRAFT` / `PUBLISHED`, créateur, tags, ajout, remplacement et suppression.
- **Interfaces** : API REST, serveur **MCP** (HTTP), interface web React et plugin Claude Code (skills).
- **Historique** des appels, avec le volume de contexte renvoyé (tokens estimés), la durée et les erreurs.

## Démarrer

```bash
scripts/build.sh                    # Linux / macOS  (Windows : .\scripts\build.ps1)
docker compose up -d                # ou : docker compose up -d --build
open http://localhost:8080          # interface web
```

Les scripts de build construisent une image Linux pour l'architecture de la machine. Options : `--platform amd64|arm64|all`, `--push`, `--tag`, `--save fichier.tar` pour un transfert hors-ligne. Côté PowerShell, ce sont les mêmes options avec `-Platform`, `-Push`, `-Tag` et `-Save`. Sous Windows, Docker Desktop doit être en mode « Linux containers ». Le premier build prend environ 10 minutes ; l'image pèse 1,6 Go (674 Mo compressée).

Le serveur n'a pas d'authentification (mono-utilisateur) : `docker-compose.yml` ne publie le port que sur `127.0.0.1`. Ne l'exposez que sur un réseau de confiance.

Le réseau n'est nécessaire qu'au build : modèles, ONNX Runtime et extension `vector` de LadybugDB sont intégrés à l'image. Au premier démarrage, un projet `default` est créé.

## Modèles embarqués

| Rôle | Modèle | Fichier |
|---|---|---|
| Embeddings (384 dim, FR/EN et 90+ langues) | `Xenova/multilingual-e5-small` | `onnx/model_quantized.onnx` (118 Mo) |
| Entités (zero-shot, multilingue) | `onnx-community/gliner_multi-v2.1` | `onnx/model_fp16.onnx` (580 Mo) |

On peut les changer au build (`--build-arg EMBED_REPO=… EMBED_FILE=… NER_REPO=… NER_FILE=…`). La variante int8 de GLiNER est à éviter : ses scores s'effondrent (0,2 à 0,5 contre 0,9 et plus en fp16). Un projet retient le modèle d'embedding avec lequel il a été indexé, et le serveur refuse de l'ouvrir avec un autre.

## Configuration (variables d'environnement)

| Variable | Défaut | Rôle |
|---|---|---|
| `INNERRAG_DATA` | `/data` | Racine des données : `projects/<id>/` et `history/` |
| `INNERRAG_DEFAULT_PROJECT` | `default` | Projet de `/mcp` et des appels sans projet |
| `INNERRAG_USER` | `local` | Créateur enregistré sur les documents (mono-utilisateur) |
| `INNERRAG_DEFAULT_STATUS` | `PUBLISHED` | Statut des nouveaux documents (`DRAFT` pour imposer une relecture) |
| `INNERRAG_NER_LABELS` | `person,organization,location,event,product,technology` | Types d'entités extraits |
| `INNERRAG_NER_THRESHOLD` | `0.5` | Seuil de confiance du NER |
| `INNERRAG_CHUNK_SIZE` / `_OVERLAP` | `1000` / `150` | Taille des passages en caractères |
| `INNERRAG_ENTITY_SEED_DISTANCE` | `0.18` | Distance cosinus max. pour qu'une entité proche de la question serve de point de départ |
| `INNERRAG_BUFFER_POOL_MB` | `256` | Mémoire tampon de chaque base ouverte |
| `INNERRAG_MAX_UPLOAD_MB` | `200` | Taille maximale d'une requête (fichier envoyé) |
| `INNERRAG_KEEP_ORIGINALS` | `true` | Garder les fichiers envoyés dans `<projet>/files/` |
| `INNERRAG_DEVICE` | `auto` | `auto`, `cuda` ou `cpu` (voir GPU) |
| `INNERRAG_THREADS` | nb. de CPU | Threads des modèles et de la base |

## Formats et ingestion

| Format | Extraction |
|---|---|
| PDF | Converti en Markdown à partir de la mise en page (`pdftohtml -xml`) : les titres sont déduits des tailles de police, les paragraphes recollés, le code repéré par la police à chasse fixe ; en-têtes, pieds et numéros de page sont retirés et des repères de page sont gardés. Si la mise en page est illisible, on retombe sur le texte brut (`pdftotext`). Les PDF scannés (images seules) sont refusés : pas d'OCR. |
| Word `.docx` | Lu directement : titres (`Titre 1`, `Heading 1`…), listes, tableaux, titre du document |
| PowerPoint `.pptx` | Une section par diapositive, dans l'ordre de la présentation, avec titre et notes de l'orateur |
| `.doc`, `.ppt` (97-2003) | `catdoc` / `catppt` (texte brut) |
| Markdown | Front-matter `title`, `tags`, `status` ; titres `#` |
| HTML, texte | HTML converti en Markdown simple ; texte tel quel |

Le texte extrait est conservé en entier (Markdown) pour la lecture, et l'original est gardé dans `files/` du projet (désactivable avec `INNERRAG_KEEP_ORIGINALS=false`). L'interface a un **lecteur** pleine page : sommaire par chapitres et texte mis en forme. Les numéros de page ouvrent le PDF original à la bonne page, et un onglet donne les passages indexés. Le texte est découpé en suivant les titres et, pour un PDF, sans chevaucher les pages : chaque passage connaît sa page, ce qui permet aux résultats de recherche de pointer vers « page 41 ». Chaque passage commence par son fil de titres (`Chapitre 3 › Installation`), ce qui garde le contexte au moment de la recherche.

**Toute ingestion est asynchrone** : l'appel renvoie `202` et un job, traité en file, un document à la fois. On suit sa progression (extraction, vectorisation, entités, écriture) sur `GET /api/jobs/{id}`, et l'interface l'affiche en haut de la page Documents. Ajouter `?wait=true` donne le comportement synchrone. Ordre de grandeur sur CPU (8 cœurs, arm64) : environ 0,25 s par passage, soit 8 à 9 minutes pour un livre PDF de 728 pages (1 959 passages). Les jobs sont gardés en mémoire : un redémarrage du serveur pendant une ingestion l'interrompt, et il faut renvoyer le fichier.

```bash
curl -F "file=@rapport.pdf" -F "tags=finance,2026" -F "status=DRAFT" \
     http://localhost:8080/api/projects/mon-projet/documents/upload
```

## GPU (NVIDIA)

Par défaut, l'image tourne sur CPU. Une variante **CUDA** fait tourner les deux modèles (embeddings et entités) sur une carte NVIDIA :

```bash
scripts/build.sh --gpu                 # ou .\scripts\build.ps1 -Gpu  → image innerrag:cuda (linux/amd64)
docker run -d --gpus all -p 8080:8080 -v "$PWD/data:/data" innerrag:cuda
# ou : docker compose --profile gpu up -d innerrag-gpu
```

- Il faut un hôte Linux, ou Windows avec WSL2, une carte NVIDIA, un pilote récent (CUDA 12) et le NVIDIA Container Toolkit. **Docker sur macOS n'a pas accès au GPU** (Apple Silicon compris) : l'image CPU reste la seule option sur Mac.
- `INNERRAG_DEVICE` vaut `auto` par défaut (GPU s'il est utilisable, sinon CPU, sans erreur). Les autres valeurs sont `cuda` (le GPU est exigé, échec sinon) et `cpu`. Le périphérique réellement utilisé est indiqué au démarrage, dans `/api/config` et dans l'interface.
- L'image CUDA est plus lourde (+2 Go environ : ONNX Runtime GPU, cuBLAS, cuDNN). L'extraction d'entités, qui représente l'essentiel du temps d'ingestion, est celle qui gagne le plus.

## Projets et partage via git

Chaque projet est un dossier `/data/projects/<id>/` :

```
innerrag.lbdb   la base LadybugDB (un seul fichier)
project.json    titre, description, modèle d'embedding
files/          les fichiers originaux (PDF, Word…)
.gitignore      fichiers temporaires
```

Le serveur fait un checkpoint après chaque écriture : le fichier `.lbdb` est toujours cohérent et peut être commité sans arrêter le serveur. Pour partager un projet dans un dépôt :

```bash
docker run -d -p 8080:8080 --user "$(id -u):$(id -g)" \
  -v "$PWD/.innerrag:/data/projects/mon-projet" innerrag
git add .innerrag && git commit -m "Base de connaissances"
```

C'est un fichier binaire : git ne sait pas fusionner deux modifications parallèles. Convenez de qui ingère à quel moment.

## API REST

Toutes les routes de projet sont sous `/api/projects/{project}`.

| Méthode | Route | Rôle |
|---|---|---|
| GET | `/api/health`, `/api/config` | État, modèles, configuration |
| GET, POST | `/api/projects` | Lister, créer (`{"id","title","description"}`) |
| GET, PATCH, DELETE | `/api/projects/{p}` | Lire, renommer, supprimer |
| GET | `…/stats`, `…/tags` | Compteurs, tags |
| GET, POST | `…/documents` | Lister (`?status=&tag=&q=`), ajouter du texte JSON (409 si l'id existe) |
| POST | `…/documents/upload` | Ajouter un fichier (multipart : `file`, et en option `title`, `id`, `tags`, `status`, `source`) |
| GET, PUT, PATCH, DELETE | `…/documents/{id}` | Lire, remplacer par du texte (réindexe), modifier les métadonnées, supprimer |
| PUT | `…/documents/{id}/upload` | Remplacer par un fichier |
| GET | `…/documents/{id}/content` | Texte complet en Markdown (avec repères `<!-- page N -->` pour un PDF) |
| GET | `…/documents/{id}/passages?offset=&limit=` | Passages indexés, paginés, avec leur page |
| GET | `…/documents/{id}/file` | Fichier original (`#page=N` ouvre un PDF à la page voulue) |
| GET | `/api/jobs?project=`, `/api/jobs/{id}` | Ingestions en cours et récentes |
| DELETE | `/api/jobs/{id}` | Annuler une ingestion en attente ou en cours (rien n'est écrit) |
| GET | `…/entities`, `…/entities/{id}` | Entités, voisinage et passages |
| GET | `…/graph`, `…/graph/neighbourhood/{id}` | Sous-graphes pour la visualisation |
| POST | `…/search` | `{"query","k","tags","include_drafts","use_graph"}` |
| POST | `…/cypher` | Cypher en lecture seule |
| GET | `/api/history` | Historique (`?hours=&channel=&operation=&project=&errors=`) |

Corps d'un document texte : `{"text","title?","id?","source?","tags?":[],"status?":"DRAFT|PUBLISHED","metadata?":{}}`. Les ajouts et remplacements renvoient `202` avec le job ; `?wait=true` renvoie le rapport d'ingestion.

## MCP

- `POST /mcp` : projet par défaut ; chaque outil accepte un argument `project`, et `list_projects` liste les projets.
- `POST /mcp/{project}` : lié à un projet.

Outils : `search_knowledge`, `explore_entity`, `ingest_document` (texte), `ingest_file` (fichier en base64), `ingestion_status`, `list_documents`, `graph_stats`, `run_cypher`. Les ingestions répondent tout de suite avec un job, sauf avec `wait: true` (attente jusqu'à 2 minutes).

```bash
claude mcp add --transport http innerrag http://localhost:8080/mcp/mon-projet
```

## Plugin Claude Code (skills)

Le dépôt est aussi une marketplace Claude Code. Le plugin `innerrag` apporte le MCP et six skills : recherche, ingestion, documents, projets, exploration du graphe et consommation. Ils s'appuient sur un petit CLI Python sans dépendance (`plugins/innerrag/scripts/innerrag.py`).

```
/plugin marketplace add /chemin/vers/innerrag
/plugin install innerrag@innerrag
```

Variables lues par le plugin : `INNERRAG_URL` (défaut `http://localhost:8080`) et `INNERRAG_PROJECT`.

## Développement

Rust n'est pas requis sur la machine : tout se compile dans un conteneur (Debian trixie, GCC 14, nécessaire aux en-têtes C++20 de LadybugDB).

```bash
docker build --target models -t innerrag-models .           # modèles, une fois
cd ui && npm install && npm run dev                          # UI sur :5173, proxy vers :18080
```

Code : `src/` (serveur), `ui/` (React + Vite), `plugins/innerrag/` (plugin Claude Code), `docs/PLAN.md` (plan initial). La maquette de l'interface a été conçue dans Claude Design.
