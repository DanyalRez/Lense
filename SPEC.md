# LENSE — spec v1

Lanceur clavier façon Spotlight pour Windows 11, de la famille DESKTOUCH / Deskorb.
Process résident dans la zone de notification ; un raccourci global ouvre une fenêtre Liquid Glass en haut de l'écran.

## Objectifs

| # | Objectif | Critère mesurable |
|---|---|---|
| 1 | Ouvrir instantanément | hotkey → fenêtre visible et focus < 50 ms |
| 2 | Taper sans latence | résultats redessinés < 16 ms par frappe (index de 100 k fichiers) |
| 3 | Être invisible au repos | RAM < 30 Mo (working set), CPU ~0 % fenêtre cachée |
| 4 | Trouver la bonne chose en 2–3 lettres | la frecency remonte ce qui est ouvert souvent/récemment |
| 5 | Ne rien fuiter | aucune donnée perso (chemins, historique) dans le repo |

Hors V1 : commandes système, presse-papier, plugins, onglets navigateur, Everything, devises (réseau), interface de réglages en verre.

## Stack

Identique à Deskorb : **Rust + `windows-sys` 0.59, aucune autre dépendance.**
`render.rs` copié de Deskorb tel quel ; `win.rs` et `settings.rs` repris (texte GDI → calque alpha, capture + frost du fond, menu, fichier clé=valeur, démarrage avec Windows).
Pas de COM « confort » : les quelques interfaces shell nécessaires (IShellFolder pour `shell:AppsFolder`) sont appelées par leur vtable, ~60 lignes.

## Modules

```
src/
  main.rs      fenêtre layered, boucle de messages, tray, hotkey, clavier, dessin de la liste
  render.rs    moteur Liquid Glass (copie Deskorb)
  win.rs       texte, capture du fond, menu, icônes HICON → calque (repris de Deskorb)
  settings.rs  settings.txt (clé=valeur), démarrage avec Windows
  apps.rs      index des apps (menu Démarrer, AppsFolder/UWP, .exe épinglés), lancement, icônes
  files.rs     index de fichiers : scan, surveillance, persistance
  fuzzy.rs     correspondance + score + tolérance aux fautes
  history.rs   frecency (compteur + date du dernier lancement)
  calc.rs      expressions et conversions d'unités
  search.rs    fusion des sources, classement, ligne « recherche web »
```

Flux : `WM_HOTKEY` → capture du fond sous la fenêtre + frost → affichage.
`WM_CHAR` → `search::query(&str)` → `Vec<Hit>` (≤ 8) → redessin des calques texte/sélection seulement.
Un fil de fond par racine de fichiers (scan puis `ReadDirectoryChangesW`) ; l'index est partagé via `Arc<RwLock>`, le fil UI ne bloque jamais plus d'une lecture.

### Rendu (clé de la perf)

- Fenêtre créée une fois au démarrage, cachée (`ShowWindow SW_HIDE`), jamais détruite.
- Le verre (fond capturé, frost, réfraction, rim) est calculé **à l'ouverture et quand le nombre de lignes change**, puis mis en cache.
- À chaque frappe ou flèche : seuls le texte, les icônes et la pastille de sélection sont recomposés sur le cache (blend alpha), puis `UpdateLayeredWindow`.
- Si le coût du verre dépasse le budget à l'ouverture : fenêtre de hauteur fixe (8 lignes) — décidé à la tranche 1 sur mesure.

### Géométrie et DA

- 680 × (56 + n × 48) px logiques (DPI-aware), n ≤ 8 ; centrée horizontalement, haut à 22 % de la hauteur de l'écran courant (celui de la souris).
- Glass sombre, coins 20 px, rim gradient, texte blanc ; pastille de sélection en verre plus clair ; sous-titre gris (chemin, « Application », « Calcul »…) ; icône 32 px à gauche.
- Pied discret sur la ligne sélectionnée : `↵ Ouvrir · Ctrl ↵ Dossier · Maj ↵ Admin`.

## Sources de résultats

| Source | Contenu | Rafraîchissement |
|---|---|---|
| Apps | `.lnk` de `%APPDATA%\…\Start Menu` + `%PROGRAMDATA%\…\Start Menu` ; UWP via `shell:AppsFolder` (éléments dont le nom d'analyse est un AUMID `xxx!App`) ; `.lnk` épinglés à la barre des tâches + `pinned=` des réglages | au démarrage et « Réindexer » (< 300 ms, pas persisté) |
| Fichiers | racines de `roots=` (défaut : Bureau, Documents, Téléchargements ; ton dossier CODE s'ajoute dans ton `settings.txt` local, jamais dans le repo) ; dossiers ignorés `ignore=` (défaut : `.git node_modules target .venv __pycache__`) | index persistant + surveillance |
| Calcul | `12*4+3`, `2^10`, `(3,5+1)/2`, `sqrt 2`, `pi` ; `%` modulo | à la frappe |
| Unités | `10 km en mi`, `10km to mi`, `72 f` ; longueur, masse, température, données | à la frappe |
| Web | dernière ligne « Rechercher « x » sur Google » ; seule ligne si rien d'autre | toujours |

Dédoublonnage apps : même nom affiché → on garde le `.lnk` (il a un chemin, donc Ctrl/Maj+Entrée marchent).
Icônes : `SHGetFileInfoW` (PIDL, grande icône), chargées paresseusement pour les lignes visibles, cache mémoire LRU de 256.
Données : Ko/Mo/Go = 1024 (comme l'Explorateur), KB/MB/GB = 1000.

## Recherche et classement

1. **Préfiltre** : masque 64 bits des caractères présents dans le nom (a–z, 0–9, autres) ; une entrée sans tous les caractères de la requête est écartée sans calcul. Garde les 100 k fichiers dans le budget.
2. **Correspondance** (sur le nom, insensible à la casse et aux accents) : sous-séquence avec score — préfixe > début de mot (espace, `-`, `_`, `.`, CamelCase) > lettres consécutives > lettres éparses ; pénalité de longueur.
3. **Fautes de frappe** (apps seulement, ~quelques centaines d'entrées) : si pas de sous-séquence, Damerau-Levenshtein entre la requête et le début de chaque mot ; 1 faute tolérée dès 4 lettres, 2 dès 8.
4. **Score final** = `match + 1,5 × frecency` où `frecency = ln(1 + lancements) × 0,5^(jours depuis dernier / 14)` (addition : la note fzy peut être négative). Historique réécrit à chaque lancement.
5. Ordre des groupes : calcul/unités (si la requête en est un) en tête, puis apps et fichiers mélangés au score (bonus apps +1 ; fichiers cherchés dès 2 lettres), web en dernier.

## Format des fichiers (`%LOCALAPPDATA%\Lense\`)

**`settings.txt`** (clé=valeur, comme Deskorb)
```
hotkey=Alt+Space
engine=https://www.google.com/search?q=%s
roots=Desktop;Documents;Downloads   # dossiers de Windows (même dans OneDrive) ou chemins
ignore=.git;node_modules;target;.venv;__pycache__
pinned=
autostart=0          # activé depuis le menu de l'icône
```

**`history.txt`** (une ligne par élément lancé, réécrit au plus toutes les 30 s)
```
<clé>\t<lancements>\t<dernier lancement, unix>
```
Clé = chemin du `.lnk`/fichier ou `shell:AppsFolder\<AUMID>`.

**`files.idx`** (binaire, petit-boutiste) — arbre plutôt que chemins complets, pour la RAM
```
en-tête : "LENSE" | version u16 | nb racines u16 | nb entrées u32 | taille noms u32
racines : (len u16, UTF-16) × nb racines
entrées : parent u32 (u32::MAX = racine i encodée à part) | off nom u32 | len nom u16 | flags u8 (1 = dossier) | racine u8
noms    : blob UTF-8 contigu
```
Le masque de préfiltre et le nom en minuscules sans accents sont recalculés au chargement (pas stockés).
~25 octets/entrée + nom → 100 k fichiers ≈ 5 Mo de RAM.

**Mise à jour incrémentale** : au démarrage, on charge `files.idx` (résultats immédiats), puis un fil en priorité de fond (`THREAD_MODE_BACKGROUND_BEGIN`) rescanne pour rattraper ce qui a changé pendant que Lense était fermé. Ensuite `ReadDirectoryChangesW` récursif par racine applique ajouts/suppressions/renommages ; débordement du tampon → rescan de la racine. Sauvegarde différée (60 s après le dernier changement) et à la fermeture. Les entrées supprimées sont marquées puis compactées à la sauvegarde.

## Raccourcis clavier

| Touche | Action |
|---|---|
| **Alt+Espace** (configurable) | ouvrir / fermer |
| Échap · clic dehors (perte d'activation) | fermer (la requête est vidée) |
| ↑ / ↓ · Ctrl+K / Ctrl+J | ligne précédente / suivante |
| Entrée | ouvrir (calcul : copier le résultat ; web : ouvrir le moteur) |
| Ctrl+Entrée | ouvrir le dossier parent, élément sélectionné (`explorer /select,`) |
| Maj+Entrée | lancer en admin (`ShellExecuteW "runas"`) ; sans effet sur UWP et web |
| Ctrl+C (sans sélection de texte) | copier le chemin / le résultat |
| Ctrl+Retour arrière | effacer le mot |
| Ctrl+V, Ctrl+A, ←/→, Début/Fin | édition du champ |

Le champ de saisie est maison (WM_CHAR + caret dessiné) ; pas d'IME en V1.

### Conflit Alt+Espace

- `RegisterHotKey(MOD_ALT | MOD_NOREPEAT, VK_SPACE)` : un hotkey enregistré passe **avant** le menu système des fenêtres, donc le menu système disparaît tant que Lense tourne (documenté dans le README ; il reste accessible par clic droit sur la barre de titre).
- Si l'enregistrement échoue (`ERROR_HOTKEY_ALREADY_REGISTERED`, typiquement PowerToys Run) : Lense prend **Ctrl+Alt+Espace** à la place et affiche une bulle tray : « Alt+Espace est déjà pris (PowerToys Run ?). Lense utilise Ctrl+Alt+Espace — changez-le dans les paramètres. »
- Pas de hook clavier bas niveau (coût CPU, antivirus, fragile).

## Tray

Icône = logo. Clic gauche : ouvre Lense. Menu : **Paramètres** (ouvre `settings.txt` dans l'éditeur par défaut, rechargé à la fermeture de la fenêtre suivante), **Réindexer**, **Démarrer avec Windows** (coché), **Quitter**.
Instance unique (mutex nommé) : relancer l'exe ouvre la fenêtre existante.

## Logo

Tuile DESKTOUCH : carré arrondi en verre sombre, rim gradient, glyphe blanc = une lentille (cercle épais avec un reflet en croissant, sans manche, pour se distinguer de la loupe générique). Livrables comme Deskorb : SVG maître, version petites tailles, `lens.ico` (16 → 256), embarqué par `build.rs`.

## Vérifications

- `cargo run -- --bench` : temps d'ouverture, temps de requête p50/p99 sur l'index réel, taille de l'index — sortie console uniquement, jamais commitée.
- Tests unitaires : `fuzzy` (classement attendu sur une liste fixe), `calc` (expressions + unités), lecture/écriture de `files.idx` (aller-retour), `history` (décroissance).
- Les captures du README sont faites sur un compte/dossier de démo (pas de chemins perso).
