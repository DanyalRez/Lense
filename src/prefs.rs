//! Panneau Paramètres : une fenêtre en verre au milieu de l'écran, trois onglets (Général, Recherche, Dossiers).
//! Chaque changement s'applique tout de suite et s'écrit dans settings.txt (les lignes ajoutées à la main restent).

use crate::{
    app, blur, files, look, monitor, paint, render::{self, Back, Frame, Rr}, settings, short_dir, tr, trim, win, App, Tr,
    ACCENT, HOTKEY_ID, M, SHRINK,
};
use std::{mem::zeroed, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::Com::CoTaskMemFree,
    UI::{Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

pub const CLASS: &str = "LENSE_SETTINGS";
// pixels logiques, relatifs au panneau
const PW: f32 = 560.0;
const RADIUS: f32 = 26.0;
const TAB_Y: f32 = 58.0;
const ROWS_Y: f32 = 112.0;
const BTN_W: f32 = 160.0;
const SEG_Y: f32 = 24.0; // la barre de choix sous le libellé d'une ligne
const MAX_FOLDERS: usize = 8;

const TABS: [Tr; 3] = [("General", "Général"), ("Search", "Recherche"), ("Folders", "Dossiers")];
const LANGS: [Tr; 2] = [("English", "English"), ("Français", "Français")];
const THEMES: [Tr; 3] = [("Glass", "Verre"), ("Dark", "Sombre"), ("Light", "Clair")];
const POSITIONS: [Tr; 2] = [("Top", "En haut"), ("Center", "Au centre")];
const ROW_COUNTS: [Tr; 3] = [("5", "5"), ("8", "8"), ("10", "10")];
const ENGINE_NAMES: [Tr; 4] = [("Google", "Google"), ("DuckDuckGo", "DuckDuckGo"), ("Bing", "Bing"), ("Ecosia", "Ecosia")];
const DEFAULT_ROOTS: [&str; 3] = ["Desktop", "Documents", "Downloads"];

/// Ce qu'une ligne règle : elle se repère par là, pas par sa place.
#[derive(Clone, Copy, PartialEq)]
enum Key {
    Lang,
    Theme,
    Position,
    Rows,
    Hotkey,
    Startup,
    Engine,
    Files,
    Typos,
    Calc,
    Web,
    History,
    Folder(usize),
    AddFolder,
    Reindex,
    EditFile,
}

#[derive(Clone, Copy)]
enum Ctl {
    Seg(&'static [Tr]),
    Toggle,
    Button(Tr), // texte du bouton (le raccourci affiche le sien)
}

struct Row {
    label: String,
    sub: String, // ligne d'explication sous le libellé, vide si aucune
    ctl: Ctl,
    key: Key,
}

fn row_h(r: &Row) -> f32 {
    match r.ctl {
        Ctl::Seg(_) => 70.0,
        _ if !r.sub.is_empty() => 54.0,
        _ => 44.0,
    }
}

/// Fenêtre du panneau : place, bitmap, fond flouté et verre en cache ; libérés à la fermeture.
pub struct Panel {
    hwnd: HWND,
    shown: bool,
    win: RECT,
    s: f32,
    dc: HDC,
    dib: HBITMAP,
    bits: *mut u32,
    back: Back,
    base: Vec<u32>,
    base_key: (u32, usize), // hauteur (px) et thème pour lesquels `base` a été calculé
    tab: usize,
    capture: bool,     // en attente du nouveau raccourci
    note: Option<Tr>,  // message sous le raccourci (refusé, déjà pris)
}

impl Panel {
    pub unsafe fn new(hwnd: HWND) -> Panel {
        Panel { hwnd, shown: false, win: zeroed(), s: 1.0, dc: CreateCompatibleDC(null_mut()), dib: null_mut(), bits: null_mut(),
            back: Back::empty(), base: Vec::new(), base_key: (0, 0), tab: 0, capture: false, note: None }
    }

    fn size(&self) -> (usize, usize) {
        ((self.win.right - self.win.left) as usize, (self.win.bottom - self.win.top) as usize)
    }
}

/// Choix d'un dossier (boîte de dialogue de Windows) ; COM est déjà initialisé (STA) sur ce fil.
unsafe fn pick_folder(owner: HWND, title: &str) -> Option<String> {
    let t = win::w(title);
    let bi = BROWSEINFOW { hwndOwner: owner, lpszTitle: t.as_ptr(), ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE, ..zeroed() };
    let pidl = SHBrowseForFolderW(&bi);
    if pidl.is_null() {
        return None;
    }
    let mut buf = [0u16; 260];
    let ok = SHGetPathFromIDListW(pidl, buf.as_mut_ptr()) != 0;
    CoTaskMemFree(pidl as _);
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    (ok && len > 0).then(|| String::from_utf16_lossy(&buf[..len]))
}

impl App {
    /// Dossiers indexés tels qu'écrits ; vide dans les réglages = les trois dossiers de Windows.
    fn root_list(&self) -> Vec<String> {
        if self.cfg.roots.is_empty() { DEFAULT_ROOTS.map(String::from).to_vec() } else { self.cfg.roots.clone() }
    }

    fn prefs_rows(&self, tab: usize) -> Vec<Row> {
        let fr = self.fr();
        let row = |l: Tr, ctl, key| Row { label: tr(fr, l).into(), sub: String::new(), ctl, key };
        let with = |mut r: Row, sub: Tr| {
            r.sub = tr(fr, sub).into();
            r
        };
        match tab {
            0 => {
                let mut hk = row(("Shortcut", "Raccourci"), Ctl::Button(("", "")), Key::Hotkey);
                if self.prefs.capture {
                    hk.sub = tr(fr, ("Press the new shortcut · Esc to cancel", "Appuyez sur le nouveau raccourci · Échap pour annuler")).into();
                } else if let Some(n) = self.prefs.note {
                    hk.sub = tr(fr, n).into();
                }
                vec![
                    row(("Language", "Langue"), Ctl::Seg(&LANGS), Key::Lang),
                    row(("Theme", "Thème"), Ctl::Seg(&THEMES), Key::Theme),
                    row(("Position on screen", "Position à l'écran"), Ctl::Seg(&POSITIONS), Key::Position),
                    row(("Results shown", "Résultats affichés"), Ctl::Seg(&ROW_COUNTS), Key::Rows),
                    hk,
                    row(("Launch at startup", "Démarrer avec Windows"), Ctl::Toggle, Key::Startup),
                ]
            }
            1 => {
                let n = self.history.len();
                let mut hist = row(("Search history", "Historique"), Ctl::Button(("Clear", "Effacer")), Key::History);
                hist.sub = if fr { format!("{n} éléments retenus pour classer les résultats") } else { format!("{n} items remembered to rank results") };
                vec![
                    row(("Web search engine", "Moteur de recherche web"), Ctl::Seg(&ENGINE_NAMES), Key::Engine),
                    with(row(("Files", "Fichiers"), Ctl::Toggle, Key::Files), ("Show files from the indexed folders", "Afficher les fichiers des dossiers indexés")),
                    with(row(("Typo tolerance", "Tolérance aux fautes"), Ctl::Toggle, Key::Typos), ("“crhome” still finds Chrome", "« crhome » trouve quand même Chrome")),
                    with(row(("Calculator and conversions", "Calcul et conversions"), Ctl::Toggle, Key::Calc), ("12*4+3, 10 km in mi, 1 GB in MB", "12*4+3, 10 km en mi, 1 Go en Mo")),
                    with(row(("Web search", "Recherche web"), Ctl::Toggle, Key::Web), ("Always the last result", "Toujours le dernier résultat")),
                    hist,
                ]
            }
            _ => {
                let mut v: Vec<Row> = self.root_list().iter().take(MAX_FOLDERS).enumerate().map(|(i, r)| {
                    let label = match r.to_ascii_lowercase().as_str() {
                        "desktop" => tr(fr, ("Desktop", "Bureau")).to_string(),
                        "documents" => "Documents".into(),
                        "downloads" => tr(fr, ("Downloads", "Téléchargements")).to_string(),
                        _ => r.rsplit('\\').next().unwrap_or(r).to_string(),
                    };
                    // chemin réel, profil abrégé en ~ (short_dir garde le dossier d'un chemin : on lui en donne un enfant)
                    let path = files::resolve_roots(std::slice::from_ref(r)).pop().unwrap_or_default();
                    Row { label, sub: short_dir(&format!("{path}\\.")), ctl: Ctl::Button(("Remove", "Retirer")), key: Key::Folder(i) }
                }).collect();
                let count = self.files.try_read().map_or(0, |i| i.len());
                let mut re = row(("Index", "Index"), Ctl::Button(("Rebuild", "Réindexer")), Key::Reindex);
                re.sub = if fr { format!("{count} fichiers indexés") } else { format!("{count} files indexed") };
                v.push(row(("Add a folder", "Ajouter un dossier"), Ctl::Button(("Add…", "Ajouter…")), Key::AddFolder));
                v.push(re);
                v.push(with(row(("Advanced", "Avancé"), Ctl::Button(("Open file", "Ouvrir le fichier")), Key::EditFile),
                    ("Ignored folders, extra apps: settings.txt", "Dossiers ignorés, apps en plus : settings.txt")));
                v
            }
        }
    }

    fn prefs_h(&self, tab: usize) -> f32 {
        ROWS_Y + self.prefs_rows(tab).iter().map(row_h).sum::<f32>() + 12.0
    }

    /// Case choisie d'une barre de choix (`usize::MAX` : aucune, ex. moteur personnalisé), ou interrupteur (0 / 1).
    fn prefs_value(&self, key: Key) -> usize {
        let c = &self.cfg;
        match key {
            Key::Lang => c.french as usize,
            Key::Theme => c.theme,
            Key::Position => c.center as usize,
            Key::Rows => settings::ROWS.iter().position(|&n| n == c.rows).unwrap_or(1),
            Key::Engine => settings::ENGINES.iter().position(|e| e.1 == c.engine).unwrap_or(usize::MAX),
            Key::Startup => c.autostart as usize,
            Key::Files => c.files as usize,
            Key::Typos => c.typos as usize,
            Key::Calc => c.calc as usize,
            Key::Web => c.web as usize,
            _ => 0,
        }
    }

    // ---------- ouverture, fermeture ----------

    /// Au milieu de l'écran de la souris ; la fenêtre a la hauteur du plus grand onglet, la forme suit l'onglet.
    pub unsafe fn prefs_open(&mut self) {
        if self.prefs.shown {
            SetForegroundWindow(self.prefs.hwnd);
            return;
        }
        self.reload(); // reflète aussi ce qui a été changé à la main dans settings.txt
        let (r, s) = monitor();
        let max_h = (0..TABS.len()).map(|t| self.prefs_h(t)).fold(0.0, f32::max);
        let p = &mut self.prefs;
        p.s = s;
        let (ww, wh) = (((PW + 2.0 * M) * s) as i32, ((max_h + 2.0 * M) * s) as i32);
        let left = r.left + (r.right - r.left - ww) / 2;
        let top = r.top + ((r.bottom - r.top - wh) / 2).max(0);
        p.win = RECT { left, top, right: left + ww, bottom: top + wh };
        let (bmp, bits) = win::dib(ww, wh);
        (p.dib, p.bits) = (bmp, bits);
        SelectObject(p.dc, bmp);
        p.back = win::capture(left, top, ww as usize, wh as usize, SHRINK);
        p.back.frost(blur(s));
        p.base_key = (0, 0);
        self.prefs_draw();
        ShowWindow(self.prefs.hwnd, SW_SHOWNA);
        SetForegroundWindow(self.prefs.hwnd);
        self.prefs.shown = true;
    }

    pub unsafe fn prefs_hide(&mut self) {
        let p = &mut self.prefs;
        if !p.shown {
            return;
        }
        if std::mem::take(&mut p.capture) {
            self.register(); // fermé pendant la saisie d'un raccourci : l'ancien revient
        }
        let p = &mut self.prefs;
        p.shown = false;
        p.note = None;
        ShowWindow(p.hwnd, SW_HIDE);
        DeleteDC(p.dc);
        p.dc = CreateCompatibleDC(null_mut());
        DeleteObject(p.dib);
        (p.dib, p.bits) = (null_mut(), null_mut());
        (p.back, p.base) = (Back::empty(), Vec::new());
        trim();
    }

    /// La hauteur maximale a changé (dossier ajouté ou retiré) : on rouvre, sur le même onglet.
    unsafe fn prefs_reopen(&mut self) {
        self.prefs_hide();
        self.prefs_open();
    }

    // ---------- dessin ----------

    unsafe fn prefs_draw(&mut self) {
        if self.prefs.bits.is_null() {
            return;
        }
        let (fr, theme) = (self.fr(), self.cfg.theme);
        let look = look(theme);
        let ink = look.ink;
        let tab = self.prefs.tab;
        let rows = self.prefs_rows(tab);
        let values: Vec<usize> = rows.iter().map(|r| self.prefs_value(r.key)).collect();
        let h = self.prefs_h(tab);
        let hotkey = self.cfg.hotkey.clone();
        let p = &mut self.prefs;
        let s = p.s;
        let (ww, wh) = p.size();
        let rr = Rr { x: M * s, y: M * s, w: PW * s, h: h * s, r: RADIUS * s };
        if p.base_key != (rr.h as u32, theme) {
            let f = Frame { back: &p.back, layers: &[], a: rr, b: None, smooth: 0.0, refr_height: 22.0 * s, refr_amount: 36.0 * s,
                glyph: false, pills: &[], alpha: 1.0, scale: s, look };
            p.base.resize(ww * wh, 0);
            render::render(&f, &mut p.base, ww);
            p.base_key = (rr.h as u32, theme);
        }
        let out = std::slice::from_raw_parts_mut(p.bits, ww * wh);
        out.copy_from_slice(&p.base);

        let px = |v: f32| (v * s) as i32;
        let rect = |x: f32, y: f32, w: f32, h: f32| RECT { left: px(x), top: px(y), right: px(x + w), bottom: px(y + h) };
        let at = |x: f32, y: f32, w: f32, h: f32, r: f32| Rr { x: rr.x + x * s, y: rr.y + y * s, w: w * s, h: h * s, r: r * s };
        let t = |txt, r, size: f32, weight, flags, level| win::Text { s: txt, r, size: size * s, weight, flags, level };
        let center = DT_CENTER | DT_VCENTER;
        let mut texts = vec![
            t(tr(fr, ("Settings", "Paramètres")), rect(24.0, 16.0, 300.0, 30.0), 18.0, 600, DT_LEFT | DT_VCENTER, 1.0),
            t(tr(fr, ("Esc to close", "Échap pour fermer")), rect(PW - 224.0, 16.0, 200.0, 30.0), 11.0, 400, DT_RIGHT | DT_VCENTER, 0.4),
        ];
        // onglets : une barre de choix comme les autres
        let seg = |out: &mut [u32], y: f32, n: usize, k: usize| {
            fill_seg(out, ww, at(20.0, y, PW - 40.0, 34.0, 17.0), n, k, ink, s);
        };
        seg(out, TAB_Y, TABS.len(), tab);
        let tw = (PW - 40.0) / TABS.len() as f32;
        for (k, name) in TABS.iter().enumerate() {
            texts.push(t(tr(fr, *name), rect(20.0 + k as f32 * tw, TAB_Y, tw, 34.0), 12.5, 600, center, if k == tab { 1.0 } else { 0.6 }));
        }
        let mut y = ROWS_Y;
        for (row, &v) in rows.iter().zip(&values) {
            let hgt = row_h(row);
            match row.ctl {
                Ctl::Seg(opts) => {
                    texts.push(t(&row.label, rect(24.0, y, PW - 48.0, 18.0), 12.5, 500, DT_LEFT | DT_VCENTER, 0.62));
                    seg(out, y + SEG_Y, opts.len(), v);
                    let w = (PW - 40.0) / opts.len() as f32;
                    for (k, o) in opts.iter().enumerate() {
                        texts.push(t(tr(fr, *o), rect(20.0 + k as f32 * w, y + SEG_Y, w, 34.0), 12.5, 600, center, if k == v { 1.0 } else { 0.62 }));
                    }
                }
                Ctl::Toggle | Ctl::Button(_) => {
                    if row.sub.is_empty() {
                        texts.push(t(&row.label, rect(24.0, y, PW - BTN_W - 60.0, 44.0), 13.5, 500, DT_LEFT | DT_VCENTER, 1.0));
                    } else {
                        texts.push(t(&row.label, rect(24.0, y + 6.0, PW - BTN_W - 60.0, 22.0), 13.5, 500, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS, 1.0));
                        texts.push(t(&row.sub, rect(24.0, y + 28.0, PW - BTN_W - 50.0, 18.0), 11.5, 400, DT_LEFT | DT_VCENTER | DT_PATH_ELLIPSIS, 0.55));
                    }
                    let mid = y + (hgt - 30.0) / 2.0;
                    if let Ctl::Button(label) = row.ctl {
                        let capturing = row.key == Key::Hotkey && p.capture;
                        let b = at(PW - 24.0 - BTN_W, mid, BTN_W, 30.0, 15.0);
                        if capturing {
                            paint::fill(out, ww, b, ACCENT, 0.55);
                        } else {
                            paint::fill(out, ww, b, ink, 0.1);
                        }
                        let text = match row.key {
                            Key::Hotkey if capturing => "…",
                            Key::Hotkey => hotkey.as_str(),
                            _ => tr(fr, label),
                        };
                        texts.push(t(text, rect(PW - 24.0 - BTN_W, mid, BTN_W, 30.0), 12.5, 600, center, 1.0));
                    } else {
                        // interrupteur : rail coloré quand il est allumé, bouton blanc qui glisse
                        let on = v == 1;
                        let track = at(PW - 24.0 - 50.0, mid, 50.0, 30.0, 15.0);
                        if on {
                            paint::fill(out, ww, track, ACCENT, 0.9);
                        } else {
                            paint::fill(out, ww, track, ink, 0.16);
                        }
                        paint::fill(out, ww, at(PW - 24.0 - 50.0 + if on { 23.0 } else { 3.0 }, mid + 3.0, 24.0, 24.0, 12.0), [1.0; 3], 1.0);
                    }
                }
            }
            y += hgt;
        }
        let lw = rr.w as usize;
        let alpha = win::text_alpha(lw, rr.h as usize, &texts);
        paint::text(out, ww, &alpha, lw, rr.x as usize, rr.y as usize, ink);
        paint::present(p.hwnd, p.dc, p.win);
    }

    // ---------- clics, touches ----------

    /// Clic en (x, y), pixels de la fenêtre du panneau.
    unsafe fn prefs_click(&mut self, wx: f32, wy: f32) {
        let s = self.prefs.s;
        let (x, y) = ((wx - M * s) / s, (wy - M * s) / s);
        if self.prefs.capture {
            // un clic ailleurs annule la saisie du raccourci
            self.prefs.capture = false;
            self.register();
            return self.prefs_draw();
        }
        if (TAB_Y..TAB_Y + 34.0).contains(&y) && (20.0..PW - 20.0).contains(&x) {
            self.prefs.tab = ((x - 20.0) / ((PW - 40.0) / TABS.len() as f32)) as usize;
            self.prefs.note = None;
            return self.prefs_draw();
        }
        let rows = self.prefs_rows(self.prefs.tab);
        let mut top = ROWS_Y;
        for row in &rows {
            let hgt = row_h(row);
            if (top..top + hgt).contains(&y) {
                match row.ctl {
                    Ctl::Seg(opts) if (top + SEG_Y..top + SEG_Y + 34.0).contains(&y) && (20.0..PW - 20.0).contains(&x) => {
                        let k = ((x - 20.0) / ((PW - 40.0) / opts.len() as f32)) as usize;
                        self.prefs_choose(row.key, k.min(opts.len() - 1));
                    }
                    Ctl::Toggle => self.prefs_toggle(row.key),
                    Ctl::Button(_) if x >= PW - 24.0 - BTN_W => return self.prefs_button(row.key),
                    _ => {}
                }
                break;
            }
            top += hgt;
        }
        self.prefs_draw();
    }

    unsafe fn prefs_choose(&mut self, key: Key, k: usize) {
        let c = &mut self.cfg;
        match key {
            Key::Lang => {
                c.french = k == 1;
                settings::set("language", if c.french { "fr" } else { "en" });
            }
            Key::Theme => {
                c.theme = k;
                settings::set("theme", ["glass", "dark", "light"][k]);
            }
            Key::Position => {
                c.center = k == 1;
                settings::set("position", if c.center { "center" } else { "top" });
            }
            Key::Rows => {
                c.rows = settings::ROWS[k];
                settings::set("rows", &c.rows.to_string());
            }
            Key::Engine => {
                c.engine = settings::ENGINES[k].1.into();
                settings::set("engine", settings::ENGINES[k].1);
            }
            _ => {}
        }
    }

    unsafe fn prefs_toggle(&mut self, key: Key) {
        let c = &mut self.cfg;
        let (name, v) = match key {
            Key::Startup => {
                c.autostart = !c.autostart;
                settings::set_startup(c.autostart);
                ("autostart", c.autostart)
            }
            Key::Files => {
                c.files = !c.files;
                ("files", c.files)
            }
            Key::Typos => {
                c.typos = !c.typos;
                ("typos", c.typos)
            }
            Key::Calc => {
                c.calc = !c.calc;
                ("calc", c.calc)
            }
            Key::Web => {
                c.web = !c.web;
                ("web", c.web)
            }
            _ => return,
        };
        settings::set(name, if v { "1" } else { "0" });
    }

    unsafe fn prefs_button(&mut self, key: Key) {
        let fr = self.fr();
        match key {
            Key::Hotkey => {
                // le raccourci actuel est libéré pendant la saisie, sinon Windows le garderait pour lui
                (self.prefs.capture, self.prefs.note) = (true, None);
                UnregisterHotKey(self.hwnd, HOTKEY_ID);
            }
            Key::History => self.history.clear(),
            Key::Folder(i) => {
                let mut list = self.root_list();
                list.remove(i);
                return self.set_roots(list);
            }
            Key::AddFolder => {
                // la boîte de dialogue est modale : le panneau ignore ses messages tant qu'elle est ouverte
                let Some(dir) = pick_folder(self.prefs.hwnd, tr(fr, ("Folder to index", "Dossier à indexer"))) else { return };
                let mut list = self.root_list();
                if !list.iter().any(|r| r.eq_ignore_ascii_case(&dir)) {
                    list.push(dir);
                    return self.set_roots(list);
                }
            }
            Key::Reindex => {
                self.reindex();
                self.index_files();
            }
            Key::EditFile => {
                settings::ensure();
                if let Some(f) = settings::file() {
                    ShellExecuteW(null_mut(), win::w("open").as_ptr(), win::w(&f.to_string_lossy()).as_ptr(), std::ptr::null(),
                        std::ptr::null(), SW_SHOWNORMAL);
                }
                return; // l'éditeur prend la main : le panneau se range
            }
            _ => {}
        }
        self.prefs_draw();
    }

    // shortcut: retirer tous les dossiers revient aux trois de Windows (roots vide) ; un « aucun dossier » si besoin
    unsafe fn set_roots(&mut self, list: Vec<String>) {
        settings::set("roots", &list.join(";"));
        self.cfg.roots = list;
        self.index_files();
        self.prefs_reopen();
    }

    /// Touche dans le panneau : Échap ferme ; pendant la saisie d'un raccourci, la combinaison devient le raccourci.
    unsafe fn prefs_key(&mut self, vk: u16) {
        if !self.prefs.capture {
            if vk == VK_ESCAPE {
                self.prefs_hide();
            }
            return;
        }
        if vk == VK_ESCAPE {
            self.prefs.capture = false;
            self.register();
            return self.prefs_draw();
        }
        // modificateurs seuls : on attend la touche
        if matches!(vk, VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN | VK_LSHIFT | VK_RSHIFT | VK_LCONTROL | VK_RCONTROL | VK_LMENU | VK_RMENU) {
            return;
        }
        let down = |k: VIRTUAL_KEY| GetKeyState(k as i32) < 0;
        let mods = [(VK_CONTROL, MOD_CONTROL), (VK_MENU, MOD_ALT), (VK_SHIFT, MOD_SHIFT), (VK_LWIN, MOD_WIN), (VK_RWIN, MOD_WIN)]
            .iter().filter(|m| down(m.0)).fold(0, |a, m| a | m.1);
        match settings::hotkey_name(mods, vk as u32) {
            None => {
                self.prefs.note = Some(("Use Ctrl, Alt, Shift or Win with a letter, digit, Space or F1–F24",
                    "Ctrl, Alt, Maj ou Win avec une lettre, un chiffre, Espace ou F1–F24"));
            }
            Some(hk) => {
                self.prefs.capture = false;
                self.prefs.note = (!self.try_hotkey(&hk)).then_some(("Already used by another app", "Déjà utilisé par une autre app"));
            }
        }
        self.prefs_draw();
    }
}

/// Barre de choix : un rail discret et la case `k` (sur `n`) mise en avant ; `k` hors limites : aucune.
fn fill_seg(out: &mut [u32], w: usize, rail: Rr, n: usize, k: usize, ink: render::Rgb, s: f32) {
    paint::fill(out, w, rail, ink, 0.08);
    if k < n {
        let cw = rail.w / n as f32;
        let r = Rr { x: rail.x + k as f32 * cw + 3.0 * s, y: rail.y + 3.0 * s, w: cw - 6.0 * s, h: rail.h - 6.0 * s, r: rail.r - 3.0 * s };
        paint::fill(out, w, r, ink, 0.18);
    }
}

pub unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            let (x, y) = ((lp & 0xFFFF) as i16 as f32, ((lp >> 16) & 0xFFFF) as i16 as f32);
            app(|a| a.prefs_click(x, y));
        }
        // Alt+… arrive en WM_SYSKEYDOWN : nécessaire pour saisir Alt+Espace comme raccourci
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            app(|a| a.prefs_key(wp as u16));
        }
        WM_CHAR | WM_SYSCHAR => {} // pas de bip sur Alt+lettre
        WM_SYSCOMMAND if (wp & 0xFFF0) as u32 == SC_KEYMENU => {}
        // clic ailleurs : le panneau se range (pas pendant le choix d'un dossier : l'état est alors emprunté)
        WM_ACTIVATE if wp & 0xFFFF == WA_INACTIVE as usize => {
            app(|a| a.prefs_hide());
        }
        WM_CLOSE => {
            app(|a| a.prefs_hide());
        }
        _ => return DefWindowProcW(h, msg, wp, lp),
    }
    0
}
