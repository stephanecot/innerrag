# Relier les images au bon contenu

Réflexion du 7 octobre 2026.

## Le constat

Sur le projet `sample`, le panneau du nœud **GTA 6** (page d'accueil de jeuxvideo.com) affiche 23 images. Une seule parle de GTA 6 : la vignette de l'article « GTA 6 plus hardcore que GTA 5 ? ». Les autres viennent de Star Wars Galactic Racer, de la Paris Games Week, d'une publicité pour des écouteurs, d'un logo McDonald's…

La cause est le grain du lien. Aujourd'hui, une image est rattachée au **passage** qui la contient (`Chunk -[:SHOWS]-> Image`), et une entité à toutes les images des passages qui la citent :

```
Entity <-[:MENTIONS]- Chunk -[:SHOWS]-> Image
```

Un passage fait environ 1 000 caractères. Sur une page d'accueil, il regroupe 4 ou 5 cartes d'articles, chacune avec son image. GTA 6 est cité dans 3 passages, ce qui ramène toutes les images de ces 3 passages. Le même défaut existe, en moins visible, dans les livres : une page de PDF qui montre deux figures et parle de trois notions relie chaque figure aux trois notions.

Le lien passage → image reste juste : il sert au lecteur et aux résultats de recherche. C'est le lien **image → entité** qui manque. Il faut le calculer à l'ingestion et le stocker avec un score.

## Les signaux disponibles, du plus sûr au plus coûteux

### 1. La légende (effort S)

La légende vient de l'attribut alt, d'une figcaption, d'une ligne « Figure 3 : … » ou du titre de carte qui suit l'image. Quand elle nomme une entité, le lien est presque certain.

- Passer la légende dans GLiNER (elle est déjà assez courte) et chercher aussi, tels quels, les noms des entités déjà connues dans le passage.
- Sur jeuxvideo.com, 30 des 74 images ont maintenant une légende. « GTA 6 plus hardcore que GTA 5 ? » relie directement la vignette à GTA 6 et à GTA 5.

### 2. La proximité dans le texte (effort S à M)

Dans le passage, l'image a une position : le marqueur `imgN` est placé à l'endroit exact où elle se trouvait. Les mentions d'entités ont aussi une position : la forme `surface` est stockée, et GLiNER donne les positions de début et de fin. Le score décroît avec la distance en caractères entre l'image et la mention. Le score est nul quand un titre, un séparateur de carte ou une autre image s'intercale.

- Les mentions juste avant (« voir la figure ci-dessous ») et juste après (titre de carte, explication) comptent toutes les deux. Celles d'après pèsent un peu plus pour le web, celles d'avant pour les livres.
- Ce signal suffit pour la plupart des PDF techniques : la figure est entourée du texte qui la commente.

### 3. La structure du document (effort M)

L'extraction connaît le bloc qui contient l'image, mais elle le perd aujourd'hui :

- **Web** : l'image est dans un `<a>`, un `<article>`, un `<li>` ou un `<figure>` avec son titre. Le texte de ce bloc est le contexte de l'image ; ce qui est hors du bloc ne la concerne pas.
- **PDF** : `pdftohtml -xml` donne les coordonnées. Le texte au-dessus et en dessous de l'image, sur la même page, avec un recouvrement horizontal, est son contexte.
- **Word** : le paragraphe d'ancrage de l'image et le paragraphe suivant.
- **PowerPoint** : la diapositive entière. C'est déjà le bon grain : un titre et une image.

L'extraction produirait pour chaque image un « contexte » de quelques lignes (`ExtractedImage.context`). Les signaux 1 et 2 s'appliqueraient sur ce contexte plutôt que sur le passage.

### 4. Ce que montre l'image (effort L, optionnel)

- **OCR** (Tesseract, local) sur les schémas et captures d'écran. Le texte lu dans un diagramme d'architecture (« API Gateway », « Order Service ») donne des liens très précis, utiles pour des spécifications. Coût : une dépendance de plus dans l'image Docker et environ 1 s par image sur CPU.
- **Embeddings image-texte** (un modèle CLIP multilingue en ONNX, 300 à 600 Mo) : comparer l'image aux noms des entités du passage. Utile pour les photos sans légende. La précision est faible sur les noms propres (un jeu vidéo, un produit), et l'image Docker serait bien plus lourde. À garder pour plus tard.

## Écarter les images qui ne montrent rien

Le lien ne vaut que si l'image a un sens. Parmi les images de jeuxvideo.com, une partie est de la décoration : publicités, logos de partenaires, pictos « News jeu ». Règles simples à ajouter à `is_furniture` et `save_images` :

- classes et identifiants publicitaires (`pub`, `ad-`, `ads`, `sponsor`, `partner`, `promo`, `outbrain`, `taboola`) ;
- domaines de régies connus ;
- bannières très allongées (rapport largeur/hauteur supérieur à 5) ;
- images dont la légende est seulement une rubrique (« News jeu », « Vidéo », « Test »), qui ne servent alors pas de légende.

## Modèle de données proposé

```
CREATE REL TABLE DEPICTS(FROM Image TO Entity, score DOUBLE, how STRING)
-- how : caption | near | block | ocr | vision
```

- Calculé dans la même transaction que `MENTIONS`, à partir des mentions du passage de l'image et de son contexte.
- On garde au plus 5 entités par image, avec un score d'au moins 0,4.
- Une image sans aucun lien reste rattachée à son passage. Elle reste visible dans le lecteur et la recherche, mais n'apparaît plus sur les nœuds.
- Les documents existants n'ont pas ce lien. Une ré-ingestion le crée. En attendant, le panneau garde le comportement actuel pour les documents sans `DEPICTS`.

## Effets dans l'interface et pour l'agent

- **Carte, panneau d'un nœud** : seulement les images liées à l'entité, triées par score, avec une indication du lien en survol (« légende », « texte voisin »). Les autres images des passages restent derrière un lien « autres images des passages (n) », replié.
- **Recherche** : une image ne remonte avec un passage que si elle est liée à une entité de la question, ou si le passage est petit (une diapositive, une figure isolée).
- **Assistant et MCP** : `search_knowledge` et `read_passages` ne joignent que les images liées au sujet. Aujourd'hui, l'agent reçoit des vignettes hors sujet et en affiche parfois une mauvaise.
- **Nœud image sur la carte** (plus tard) : une image liée à deux entités renforce leur relation, comme une co-occurrence dans une phrase.

## Mesurer avant et après

Constituer un petit jeu de vérité de 30 à 50 images, chacune avec les entités attendues :

- la page jeuxvideo.com ;
- quelques figures de livres techniques en PDF ;
- une page Wikipédia.

Mesurer la précision (images affichées sur un nœud qui le concernent vraiment) et le rappel (images attendues qui apparaissent). La mesure s'ajouterait à la page Évaluation, à côté de celle des recherches. Aujourd'hui, la précision sur le nœud GTA 6 est de 1 sur 23.

## Ordre proposé

1. Légende avec correspondance de noms, et filtre des images décoratives. C'est le gain le plus visible sur le web.
2. Proximité dans le passage, et table `DEPICTS` avec score. Le panneau du nœud filtre sur ce lien.
3. Contexte structurel à l'extraction : carte web, coordonnées PDF, ancrage Word.
4. Jeu de vérité et mesure dans Évaluation.
5. Plus tard, et seulement si les mesures le justifient : OCR, puis embeddings image-texte.
