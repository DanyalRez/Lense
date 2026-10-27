//! LENSE : lanceur clavier façon Spotlight. Process résident dans la zone de notification ; un raccourci global
//! ouvre une fenêtre Liquid Glass en haut de l'écran (champ de recherche, résultats dessous).
//! Le verre est calculé à l'ouverture (et quand le nombre de lignes change) puis gardé : à chaque frappe, seuls
//! le texte, le curseur et la sélection sont recomposés dessus.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apps;
mod calc;
mod files;
mod fuzzy;
mod history;
mod paint;
mod prefs;
#[allow(dead_code, clippy::approx_constant)] // copié de Deskorb tel quel, tout n'est pas utilisé
mod render;
mod settings;
mod win;

use paint::{blit, fill};
use render::{Back, Frame, Look, Pill, Rgb, Rr};
use settings::Config;
use std::{
    cell::RefCell,
    collections::HashMap,
    mem::zeroed,
    ptr::{null, null_mut},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{
        Com::{CoInitializeEx, COINIT_APARTMENTTHREADED},
        LibraryLoader::GetModuleHandleW,
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::{CreateMutexW, GetCurrentProcess, GetCurrentThread, SetProcessWorkingSetSize, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN},
    },
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

const WM_TRAY: u32 = WM_APP + 1;
const WM_SHOW: u32 = WM_APP + 2; // une seconde instance a été lancée : on s'ouvre
const WM_INDEXED: u32 = WM_APP + 3; // index des apps prêt (fil de fond → fil de l'interface)
const WM_FILES: u32 = WM_APP + 4; // index de fichiers modifié : à enregistrer (avec un délai)
const WM_ICONS: u32 = WM_APP + 5; // des icônes de fichiers manquent : chargées après le dessin
const HOTKEY_ID: i32 = 1;
const T_SAVE: usize = 1;
const SAVE_AFTER_MS: u32 = 60_000; // après le dernier changement : une rafale ne réécrit files.idx qu'une fois
const APP_BONUS: f32 = 1.0; // une app passe devant un fichier aussi bien noté
const FILE_MIN_QUERY: usize = 2; // une seule lettre ne cherche que les apps
const ID_SETTINGS: usize = 1;
const ID_STARTUP: usize = 2;
const ID_QUIT: usize = 3;
const ID_REINDEX: usize = 4;
const ICON: f32 = 32.0;
const FRECENCY: f32 = 1.5; // poids de la frecency face à la note de correspondance
// le fond est capturé et flouté SHRINK fois plus petit (il sera flou de toute façon), puis lu agrandi
const SHRINK: usize = 3;

/// Rayon du flou, en pixels du fond réduit : ~16 px logiques.
fn blur(s: f32) -> usize {
    (16.0 * s / SHRINK as f32).round().max(1.0) as usize
}
// pixels logiques (96 dpi) ; la marge autour de la forme laisse la place à l'ombre
const M: f32 = 30.0;
const W: f32 = 680.0;
const FIELD: f32 = 56.0;
const ROW: f32 = 48.0;
const RADIUS: f32 = 20.0;
const TOP: f32 = 0.22; // haut de la forme, en fraction de la hauteur de l'écran
const TEXT_X: f32 = 52.0;
const QUERY_SIZE: f32 = 21.0;
const ACCENT: Rgb = [0.38, 0.62, 1.0];

/// Texte anglais ou français.
pub type Tr = (&'static str, &'static str);

pub fn tr(fr: bool, t: Tr) -> &'static str {
    if fr { t.1 } else { t.0 }
}

/// Apparence : 0 verre (texte blanc lisible sur tout fond), 1 sombre opaque, 2 clair.
pub fn look(theme: usize) -> Look {
    match theme {
        1 => Look { refraction: 0.0, tint: [0.09, 0.09, 0.11], tint_amount: 0.9, ink: [1.0; 3] },
        2 => Look { refraction: 1.0, tint: [0.97, 0.97, 0.98], tint_amount: 0.74, ink: [0.08, 0.08, 0.1] },
        _ => Look { refraction: 1.0, tint: [0.05, 0.05, 0.07], tint_amount: 0.55, ink: [1.0; 3] },
    }
}

static INDEX: Mutex<Option<Vec<apps::Entry>>> = Mutex::new(None);

/// Indexe les apps dans un fil à part (~1 s avec les icônes), puis prévient la fenêtre.
fn start_index(hwnd: HWND, pinned: Vec<String>) {
    let (h, n) = (hwnd as isize, (ICON * unsafe { GetDpiForSystem() } as f32 / 96.0).round() as usize);
    std::thread::spawn(move || unsafe {
        CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        let list = apps::index(&pinned, n);
        *INDEX.lock().unwrap() = Some(list);
        PostMessageW(h as HWND, WM_INDEXED, 0, 0);
    });
}

/// Index de fichiers : chargé depuis files.idx (résultats tout de suite), rescanné en priorité basse pour
/// rattraper ce qui a changé Lense fermé, puis surveillé racine par racine.
// shortcut: les changements entre le scan d'un dossier et le début de sa surveillance sont perdus jusqu'au
// prochain rescan (Réindexer) ; surveiller avant de scanner si ça se voit
fn start_files(hwnd: HWND, shared: files::Shared, roots: Vec<String>, ignore: Vec<String>) {
    let h = hwnd as isize;
    let dirty = move || unsafe { PostMessageW(h as HWND, WM_FILES, 0, 0); };
    std::thread::spawn(move || unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
        if let Some(i) = files::load(&roots) {
            *shared.write().unwrap() = i;
        }
        let rescan = {
            let (shared, roots, ignore) = (shared.clone(), roots.clone(), ignore.clone());
            move || {
                let fresh = files::Index::scan(roots.clone(), &ignore);
                let mut cur = shared.write().unwrap();
                if cur.roots == roots || cur.roots.is_empty() {
                    *cur = fresh; // sinon les réglages ont changé entre-temps : un autre scan est en route
                }
                drop(cur);
                dirty();
            }
        };
        rescan();
        for r in 0..roots.len() {
            let (shared, ignore, rescan) = (shared.clone(), ignore.clone(), rescan.clone());
            std::thread::spawn(move || files::watch(shared, r as u8, ignore, dirty, rescan));
        }
    });
}

/// Un résultat affiché.
struct Hit {
    app: Option<usize>, // indice dans `apps`, sinon un fichier
    title: String,
    sub: String,
    path: String,
    dir: bool,
    copy: Option<String>, // calcul ou conversion : Entrée copie ce texte
    web: bool,            // recherche web : `path` est l'adresse
    glyph: &'static str,  // pictogramme (police d'icônes de Windows) quand il n'y a pas d'icône
}

impl Hit {
    fn is_file(&self) -> bool {
        self.app.is_none() && self.copy.is_none() && !self.web
    }
}

/// Dossier d'un chemin, le profil abrégé en `~` (sous-titre d'un fichier).
fn short_dir(path: &str) -> String {
    let dir = path.rsplit_once('\\').map_or(path, |d| d.0);
    match std::env::var("USERPROFILE") {
        Ok(home) if dir.len() >= home.len() && dir[..home.len()].eq_ignore_ascii_case(&home) => format!("~{}", &dir[home.len()..]),
        _ => dir.to_string(),
    }
}

/// Clé du cache d'icônes d'un fichier : l'extension (icône du type), ou le chemin pour ce qui a sa propre icône.
fn icon_key(path: &str, dir: bool) -> String {
    if dir {
        return "<dossier>".into();
    }
    let ext = path.rsplit_once('.').map_or("", |e| e.1).to_ascii_lowercase();
    if matches!(ext.as_str(), "exe" | "lnk" | "ico" | "url") { path.to_string() } else { ext }
}

/// Rend à Windows les pages de mémoire inutilisées (images libérées à la fermeture de la fenêtre).
fn trim() {
    unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };
}

fn shape_h(rows: usize) -> f32 {
    if rows == 0 { FIELD } else { FIELD + rows as f32 * ROW + 8.0 }
}

/// Écran de la souris : zone de travail et échelle.
unsafe fn monitor() -> (RECT, f32) {
    let mut p: POINT = zeroed();
    GetCursorPos(&mut p);
    let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
    let mut mi: MONITORINFO = zeroed();
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(mon, &mut mi);
    let (mut dx, mut dy) = (96, 96);
    GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    (mi.rcWork, dx as f32 / 96.0)
}

struct App {
    hwnd: HWND,
    cfg: Config,
    hotkey: String, // raccourci réellement enregistré (vide si aucun)
    icon: HICON,
    taskbar_created: u32,
    shown: bool,
    hidden_at: Instant,
    // fenêtre : position, échelle, bitmap ; libérées à la fermeture
    win: RECT,
    s: f32,
    dc: HDC,
    dib: HBITMAP,
    bits: *mut u32,
    back: Back,
    base: Vec<u32>,   // verre seul, sans texte
    base_rows: usize, // nombre de lignes pour lequel `base` a été calculé
    // saisie
    query: Vec<char>,
    caret: usize,
    all: bool,         // tout le texte sélectionné (Ctrl+A) : la frappe suivante le remplace
    high: Option<u16>, // première moitié d'une paire UTF-16 (emoji) reçue par WM_CHAR
    apps: Vec<apps::Entry>,
    indexing: bool,
    history: history::History,
    files: files::Shared,
    files_dirty: bool,
    icons: HashMap<String, Vec<u32>>, // icônes des fichiers, `icon_n` px
    icon_n: usize,
    hits: Vec<Hit>,
    sel: usize,
    prefs: prefs::Panel,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// `None` aussi quand l'état est déjà emprunté : ShowWindow, SetForegroundWindow ou un menu renvoient des
/// messages (WM_ACTIVATE…) pendant qu'on le modifie ; on les ignore au lieu de paniquer.
fn app<T>(f: impl FnOnce(&mut App) -> T) -> Option<T> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

impl App {
    unsafe fn new(hwnd: HWND, prefs_hwnd: HWND) -> Self {
        App {
            hwnd, cfg: settings::load(), hotkey: String::new(), icon: null_mut(),
            taskbar_created: RegisterWindowMessageW(win::w("TaskbarCreated").as_ptr()),
            shown: false, hidden_at: Instant::now(),
            win: zeroed(), s: 1.0, dc: CreateCompatibleDC(null_mut()), dib: null_mut(), bits: null_mut(),
            back: Back::empty(), base: Vec::new(), base_rows: usize::MAX,
            query: Vec::new(), caret: 0, all: false, high: None, apps: Vec::new(), indexing: false, history: history::History::load(),
            files: Arc::new(RwLock::new(files::Index::default())), files_dirty: false, icons: HashMap::new(),
            icon_n: (ICON * GetDpiForSystem() as f32 / 96.0).round() as usize, hits: Vec::new(), sel: 0,
            prefs: prefs::Panel::new(prefs_hwnd),
        }
    }

    fn fr(&self) -> bool {
        self.cfg.french
    }

    /// (Re)lance l'index de fichiers pour les racines des réglages ; l'ancien est abandonné.
    // shortcut: les fils de surveillance de l'ancien index restent bloqués (un par racine, 0 % CPU) jusqu'à
    // la fermeture de Lense ; les arrêter (CancelIoEx) si on change souvent de racines
    fn index_files(&mut self) {
        let roots = files::resolve_roots(&self.cfg.roots);
        self.files = Arc::new(RwLock::new(files::Index::default()));
        start_files(self.hwnd, self.files.clone(), roots, self.cfg.ignore.clone());
    }

    /// Enregistre files.idx dans un fil (quelques Mo à écrire).
    fn save_files(&mut self, wait: bool) {
        if !std::mem::take(&mut self.files_dirty) {
            return;
        }
        let shared = self.files.clone();
        let t = std::thread::spawn(move || {
            if let Ok(i) = shared.read() {
                if !i.roots.is_empty() {
                    files::save(&i);
                }
            }
        });
        if wait {
            let _ = t.join();
        }
    }

    fn reindex(&mut self) {
        if !self.indexing {
            self.indexing = true;
            start_index(self.hwnd, self.cfg.pinned.clone());
        }
    }

    unsafe fn indexed(&mut self) {
        let Some(list) = INDEX.lock().unwrap().take() else { return };
        (self.apps, self.indexing) = (list, false);
        if self.shown {
            self.search();
            self.draw();
        } else {
            trim(); // l'extraction des icônes a chargé une bonne partie du shell : ~35 Mo rendus
        }
    }

    fn size(&self) -> (usize, usize) {
        ((self.win.right - self.win.left) as usize, (self.win.bottom - self.win.top) as usize)
    }

    fn shape(&self) -> Rr {
        let s = self.s;
        Rr { x: M * s, y: M * s, w: W * s, h: shape_h(self.hits.len()) * s, r: RADIUS * s }
    }

    // ---------- raccourci, réglages ----------

    unsafe fn tray_tip(&self) {
        win::tray(self.hwnd, WM_TRAY, NIM_MODIFY, self.icon, &format!("Lense — {}", self.hotkey));
    }

    /// Enregistre le raccourci des réglages ; s'il est pris (PowerToys Run…), Ctrl+Alt+Espace et une bulle.
    unsafe fn register(&mut self) {
        UnregisterHotKey(self.hwnd, HOTKEY_ID);
        let want = self.cfg.hotkey.clone();
        self.hotkey.clear();
        for hk in [want.as_str(), settings::FALLBACK_HOTKEY] {
            let Some((mods, vk)) = settings::hotkey(hk) else { continue };
            if RegisterHotKey(self.hwnd, HOTKEY_ID, mods | MOD_NOREPEAT, vk) != 0 {
                self.hotkey = hk.into();
                break;
            }
        }
        self.tray_tip();
        if self.hotkey == want {
            return;
        }
        let fr = self.fr();
        let taken = match (want == settings::DEFAULT_HOTKEY, fr) {
            (true, false) => "Alt+Space is already taken (PowerToys Run?)".into(),
            (true, true) => "Alt+Espace est déjà pris (PowerToys Run ?)".into(),
            (false, false) => format!("{want} is already taken"),
            (false, true) => format!("{want} est déjà pris"),
        };
        let text = match (self.hotkey.is_empty(), fr) {
            (true, false) => format!("{taken}, and so is {}. Pick a shortcut in Settings.", settings::FALLBACK_HOTKEY),
            (true, true) => format!("{taken}, et {} aussi. Choisissez un raccourci dans les paramètres.", settings::FALLBACK_HOTKEY),
            (false, false) => format!("{taken}. Lense uses {} — change it in Settings.", self.hotkey),
            (false, true) => format!("{taken}. Lense utilise {} — changez-le dans les paramètres.", self.hotkey),
        };
        win::notify(self.hwnd, "Lense", &text);
    }

    /// Essaie un nouveau raccourci (panneau Paramètres) : gardé et enregistré s'il est libre, sinon l'ancien revient.
    unsafe fn try_hotkey(&mut self, hk: &str) -> bool {
        UnregisterHotKey(self.hwnd, HOTKEY_ID);
        let free = settings::hotkey(hk).is_some_and(|(mods, vk)| RegisterHotKey(self.hwnd, HOTKEY_ID, mods | MOD_NOREPEAT, vk) != 0);
        if !free {
            self.register();
            return false;
        }
        (self.hotkey, self.cfg.hotkey) = (hk.into(), hk.into());
        settings::set("hotkey", hk);
        self.tray_tip();
        true
    }

    /// Relit settings.txt (le panneau l'écrit, on peut aussi l'éditer à la main) à chaque ouverture.
    unsafe fn reload(&mut self) {
        let cfg = settings::load();
        if cfg.hotkey != self.cfg.hotkey {
            self.cfg.hotkey = cfg.hotkey.clone();
            self.register();
        }
        if cfg.autostart != self.cfg.autostart {
            settings::set_startup(cfg.autostart);
        }
        let refiles = (&cfg.roots, &cfg.ignore) != (&self.cfg.roots, &self.cfg.ignore);
        self.cfg = cfg;
        if refiles {
            self.index_files();
        }
    }

    // ---------- ouverture, fermeture ----------

    unsafe fn toggle(&mut self) {
        if self.shown { self.hide() } else { self.open() }
    }

    /// En haut (ou au milieu) de l'écran de la souris ; la bitmap a la hauteur maximale, la forme grandit dedans.
    unsafe fn place(&mut self) {
        let (r, s) = monitor();
        self.s = s;
        let (ww, wh) = (((W + 2.0 * M) * s) as i32, ((shape_h(self.cfg.rows) + 2.0 * M) * s) as i32);
        let left = r.left + (r.right - r.left - ww) / 2;
        let top = if self.cfg.center {
            r.top + (r.bottom - r.top - wh) / 2
        } else {
            r.top + ((r.bottom - r.top) as f32 * TOP) as i32 - (M * s) as i32
        };
        self.win = RECT { left, top, right: left + ww, bottom: top + wh };
        let (bmp, bits) = win::dib(ww, wh);
        (self.dib, self.bits) = (bmp, bits);
        SelectObject(self.dc, bmp);
    }

    unsafe fn open(&mut self) {
        self.prefs_hide();
        self.reload();
        self.place();
        let (ww, wh) = self.size();
        self.back = win::capture(self.win.left, self.win.top, ww, wh, SHRINK);
        self.back.frost(blur(self.s));
        self.base_rows = usize::MAX;
        (self.query, self.caret, self.all, self.high) = (Vec::new(), 0, false, None);
        self.search();
        self.draw();
        ShowWindow(self.hwnd, SW_SHOWNA);
        SetForegroundWindow(self.hwnd); // permis : on vient de recevoir le raccourci (ou un clic sur l'icône)
        self.shown = true;
    }

    /// Fenêtre rangée : on rend ses images (fond, verre, bitmap) pour rester sous 30 Mo au repos.
    unsafe fn hide(&mut self) {
        if !self.shown {
            return;
        }
        self.shown = false;
        self.hidden_at = Instant::now();
        ShowWindow(self.hwnd, SW_HIDE);
        DeleteDC(self.dc); // libère d'abord la bitmap sélectionnée dedans
        self.dc = CreateCompatibleDC(null_mut());
        DeleteObject(self.dib);
        (self.dib, self.bits) = (null_mut(), null_mut());
        (self.back, self.base, self.hits) = (Back::empty(), Vec::new(), Vec::new());
        trim();
    }

    // ---------- saisie ----------

    /// Apps et fichiers mélangés à la note (correspondance + frecency ; bonus aux apps).
    unsafe fn search(&mut self) {
        let text: String = self.query.iter().collect();
        let q = fuzzy::Key::new(text.trim());
        self.sel = 0;
        self.hits.clear();
        if q.is_empty() {
            return;
        }
        let (fr, rows, cfg) = (self.fr(), self.cfg.rows, &self.cfg);
        let (now, hist) = (history::now(), &self.history);
        let mut found: Vec<(f32, Hit)> = Vec::new();
        // un calcul ou une conversion passe en tête
        if let Some(a) = calc::eval(&text).filter(|_| cfg.calc) {
            let (sub, glyph) = if a.conversion { ("Conversion", "\u{E8AB}") } else { (tr(fr, ("Calculation", "Calcul")), "\u{E8EF}") };
            found.push((f32::INFINITY, Hit { app: None, title: a.text, sub: sub.into(), path: String::new(), dir: false,
                copy: Some(a.copy), web: false, glyph }));
        }
        let mut apps: Vec<(f32, usize)> = self.apps.iter().enumerate()
            .filter_map(|(i, e)| Some((fuzzy::score(&q, &e.key, cfg.typos)? + APP_BONUS + FRECENCY * hist.frecency(&e.path, now), i)))
            .collect();
        apps.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
        for &(s, i) in apps.iter().take(rows) {
            let e = &self.apps[i];
            let sub = if e.store() { tr(fr, ("Store app", "App du Store")) } else { "Application" };
            found.push((s, Hit { app: Some(i), title: e.name.clone(), sub: sub.into(), path: e.path.clone(), dir: false,
                copy: None, web: false, glyph: "" }));
        }
        // pendant qu'un fil de surveillance écrit (quelques µs), on saute les fichiers pour cette frappe
        let files = self.files.clone();
        if let (true, Ok(idx)) = (cfg.files && q.len() >= FILE_MIN_QUERY, files.try_read()) {
            // plus large que l'affichage : la frecency peut faire remonter un fichier moins bien noté
            for (s, n) in idx.search(&q, 4 * rows) {
                let path = idx.path(n);
                let score = s + FRECENCY * hist.frecency(&path, now);
                found.push((score, Hit { app: None, title: idx.name(n).into(), sub: short_dir(&path), path, dir: idx.is_dir(n),
                    copy: None, web: false, glyph: "" }));
            }
        }
        found.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
        let room = if cfg.web { rows - 1 } else { rows };
        self.hits = found.into_iter().take(room).map(|f| f.1).collect();
        // toujours en dernier (seule ligne quand rien ne correspond)
        if cfg.web {
            let (q, engine) = (text.trim(), &cfg.engine);
            let name = settings::engine_name(engine);
            let title = if fr { format!("Rechercher « {q} » sur {name}") } else { format!("Search “{q}” on {name}") };
            self.hits.push(Hit { app: None, title, sub: tr(fr, ("Web search", "Recherche web")).into(),
                path: settings::web_url(engine, q), dir: false, copy: None, web: true, glyph: "\u{E774}" });
        }
        if self.hits.iter().any(|h| h.is_file() && !self.icons.contains_key(&icon_key(&h.path, h.dir))) {
            PostMessageW(self.hwnd, WM_ICONS, 0, 0);
        }
    }

    /// Icônes des fichiers affichés qui manquent (par type, une fois pour toutes ; cache borné). Appelé après le
    /// dessin : la première fois, le shell met jusqu'à ~25 ms par type, la frappe n'attend pas.
    unsafe fn load_icons(&mut self) {
        if self.icons.len() > 256 {
            self.icons.clear();
        }
        let mut loaded = false;
        for h in self.hits.iter().filter(|h| h.is_file()) {
            let key = icon_key(&h.path, h.dir);
            if !self.icons.contains_key(&key) {
                let px = if key == h.path { apps::icon(&h.path, self.icon_n) } else { apps::type_icon(&key, h.dir, self.icon_n) };
                self.icons.insert(key, px);
                loaded = true;
            }
        }
        if loaded {
            self.draw();
        }
    }

    /// Touche spéciale. Renvoie si la requête a changé (`None` : touche ignorée).
    unsafe fn key(&mut self, vk: u16) -> Option<bool> {
        let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
        let shift = GetKeyState(VK_SHIFT as i32) < 0;
        let n = self.query.len();
        let step = |sel: usize, d: isize, len: usize| if len == 0 { 0 } else { (sel as isize + d).rem_euclid(len as isize) as usize };
        match vk {
            VK_ESCAPE => self.hide(),
            VK_UP => self.sel = step(self.sel, -1, self.hits.len()),
            VK_DOWN => self.sel = step(self.sel, 1, self.hits.len()),
            k if ctrl && k == b'K' as u16 => self.sel = step(self.sel, -1, self.hits.len()),
            k if ctrl && k == b'J' as u16 => self.sel = step(self.sel, 1, self.hits.len()),
            // Ctrl+, : les paramètres, comme dans la plupart des apps
            VK_OEM_COMMA if ctrl => {
                self.hide();
                self.prefs_open();
            }
            VK_RETURN => self.activate(ctrl, shift),
            VK_LEFT => (self.caret, self.all) = (self.caret.saturating_sub(1), false),
            VK_RIGHT => (self.caret, self.all) = ((self.caret + 1).min(n), false),
            VK_HOME => (self.caret, self.all) = (0, false),
            VK_END => (self.caret, self.all) = (n, false),
            k if ctrl && k == b'A' as u16 => self.all = n > 0,
            // le texte sélectionné s'il y en a, sinon le résultat ou le chemin de la ligne choisie
            k if ctrl && k == b'C' as u16 => {
                let text = if self.all { Some(self.query.iter().collect()) } else {
                    self.hits.get(self.sel).map(|h| h.copy.clone().unwrap_or_else(|| h.path.clone()))
                };
                if let Some(t) = text.filter(|t: &String| !t.is_empty()) {
                    win::set_clipboard(self.hwnd, &t);
                }
            }
            VK_BACK | VK_DELETE if self.all => {
                (self.query, self.caret, self.all) = (Vec::new(), 0, false);
                return Some(true);
            }
            VK_BACK if ctrl => {
                // le mot avant le curseur, et les espaces qui le suivent
                let mut i = self.caret;
                while i > 0 && self.query[i - 1] == ' ' { i -= 1; }
                while i > 0 && self.query[i - 1] != ' ' { i -= 1; }
                self.query.drain(i..self.caret);
                self.caret = i;
                return Some(true);
            }
            VK_BACK if self.caret > 0 => {
                self.caret -= 1;
                self.query.remove(self.caret);
                return Some(true);
            }
            VK_DELETE if self.caret < n => {
                self.query.remove(self.caret);
                return Some(true);
            }
            k if ctrl && k == b'V' as u16 => {
                let Some(t) = win::clipboard_text(self.hwnd) else { return Some(false) };
                self.insert(&t.chars().collect::<Vec<_>>());
                return Some(true);
            }
            _ => return None,
        }
        Some(false)
    }

    /// Caractère tapé (WM_CHAR). Renvoie si la requête a changé.
    fn char(&mut self, u: u16) -> bool {
        if u < 0x20 || u == 0x7F {
            return false; // Entrée, Échap, Ctrl+lettre, Ctrl+Retour arrière : traités par `key`
        }
        if (0xD800..0xDC00).contains(&u) {
            self.high = Some(u);
            return false;
        }
        let units = match self.high.take() {
            Some(h) => vec![h, u],
            None => vec![u],
        };
        let chars: Vec<char> = char::decode_utf16(units).filter_map(Result::ok).collect();
        self.insert(&chars);
        !chars.is_empty()
    }

    fn insert(&mut self, chars: &[char]) {
        if self.all {
            (self.query, self.caret, self.all) = (Vec::new(), 0, false);
        }
        self.query.splice(self.caret..self.caret, chars.iter().copied());
        self.caret += chars.len();
    }

    /// Entrée : ouvrir ; Ctrl : dossier parent ; Maj : en admin.
    unsafe fn activate(&mut self, parent: bool, admin: bool) {
        let Some(h) = self.hits.get(self.sel) else { return };
        if let Some(text) = h.copy.clone() {
            win::set_clipboard(self.hwnd, &text);
            return self.hide();
        }
        if h.web {
            let url = h.path.clone();
            self.hide();
            return apps::launch(&url, false, false); // navigateur par défaut ; Ctrl et Maj n'ont pas de sens ici
        }
        let path = h.path.clone();
        // rangée d'abord : sinon la fenêtre de l'UAC (admin) nous ferait perdre le focus pendant l'appel
        self.hide();
        apps::launch(&path, parent, admin);
        if !parent {
            self.history.bump(&path, history::now());
        }
    }

    // ---------- dessin ----------

    unsafe fn draw(&mut self) {
        if self.bits.is_null() {
            return;
        }
        let (s, fr) = (self.s, self.fr());
        let look = look(self.cfg.theme);
        let ink = look.ink;
        let (ww, wh) = self.size();
        let rr = self.shape();
        let n = self.hits.len();
        if self.base_rows != n {
            // le verre ne dépend que du nombre de lignes : recalculé seulement quand il change
            let sep = [Pill { rr: Rr { x: rr.x, y: rr.y + FIELD * s, w: rr.w, h: s.max(1.0), r: 0.0 }, color: ink, amount: 0.1, rim: 0.0 }];
            let f = Frame {
                back: &self.back, layers: &[], a: rr, b: None, smooth: 0.0,
                refr_height: 22.0 * s, refr_amount: 36.0 * s, glyph: false,
                pills: if n > 0 { &sep } else { &[] }, alpha: 1.0, scale: s, look,
            };
            self.base.resize(ww * wh, 0);
            render::render(&f, &mut self.base, ww);
            self.base_rows = n;
        }
        let out = std::slice::from_raw_parts_mut(self.bits, ww * wh);
        out.copy_from_slice(&self.base);

        let px = |v: f32| (v * s) as i32;
        let rect = |x: f32, y: f32, w: f32, h: f32| RECT { left: px(x), top: px(y), right: px(x + w), bottom: px(y + h) };
        let t = |txt, r, size: f32, weight, flags, level| win::Text { s: txt, r, size: size * s, weight, flags, level };
        let at = |x: f32, y: f32, w: f32, h: f32, r: f32| Rr { x: rr.x + x * s, y: rr.y + y * s, w: w * s, h: h * s, r: r * s };

        // champ de recherche
        let q: String = self.query.iter().collect();
        let prefix: String = self.query[..self.caret].iter().collect();
        let (text_w, caret_x) = (win::text_width(&q, QUERY_SIZE * s, 400), win::text_width(&prefix, QUERY_SIZE * s, 400));
        let placeholder = tr(fr, ("Search apps, files, calculations…", "Rechercher des apps, fichiers, calculs…"));
        let field_text = if q.is_empty() { placeholder } else { &q };
        // shortcut: une requête plus large que le champ est coupée à droite ; défilement horizontal si ça gêne
        let mut texts = vec![
            t("\u{E721}", rect(14.0, 0.0, 30.0, FIELD), 18.0, 400, DT_CENTER | DT_VCENTER, 0.6),
            t(field_text, rect(TEXT_X, 0.0, W - TEXT_X - 20.0, FIELD), QUERY_SIZE, 400, DT_LEFT | DT_VCENTER, if q.is_empty() { 0.38 } else { 1.0 }),
        ];
        if self.all && !q.is_empty() {
            fill(out, ww, Rr { x: rr.x + TEXT_X * s - 2.0 * s, y: rr.y + 14.0 * s, w: text_w as f32 + 4.0 * s, h: 28.0 * s, r: 5.0 * s }, ACCENT, 0.45);
        } else {
            fill(out, ww, Rr { x: rr.x + TEXT_X * s + caret_x as f32, y: rr.y + 16.0 * s, w: (1.5 * s).max(1.0), h: 24.0 * s, r: 0.0 }, ink, 0.9);
        }

        // résultats
        for (i, h) in self.hits.iter().enumerate() {
            let y = FIELD + 4.0 + i as f32 * ROW;
            let selected = i == self.sel;
            if selected {
                fill(out, ww, at(8.0, y, W - 16.0, ROW, 12.0), ink, 0.12);
                let hint = if h.copy.is_some() {
                    tr(fr, ("↵ Copy", "↵ Copier"))
                } else if h.web {
                    tr(fr, ("↵ Search", "↵ Rechercher"))
                } else if h.path.starts_with("shell:") {
                    tr(fr, ("↵ Open", "↵ Ouvrir"))
                } else {
                    tr(fr, ("↵ Open · Ctrl ↵ Folder · Shift ↵ Admin", "↵ Ouvrir · Ctrl ↵ Dossier · Maj ↵ Admin"))
                };
                texts.push(t(hint, rect(W - 300.0, y, 280.0, ROW), 11.0, 400, DT_RIGHT | DT_VCENTER, 0.45));
            }
            // icône centrée dans sa case (elle est à l'échelle de l'écran principal)
            let slot = at(20.0, y + (ROW - ICON) / 2.0, ICON, ICON, 8.0);
            let icon = match h.app {
                Some(k) => Some((&self.apps[k].icon, self.apps[k].icon_n)),
                None if h.is_file() => self.icons.get(&icon_key(&h.path, h.dir)).map(|px| (px, self.icon_n)),
                None => None, // calcul, recherche web : leur pictogramme
            };
            match icon.filter(|i| !i.0.is_empty()) {
                Some((px, n)) => {
                    let x0 = (slot.x + (slot.w - n as f32) / 2.0).max(0.0) as usize;
                    let y0 = (slot.y + (slot.h - n as f32) / 2.0).max(0.0) as usize;
                    blit(out, ww, px, n, x0, y0);
                }
                None => {
                    fill(out, ww, slot, ink, 0.14);
                    if !h.glyph.is_empty() {
                        texts.push(t(h.glyph, rect(20.0, y + (ROW - ICON) / 2.0, ICON, ICON), 16.0, 400, DT_CENTER | DT_VCENTER, 0.85));
                    }
                }
            }
            let tw = if selected { W - 64.0 - 310.0 } else { W - 64.0 - 20.0 };
            texts.push(t(&h.title, rect(64.0, y + 5.0, tw, 22.0), 14.5, 600, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS, 0.95));
            texts.push(t(&h.sub, rect(64.0, y + 26.0, tw, 16.0), 11.5, 400, DT_LEFT | DT_VCENTER | DT_PATH_ELLIPSIS, 0.55));
        }

        let lw = rr.w as usize;
        let alpha = win::text_alpha(lw, rr.h as usize, &texts);
        paint::text(out, ww, &alpha, lw, rr.x as usize, rr.y as usize, ink);
        paint::present(self.hwnd, self.dc, self.win);
    }

    // ---------- zone de notification ----------

    unsafe fn tray_menu(&mut self) {
        let fr = self.fr();
        let items = [(ID_SETTINGS, tr(fr, ("Settings…", "Paramètres…")), false), (ID_REINDEX, tr(fr, ("Rebuild index", "Réindexer")), false),
            (ID_STARTUP, tr(fr, ("Launch at startup", "Démarrer avec Windows")), self.cfg.autostart), (0, "", false),
            (ID_QUIT, tr(fr, ("Quit", "Quitter")), false)];
        match win::menu(self.hwnd, &items) {
            ID_SETTINGS => self.prefs_open(),
            ID_REINDEX => {
                self.cfg = settings::load(); // `pinned=`, `roots=` ont pu changer
                self.reindex();
                self.index_files();
            }
            ID_STARTUP => {
                self.cfg.autostart = !self.cfg.autostart;
                settings::set("autostart", if self.cfg.autostart { "1" } else { "0" });
                settings::set_startup(self.cfg.autostart);
            }
            ID_QUIT => {
                DestroyWindow(self.hwnd);
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            app(|a| a.toggle());
        }
        WM_SHOW => {
            app(|a| if !a.shown { a.open() });
        }
        WM_INDEXED => {
            app(|a| a.indexed());
        }
        WM_ICONS => {
            app(|a| a.load_icons());
        }
        WM_FILES => {
            app(|a| a.files_dirty = true);
            SetTimer(h, T_SAVE, SAVE_AFTER_MS, None); // réarmée à chaque changement : enregistre après le calme
        }
        WM_TIMER if wp == T_SAVE => {
            KillTimer(h, T_SAVE);
            app(|a| a.save_files(false));
        }
        WM_TRAY => match lp as u32 {
            WM_LBUTTONUP => {
                // le clic sur l'icône a déjà rangé la fenêtre (perte du focus) : ne pas la rouvrir aussitôt
                app(|a| if !a.shown && a.hidden_at.elapsed().as_millis() > 250 { a.open() });
            }
            WM_RBUTTONUP => {
                app(|a| a.tray_menu());
            }
            _ => {}
        },
        WM_KEYDOWN => {
            app(|a| match a.key(wp as u16) {
                Some(true) => {
                    a.search();
                    a.draw();
                }
                Some(false) => a.draw(),
                None => {}
            });
        }
        WM_CHAR => {
            app(|a| if a.char(wp as u16) {
                a.search();
                a.draw();
            });
        }
        // Alt relâché juste après Alt+Espace : pas de boucle de menu système (elle mangerait la frappe suivante)
        WM_SYSCOMMAND if (wp & 0xFFF0) as u32 == SC_KEYMENU => {}
        WM_ACTIVATE if wp & 0xFFFF == WA_INACTIVE as usize => {
            app(|a| a.hide());
        }
        WM_CLOSE => {
            app(|a| a.hide()); // Alt+F4 range, ne quitte pas
        }
        WM_DESTROY => {
            app(|a| a.save_files(true));
            UnregisterHotKey(h, HOTKEY_ID);
            win::tray(h, WM_TRAY, NIM_DELETE, null_mut(), "");
            PostQuitMessage(0);
        }
        // Explorer redémarré : l'icône a disparu, on la remet
        _ if app(|a| a.taskbar_created == msg).unwrap_or(false) => {
            app(|a| win::tray(h, WM_TRAY, NIM_ADD, a.icon, &format!("Lense — {}", a.hotkey)));
        }
        _ => return DefWindowProcW(h, msg, wp, lp),
    }
    0
}

/// Les deux fenêtres en couches : le lanceur et le panneau Paramètres.
unsafe fn create_windows() -> (HWND, HWND) {
    let inst = GetModuleHandleW(null());
    let make = |name: &str, wndproc: WNDPROC| {
        let class = win::w(name);
        let wc = WNDCLASSW { lpfnWndProc: wndproc, hInstance: inst, lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW), ..zeroed() };
        RegisterClassW(&wc);
        CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TOPMOST, class.as_ptr(), win::w("Lense").as_ptr(), WS_POPUP,
            0, 0, 1, 1, null_mut(), null_mut(), inst, null())
    };
    (make("LENSE", Some(proc)), make(prefs::CLASS, Some(prefs::proc)))
}

fn working_set_mb() -> f64 {
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS = zeroed();
        c.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb);
        c.WorkingSetSize as f64 / 1_048_576.0
    }
}

/// `lense --bench` : ouvre la fenêtre 10 fois, tape une requête lettre par lettre, mesure. Sortie console seulement.
fn bench() {
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        format!("p50 {:.1} ms · p99 {:.1} ms · max {:.1} ms", v[v.len() / 2], v[v.len() * 99 / 100], v[v.len() - 1])
    };
    unsafe {
        // l'exe release n'a pas de console : on écrit dans celle du terminal qui l'a lancé
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        let (h, ph) = create_windows();
        let mut a = App::new(h, ph);
        let rows = a.cfg.rows;
        let t = Instant::now();
        a.apps = apps::index(&a.cfg.pinned, ICON as usize);
        let with_icon = a.apps.iter().filter(|e| !e.icon.is_empty()).count();
        let store = a.apps.iter().filter(|e| e.store()).count();
        println!("apps : {} ({store} du Store, {with_icon} avec icône) indexées en {:.0} ms", a.apps.len(), ms(t.elapsed()));
        // fichiers : scan complet, taille de l'index, aller-retour files.idx (en mémoire), requêtes
        let roots = files::resolve_roots(&a.cfg.roots);
        let t = Instant::now();
        let idx = files::Index::scan(roots.clone(), &a.cfg.ignore);
        let scan = ms(t.elapsed());
        let bytes = idx.to_bytes();
        let t = Instant::now();
        let back = files::Index::from_bytes(&bytes);
        println!("fichiers : {} entrées, {} racines, scan {scan:.0} ms · files.idx {:.1} Mo, relu en {:.0} ms ({})",
            idx.len(), roots.len(), bytes.len() as f64 / 1_048_576.0, ms(t.elapsed()), if back.is_some() { "ok" } else { "ÉCHEC" });
        let mut qs = Vec::new();
        for q in ["ma", "doc", "pdf", "rap", "lens", "main rs", "image", "facture", "readme", "png", "xx", "config"] {
            let k = fuzzy::Key::new(q);
            let t = Instant::now();
            idx.search(&k, 4 * rows);
            qs.push(ms(t.elapsed()));
        }
        qs.sort_by(f64::total_cmp);
        println!("requête fichiers : p50 {:.1} ms · max {:.1} ms", qs[qs.len() / 2], qs[qs.len() - 1]);
        *a.files.write().unwrap() = idx;
        let (mut opens, mut keys) = (Vec::new(), Vec::new());
        for _ in 0..10 {
            let t = Instant::now();
            a.open();
            opens.push(ms(t.elapsed()));
            for c in "visual studio".encode_utf16() {
                let (t, n) = (Instant::now(), a.hits.len());
                a.char(c);
                a.search();
                let ts = Instant::now();
                a.draw();
                let d = ms(t.elapsed());
                keys.push(d);
                if d > 10.0 {
                    println!("  frappe lente {d:.1} ms (recherche {:.1}) : {n} → {} lignes", ms(ts - t), a.hits.len());
                }
            }
            let t = Instant::now();
            a.key(VK_DOWN);
            a.draw();
            keys.push(ms(t.elapsed()));
            a.hide();
        }
        // détail de l'ouverture : capture puis flou du fond
        let (ww, wh) = (((W + 2.0 * M) * a.s) as usize, ((shape_h(rows) + 2.0 * M) * a.s) as usize);
        let t = Instant::now();
        let mut back = win::capture(0, 0, ww, wh, SHRINK);
        let cap = ms(t.elapsed());
        let t = Instant::now();
        back.frost(blur(a.s));
        let frost = ms(t.elapsed());
        let s = a.s;
        let glass = |n: usize| {
            let rr = Rr { x: M * s, y: M * s, w: W * s, h: shape_h(n) * s, r: RADIUS * s };
            let f = Frame { back: &back, layers: &[], a: rr, b: None, smooth: 0.0, refr_height: 22.0 * s, refr_amount: 36.0 * s,
                glyph: false, pills: &[], alpha: 1.0, scale: s, look: look(0) };
            let mut out = vec![0u32; ww * wh];
            let t = Instant::now();
            render::render(&f, &mut out, ww);
            ms(t.elapsed())
        };
        let t = Instant::now();
        let txt = win::text_alpha((W * s) as usize, (shape_h(rows) * s) as usize, &[]);
        let text = ms(t.elapsed());
        drop(txt);
        println!("écran {:.0} % · fenêtre {ww}×{wh} px · capture {cap:.1} · flou {frost:.1} · verre 0 ligne {:.1} · {rows} lignes {:.1} · calque texte {text:.1} ms",
            s * 100.0, glass(0), glass(rows));
        println!("ouverture : {}  (cible < 50 ms)", stats(&mut opens));
        println!("frappe    : {}  (cible < 16 ms)", stats(&mut keys));
        println!("RAM fenêtre cachée : {:.1} Mo  (cible < 30 Mo)", working_set_mb());
    }
}

fn main() {
    settings::migrate();
    if std::env::args().any(|a| a == "--bench") {
        return bench();
    }
    unsafe {
        // une seule instance : la relancer ouvre la fenêtre existante
        CreateMutexW(null(), 0, win::w("Local\\Lense.Instance").as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let h = FindWindowW(win::w("LENSE").as_ptr(), null());
            if !h.is_null() {
                let mut pid = 0;
                GetWindowThreadProcessId(h, &mut pid);
                AllowSetForegroundWindow(pid); // c'est nous qui avons le premier plan : on le lui passe
                PostMessageW(h, WM_SHOW, 0, 0);
            }
            return;
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32); // ShellExecute sur `shell:AppsFolder\…`, choix de dossier
        let (h, ph) = create_windows();
        let mut a = App::new(h, ph);
        a.icon = win::lens_icon();
        win::tray(h, WM_TRAY, NIM_ADD, a.icon, "Lense");
        a.register();
        settings::set_startup(a.cfg.autostart);
        a.reindex();
        a.index_files();
        APP.with_borrow_mut(|s| *s = Some(a));

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frecency_lifts_what_you_open() {
        let names = ["PowerShell", "Photoshop", "Paint"];
        let keys: Vec<fuzzy::Key> = names.iter().map(|n| fuzzy::Key::new(n)).collect();
        let q = fuzzy::Key::new("ps");
        let now = 1_800_000_000;
        let mut h = history::History::default();
        // même calcul que `search`, sur trois noms
        let top = |h: &history::History| {
            let mut v: Vec<(f32, usize)> = keys.iter().zip(names).enumerate()
                .filter_map(|(i, (k, n))| Some((fuzzy::score(&q, k, true)? + FRECENCY * h.frecency(n, now), i))).collect();
            v.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
            v.into_iter().map(|x| x.1).collect::<Vec<_>>()
        };
        assert_eq!(top(&h), [0, 1], "sans historique : PowerShell (CamelCase) ; Paint ne correspond pas");
        for _ in 0..3 {
            h.bump("Photoshop", now - 86_400);
        }
        assert_eq!(top(&h)[0], 1, "Photoshop, lancé souvent hier, passe devant");
        // une app lancée une fois il y a longtemps ne bat pas une meilleure correspondance
        let mut old = history::History::default();
        old.bump("Photoshop", now - 120 * 86_400);
        assert_eq!(top(&old)[0], 0);
    }

    #[test]
    fn file_rows() {
        assert_eq!(icon_key(r"C:\a\photo.JPG", false), "jpg", "par type");
        assert_eq!(icon_key(r"C:\a\outil.exe", false), r"C:\a\outil.exe", "icône propre");
        assert_eq!(icon_key(r"C:\a\b", true), "<dossier>");
        assert_eq!(icon_key(r"C:\a\Makefile", false), "");
        let home = std::env::var("USERPROFILE").unwrap();
        assert_eq!(short_dir(&format!(r"{home}\Documents\x.txt")), r"~\Documents");
        assert_eq!(short_dir(r"D:\data\x.txt"), r"D:\data");
    }

    #[test]
    fn themes_keep_text_readable() {
        // texte clair sur verre sombre, foncé sur verre clair
        assert!(look(0).ink[0] > 0.9 && look(1).ink[0] > 0.9 && look(2).ink[0] < 0.2);
        assert_eq!(tr(false, ("Open", "Ouvrir")), "Open");
        assert_eq!(tr(true, ("Open", "Ouvrir")), "Ouvrir");
    }
}
