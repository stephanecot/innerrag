# Idées pour la suite d'innerrag

Réflexion menée le 7 octobre 2026 par un sous-agent positionné en expert IA agentique et RAG, à partir de l'état du produit à cette date :

- recherche hybride (vecteur, graphe, BM25) avec seuil et reclassement ;
- évaluation ;
- ingestion asynchrone de PDF, Word et PowerPoint ;
- dossiers surveillés ;
- MCP, interface et skills.

Contrainte rappelée : tout est local, sur CPU, sans LLM génératif côté serveur. C'est l'agent client (Claude Code, etc.) qui raisonne.

## Point bloquant avant le travail en équipe

Un fichier `.lbdb` binaire versionné dans git ne se fusionne pas. Si deux personnes ingèrent en parallèle, l'une perd son travail. Voir l'idée 7.

## Les 10 idées, classées par rapport valeur/effort

### 1. Boucle de citation et carte des lacunes (effort S)

- **Problème** : le serveur ne sait ni quels passages ont vraiment servi à l'agent, ni quelles questions sont restées sans réponse.
- **Comment** :
  - un outil MCP `cite(query_id, chunk_ids, outcome)`, ou un hook Stop du plugin Claude Code qui relève les passages cités dans la réponse finale ;
  - des entrées `eval.json` sont proposées automatiquement (question → passage cité) et validées dans l'interface ;
  - les questions sous le seuil sont regroupées (embeddings + regroupement sur CPU), ce qui donne une page « lacunes documentaires » triée par fréquence.
- **Risque** : signal biaisé si l'agent ne cite pas. C'est au hook de le faire, pas à la bonne volonté de l'agent.

### 2. Contexte à budget, en deux temps, avec mémoire de session (effort S)

- **Problème** : renvoyer les k meilleurs passages coûte des milliers de tokens, souvent redondants d'un appel à l'autre.
- **Comment** :
  - `search_knowledge(budget=1500, mode="map")` renvoie une carte compacte : identifiant, fil de titres, la meilleure phrase (notée par le cross-encoder), score, fraîcheur ;
  - `read_chunks(ids, window=±1)` déplie le texte complet en suivant les arêtes `NEXT` ;
  - avec un `session_id`, le serveur remplace les passages déjà envoyés par « [c123 déjà fourni] » ;
  - les passages trop proches de ceux déjà retenus sont écartés (MMR).
- **Risque** : davantage d'allers-retours. Il faut mesurer les tokens totaux par tâche, pas par appel ; l'historique des appels le permet déjà.

### 3. En-tête de confiance par passage (effort S)

- **Problème** : l'agent traite un brouillon de 2021 comme une spécification à jour.
- **Comment** : on ajoute en une ligne la date et l'auteur de la dernière modification (`git log` sur `files/`), le statut, les contradictions ouvertes (idée 5) et la date de dernière vérification. Par exemple : `[spec-api.pdf p.12, modifié il y a 14 mois, 1 contradiction ouverte]`. Cela sert de critère de départage au classement, pas de bonus fort.
- **Risque** : ancien ne veut pas dire faux. Mieux vaut informer l'agent que pénaliser le passage.

### 4. L'agent écrit des notes, en brouillon, via git (effort S/M)

- **Problème** : ce que l'agent découvre en cours de session (correction, FAQ, décision) est perdu.
- **Comment** :
  - un outil `propose_note(kind, title, body, cites=[chunk_ids])` écrit une note Markdown dans `files/notes/`, avec en front-matter l'agent, la session, les sources et l'empreinte des passages sources ;
  - la note est en `DRAFT`, sur une branche git, avec une file de relecture dans l'interface ;
  - une fois publiée, elle est reliée à ses sources par des arêtes `DERIVED_FROM`. Si une source change, la note passe automatiquement « à revérifier ».
- **Risque** : l'agent recycle ses propres erreurs. Parades : citations obligatoires, jamais de publication automatique.

### 5. Détecteur de contradictions sans LLM, arbitré par l'agent (effort M)

- **Problème** : deux documents disent des choses différentes et personne ne le voit.
- **Comment** :
  - on repère des paires de passages candidats : documents différents, similarité supérieure à 0,88, au moins deux entités communes ;
  - on compare des « faits saillants » extraits par expressions régulières : nombres et unités, dates, versions, obligatoire/facultatif, négations ;
  - l'agent arbitre avec `list_tensions` puis `resolve_tension(id, verdict=contradiction|complémentaire|remplacé, winner)` ;
  - le verdict est stocké en arête `CONFLICTS` ou `SUPERSEDES` avec sa provenance. Un passage remplacé sort du contexte par défaut.
- **Risque** : beaucoup de faux positifs au début. On ne montre que les candidats les plus forts.

### 6. Pont entre la doc et le code, avec détection de dérive (effort M)

- **Problème** : pour une équipe qui code avec Claude Code, c'est surtout sur le code que la doc devient fausse.
- **Comment** :
  - on extrait de la doc les éléments de code (texte entre backticks, noms en CamelCase, chemins, routes `GET /route`, variables d'environnement, options de CLI) comme entités `Symbol` ;
  - on indexe le dépôt (ctags ou tree-sitter) et on crée des arêtes `REFERS_TO` avec un statut « existe / disparu » ;
  - outils : `docs_for(path|symbol)`, que l'agent appelle avant de modifier un fichier, et `doc_drift`.
- **Risque** : homonymes et dépôts multi-langages. On commence avec les seuls éléments entre backticks.

### 7. Base reconstructible : annotations en JSONL (effort M)

- **Problème** : le blocage git décrit plus haut.
- **Comment** :
  - la source de vérité devient `files/` + `annotations/*.jsonl`, triés de façon déterministe : verdicts, notes, relations, retours de l'idée 1 ;
  - le `.lbdb` devient un cache ignoré par git, reconstruit avec des versions de modèles figées ;
  - les embeddings sont mis en cache par empreinte de passage.
- **Risque** : reconstruction lente sur CPU. Le cache par empreinte est indispensable (le réimport partiel existe déjà).

### 8. Revue de PR « knowledge diff » en CI (effort M)

- **Problème** : une PR de doc peut introduire des contradictions ou casser des références sans que personne le voie.
- **Comment** : `innerrag diff base..head` dans une GitHub Action, qui publie un commentaire de PR listant :
  - les nouvelles contradictions ;
  - les notes rendues obsolètes ;
  - les symboles disparus ;
  - la régression des métriques sur `eval.json`.

  S'appuie sur les idées 5, 6 et 7.
- **Risque** : temps de CI avec les modèles ONNX. La réindexation incrémentale est obligatoire.

### 9. Relations typées collectées par l'agent (effort M)

- **Problème** : la co-occurrence ne dit pas « remplace », « dépend de », « est responsable de ».
- **Comment** :
  - `assert_relation(src, rel, dst, evidence_chunk)` avec un vocabulaire fermé d'environ 6 relations ;
  - `explore_entity` propose de typer les paires les plus consultées, ce qui répartit le coût sur l'usage réel ;
  - la confiance augmente avec le nombre d'affirmations indépendantes.
- **Risque** : faible adoption, schéma qui dérive.

### 10. Expliquer un lien et cartographier les thèmes (effort M/L)

- **Problème** : un RAG vectoriel ne sait pas répondre à « quel rapport entre A et B ? » ni à « quels sont les grands sujets ? ».
- **Comment** :
  - `explain_link(A, B)` donne les plus courts chemins, avec des liens pondérés par PMI (co-occurrence corrigée de la fréquence) plutôt que par simple comptage. Chaque étape est justifiée par un passage ;
  - un regroupement en communautés (Leiden) fait ressortir les thèmes, avec des étiquettes extraites du texte. L'agent peut enregistrer un résumé de thème via l'idée 4.
- **Risque** : le bruit de la co-occurrence rend certains chemins absurdes. L'idée gagne en valeur une fois l'idée 9 en place.

### 11. Cache des recherches (effort S, proposé par l'utilisateur)

**Fait le 7 octobre 2026** : recherche répétée en moins d'1 ms au lieu de 482 ms ; changer k réutilise les scores du cross-encoder (210 ms au lieu de 482).

- **Problème** : une recherche prend environ 450 ms sur CPU, mesurés le 7 octobre 2026 sur le livre (2 448 passages, 8 cœurs). Le reclassement par cross-encoder en prend environ 360, l'extraction d'entités de la question environ 70, le reste environ 15. Les agents répètent souvent la même recherche : nouvel essai, mode map puis full, même question posée par plusieurs personnes.
- **Comment** :
  - cache des réponses par projet, avec pour clé la question normalisée et les paramètres, invalidé par un numéro de version du projet incrémenté à chaque ingestion ou suppression. Une recherche répétée répond en environ 1 ms ;
  - cache des scores du cross-encoder par couple (question, passage), pour que changer k, budget ou mode ne relance pas la partie coûteuse ;
  - cache des embeddings et des entités de la question (LRU).
- **Risque** : faible, grâce à l'invalidation par version. Il faut borner la mémoire (LRU) et indiquer dans la réponse et dans l'historique qu'elle vient du cache.

## Les trois à faire en premier, selon le sous-agent

1. **Boucle de citation (1)** : peu chère, elle fournit les données pour tout le reste. Le jeu de référence grossit seul, le seuil se recalibre sur l'usage réel et la page des lacunes sert tout de suite aux rédacteurs.
2. **Contexte à budget (2)** : c'est le gain le plus visible pour un utilisateur de Claude Code, et il se chiffre dans l'historique des appels.
3. **Pont doc ↔ code (6)** : ce qui distingue innerrag d'un RAG générique pour des équipes qui codent avec des agents.

La base reconstructible (7) apporte peu de valeur visible, mais elle doit être planifiée avant d'avoir plusieurs utilisateurs : sans elle, les idées 4, 5 et 8 ne fonctionnent pas en équipe.

## Avis de Claude (session de développement)

- Je suis d'accord avec ce trio.
- J'avancerais l'idée 7 si les projets doivent vraiment être partagés à plusieurs via git. Elle revient sur le choix initial de versionner la base telle quelle, donc c'est à décider explicitement.
- L'idée 2 se combine bien avec le module de chat relié à un Claude Code local, qui en serait la vitrine.
