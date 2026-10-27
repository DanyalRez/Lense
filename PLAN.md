# LENSE — plan v1

Tranches testables, dans l'ordre. Chaque tranche se termine par un **« go »** de ta part avant tout lancement sur ton PC, puis ta validation à l'usage.
Taille : S < 150 lignes, M 150–400, L > 400.

## T0 — Squelette (S)
- `cargo new`, `Cargo.toml` (features windows-sys de Deskorb + `Win32_System_Com`, `Win32_UI_Shell_Common`), profil release identique.
- Copier `render.rs`, reprendre `win.rs` / `settings.rs` de Deskorb (élaguer ce qui concerne souris/tokens).
- `.gitignore` : `target/`, `*.idx`, `history.txt`, `bench*.txt`.
- **Fini quand** : `cargo build --release` passe, aucun warning.

## T1 — Fenêtre + hotkey + tray (L)
- Fenêtre layered sans bordure, créée cachée ; verre rendu sur fond capturé ; champ de saisie maison (caret, Ctrl+A/V/Retour arrière).
- `RegisterHotKey` Alt+Espace, repli Ctrl+Alt+Espace + bulle si déjà pris ; Échap / perte d'activation → cacher.
- Tray : Paramètres, Réindexer (inactif), Démarrer avec Windows, Quitter ; mutex d'instance unique.
- Liste factice de 8 lignes pour régler la DA et le cache de verre.
- `--bench` : temps hotkey → affichage, temps de recomposition par frappe.
- **Fini quand** : ouverture < 50 ms, frappe < 16 ms, RAM < 30 Mo et CPU 0 % caché (Gestionnaire des tâches). Décision fenêtre hauteur variable / fixe.

## T2 — Apps (M)
- `apps.rs` : `.lnk` du menu Démarrer (utilisateur + commun), `shell:AppsFolder` (UWP), `.lnk` épinglés barre des tâches + `pinned=`.
- Icônes paresseuses (`SHGetFileInfoW`), cache LRU 256.
- Recherche provisoire : « contient » insensible à la casse. Entrée / Ctrl+Entrée / Maj+Entrée.
- **Fini quand** : toutes tes apps (y compris Store) apparaissent avec leur icône, se lancent, admin fonctionne sur une app classique.

## T3 — Fuzzy + frecency (M)
- `fuzzy.rs` : préfiltre masque, sous-séquence scorée, Damerau-Levenshtein pour les apps.
- `history.rs` : `history.txt`, score de frecency, écriture différée.
- Tests : liste fixe → classement attendu (« vsc » → Visual Studio Code, « chrme » → Chrome, « ps » → préfère l'app la plus lancée).
- **Fini quand** : tests verts, et à l'usage les apps fréquentes remontent en 1–2 lettres.

## T4 — Fichiers (L)
- `files.rs` : scan récursif (`FindFirstFileExW`, `FIND_FIRST_EX_LARGE_FETCH`) avec `ignore=` ; format `files.idx` + test aller-retour.
- Fil de fond : chargement immédiat, rescan en priorité basse, `ReadDirectoryChangesW` par racine, sauvegarde différée.
- Fusion dans `search.rs` avec les apps.
- `--bench` : taille de l'index, requête p50/p99.
- **Fini quand** : requête < 16 ms p99 sur ton index réel, un fichier créé/renommé apparaît en < 2 s, RAM < 30 Mo.

## T5 — Calcul + unités (M)
- `calc.rs` : parseur descendant récursif (+ − × ÷ ^ % parenthèses, virgule décimale, sqrt, pi) ; unités longueur/masse/température/données, « en » / « to ».
- Entrée copie le résultat (`SetClipboardData`).
- Tests : ~20 cas (priorités, division par zéro → pas de ligne, `10 km en mi`, `-40 c en f`, `1 Go en Mo`).
- **Fini quand** : tests verts.

## T6 — Web + finitions (S)
- Ligne de repli « Rechercher « x » sur … » (moteur `engine=` avec `%s`, encodage URL).
- Pied d'aide sur la ligne sélectionnée, rechargement de `settings.txt`.
- **Fini quand** : rien ne correspond → Entrée ouvre la recherche dans le navigateur par défaut.

## T7 — Logo + README (S)
- Logo lentille (skill logo-design, DA tuile DESKTOUCH) : SVG, petites tailles, `lens.ico`, `build.rs`.
- README : install, raccourcis, réglages, note sur Alt+Espace / PowerToys ; captures sur données de démo uniquement.
- **Fini quand** : tu valides le logo ; relecture « aucune donnée perso » avant que tu crées le repo `DanyalRez/Lense`.

## Risques connus
| Risque | Parade |
|---|---|
| Rendu du verre trop lent pour 50 ms à l'ouverture | mesuré dès T1 ; repli : hauteur fixe + verre pré-calculé, re-capture seulement du fond |
| Vol de focus refusé par Windows | le process qui reçoit `WM_HOTKEY` a le droit à `SetForegroundWindow` ; à vérifier en T1 |
| Dossier CODE énorme (`target/`, `node_modules`) | `ignore=` par défaut + mesure en T4 |
| Menu système Alt+Espace perdu | assumé, documenté ; hotkey configurable |
