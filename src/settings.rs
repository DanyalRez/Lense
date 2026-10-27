//! Réglages : `%LOCALAPPDATA%\Lense\settings.txt` (clé=valeur), changés depuis le panneau Paramètres ou à la main ;
//! les lignes inconnues sont gardées telles quelles. Lancement avec Windows.

use crate::win::w;
use std::path::PathBuf;
use windows_sys::Win32::{
    System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ},
    UI::Input::KeyboardAndMouse::*,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const DEFAULT_HOTKEY: &str = "Alt+Space";
pub const FALLBACK_HOTKEY: &str = "Ctrl+Alt+Space";
pub const DEFAULT_IGNORE: &str = ".git;node_modules;target;.venv;__pycache__";
/// Moteurs proposés dans le panneau ; `engine=` accepte aussi n'importe quelle adresse http(s) avec `%s`.
pub const ENGINES: [(&str, &str); 4] = [
    ("Google", "https://www.google.com/search?q=%s"),
    ("DuckDuckGo", "https://duckduckgo.com/?q=%s"),
    ("Bing", "https://www.bing.com/search?q=%s"),
    ("Ecosia", "https://www.ecosia.org/search?q=%s"),
];
pub const ROWS: [usize; 3] = [5, 8, 10];

/// Adresse de la recherche web : `%s` remplacé par la requête encodée (ajoutée à la fin s'il n'y en a pas).
pub fn web_url(engine: &str, q: &str) -> String {
    let enc: String = q.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect();
    if engine.contains("%s") { engine.replace("%s", &enc) } else { format!("{engine}{enc}") }
}

/// Nom affiché du moteur : celui de la liste, sinon tiré de l'adresse (« https://www.qwant.com/… » → « Qwant »).
pub fn engine_name(engine: &str) -> String {
    if let Some((name, _)) = ENGINES.iter().find(|e| e.1 == engine) {
        return name.to_string();
    }
    let host = engine.split("://").nth(1).unwrap_or(engine).split('/').next().unwrap_or("");
    let host = host.strip_prefix("www.").unwrap_or(host);
    let name = host.split('.').next().unwrap_or(host);
    let mut c = name.chars();
    c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}

#[derive(Clone, PartialEq, Debug)]
pub struct Config {
    pub hotkey: String,
    pub autostart: bool,
    pub french: bool,  // anglais par défaut
    pub theme: usize,  // 0 verre, 1 sombre, 2 clair
    pub center: bool,  // fenêtre au milieu de l'écran plutôt qu'en haut
    pub rows: usize,   // résultats affichés (une des valeurs de ROWS)
    pub files: bool,   // fichiers dans les résultats
    pub typos: bool,   // tolérance aux fautes de frappe (apps)
    pub calc: bool,    // calcul et conversions
    pub web: bool,     // ligne de recherche web
    pub pinned: Vec<String>, // .exe ajoutés à la main aux apps
    pub roots: Vec<String>,  // dossiers indexés : « Desktop », « Documents », « Downloads » ou chemins ; vide = ces trois
    pub ignore: Vec<String>, // noms de dossiers jamais indexés
    pub engine: String,      // recherche web, `%s` = la requête
}

impl Default for Config {
    fn default() -> Self {
        // démarrage avec Windows : seulement si on le demande
        Config { hotkey: DEFAULT_HOTKEY.into(), autostart: false, french: false, theme: 0, center: false, rows: 8,
            files: true, typos: true, calc: true, web: true, pinned: Vec::new(), roots: Vec::new(),
            ignore: DEFAULT_IGNORE.split(';').map(String::from).collect(), engine: ENGINES[0].1.into() }
    }
}

pub fn parse(s: &str) -> Config {
    let mut c = Config::default();
    for line in s.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        let on = v != "0";
        match k {
            "hotkey" if hotkey(v).is_some() => c.hotkey = v.into(),
            "autostart" => c.autostart = on,
            "language" => c.french = v == "fr",
            "theme" => c.theme = ["glass", "dark", "light"].iter().position(|t| *t == v).unwrap_or(0),
            "position" => c.center = v == "center",
            "rows" => c.rows = v.parse().ok().filter(|n| ROWS.contains(n)).unwrap_or(8),
            "files" => c.files = on,
            "typos" => c.typos = on,
            "calc" => c.calc = on,
            "web" => c.web = on,
            "pinned" => c.pinned = paths(v),
            "roots" => c.roots = paths(v),
            "engine" if v.starts_with("https://") || v.starts_with("http://") => c.engine = v.into(),
            "ignore" => c.ignore = v.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
            _ => {}
        }
    }
    c
}

/// « %USERPROFILE%\x ; C:\y » → chemins, variables d'environnement remplacées.
fn paths(v: &str) -> Vec<String> {
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
    v.split(';').map(str::trim).filter(|p| !p.is_empty()).map(|p| unsafe {
        let mut buf = [0u16; 1024];
        let n = ExpandEnvironmentStringsW(w(p).as_ptr(), buf.as_mut_ptr(), buf.len() as u32) as usize;
        if n == 0 || n > buf.len() { p.to_string() } else { String::from_utf16_lossy(&buf[..n - 1]) }
    }).collect()
}

/// « Ctrl+Alt+Space » → (modificateurs, touche virtuelle) pour RegisterHotKey.
pub fn hotkey(s: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = 0;
    let mut key = None;
    for part in s.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" | "maj" => mods |= MOD_SHIFT,
            "win" => mods |= MOD_WIN,
            "space" | "espace" => key = Some(VK_SPACE as u32),
            k if k.len() == 1 && k.as_bytes()[0].is_ascii_alphanumeric() => key = Some(k.as_bytes()[0].to_ascii_uppercase() as u32),
            k if k.starts_with('f') => match k[1..].parse::<u32>() {
                Ok(n @ 1..=24) => key = Some(VK_F1 as u32 + n - 1),
                _ => return None,
            },
            _ => return None,
        }
    }
    let key = key?;
    // une touche ordinaire sans modificateur serait volée à tout Windows ; seules les touches F passent
    if mods == 0 && key < VK_F1 as u32 {
        return None;
    }
    Some((mods, key))
}

/// L'inverse de `hotkey`, pour une combinaison tapée dans le panneau ; `None` si la touche n'est pas gérée.
pub fn hotkey_name(mods: HOT_KEY_MODIFIERS, vk: u32) -> Option<String> {
    let key = match vk {
        v if v == VK_SPACE as u32 => "Space".to_string(),
        v if (VK_F1 as u32..=VK_F24 as u32).contains(&v) => format!("F{}", v - VK_F1 as u32 + 1),
        v if (b'A' as u32..=b'Z' as u32).contains(&v) || (b'0' as u32..=b'9' as u32).contains(&v) => (v as u8 as char).to_string(),
        _ => return None,
    };
    let names = [(MOD_CONTROL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift"), (MOD_WIN, "Win")];
    let mut parts: Vec<String> = names.iter().filter(|m| mods & m.0 != 0).map(|m| m.1.to_string()).collect();
    parts.push(key);
    let s = parts.join("+");
    hotkey(&s).map(|_| s)
}

/// `%LOCALAPPDATA%\Lense` : réglages, historique, index de fichiers.
pub fn dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Lense"))
}

/// L'app s'appelait Lens : on reprend son dossier (réglages, historique, index) une fois.
pub fn migrate() {
    let (Some(new), Some(local)) = (dir(), std::env::var_os("LOCALAPPDATA")) else { return };
    let old = PathBuf::from(local).join("Lens");
    if !new.exists() && old.join("settings.txt").exists() {
        let _ = std::fs::rename(old, new);
    }
}

pub fn file() -> Option<PathBuf> {
    Some(dir()?.join("settings.txt"))
}

pub fn load() -> Config {
    parse(&file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default())
}

/// Écrit le fichier par défaut s'il n'existe pas.
pub fn ensure() {
    let Some(f) = file() else { return };
    if !f.exists() {
        let s = format!("# Lense — also editable from Settings; reloaded each time Lense opens\nhotkey={DEFAULT_HOTKEY}\n\
            autostart=0\nlanguage=en\ntheme=glass\nposition=top\nrows=8\nfiles=1\ntypos=1\ncalc=1\nweb=1\n\
            # indexed folders, separated by ; (Desktop, Documents, Downloads or a path)\nroots=Desktop;Documents;Downloads\n\
            ignore={DEFAULT_IGNORE}\n# .exe files to add to the apps\npinned=\n# web search, %s = the query\nengine={}\n",
            ENGINES[0].1);
        let _ = std::fs::create_dir_all(f.parent().unwrap()).and_then(|_| std::fs::write(f, s));
    }
}

/// Change une clé sans toucher aux autres lignes (le fichier peut aussi être édité à la main).
pub fn set(key: &str, value: &str) {
    let Some(f) = file() else { return };
    ensure();
    let old = std::fs::read_to_string(&f).unwrap_or_default();
    let line = format!("{key}={value}");
    let mut found = false;
    let mut out: Vec<&str> = old.lines().map(|l| match l.split_once('=') {
        Some((k, _)) if k.trim() == key => {
            found = true;
            line.as_str()
        }
        _ => l,
    }).collect();
    if !found {
        out.push(&line);
    }
    let _ = std::fs::write(f, out.join("\n") + "\n");
}

/// Lancement à l'ouverture de session ; réécrit à chaque démarrage pour suivre l'exe s'il a bougé.
pub unsafe fn set_startup(on: bool) {
    // l'entrée de l'ancien nom (Lens) part dans tous les cas
    RegDeleteKeyValueW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), w("Lens").as_ptr());
    if !on {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), w("Lense").as_ptr());
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let cmd = w(&format!("\"{}\"", exe.display()));
    RegSetKeyValueW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), w("Lense").as_ptr(), REG_SZ, cmd.as_ptr() as _, (cmd.len() * 2) as u32);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys() {
        assert_eq!(hotkey("Alt+Space"), Some((MOD_ALT, VK_SPACE as u32)));
        assert_eq!(hotkey("ctrl + alt + espace"), Some((MOD_CONTROL | MOD_ALT, VK_SPACE as u32)));
        assert_eq!(hotkey("Win+L"), Some((MOD_WIN, 'L' as u32)));
        assert_eq!(hotkey("F13"), Some((0, VK_F13 as u32)));
        assert_eq!(hotkey("Space"), None, "touche seule interdite");
        assert_eq!(hotkey("Alt+Banane"), None);
        assert_eq!(hotkey("Alt"), None, "pas de touche");
        // aller-retour depuis une combinaison tapée
        assert_eq!(hotkey_name(MOD_CONTROL | MOD_SHIFT, 'K' as u32).as_deref(), Some("Ctrl+Shift+K"));
        assert_eq!(hotkey_name(MOD_ALT, VK_SPACE as u32).as_deref(), Some("Alt+Space"));
        assert_eq!(hotkey_name(0, VK_F9 as u32).as_deref(), Some("F9"));
        assert_eq!(hotkey_name(0, 'K' as u32), None, "lettre seule");
        assert_eq!(hotkey_name(MOD_ALT, VK_TAB as u32), None, "touche non gérée");
    }

    #[test]
    fn parse_keeps_defaults_on_bad_values() {
        let c = parse("hotkey=Ctrl+Alt+K\nautostart=0\nroots=x\n");
        assert_eq!((c.hotkey.as_str(), c.autostart, c.pinned.len()), ("Ctrl+Alt+K", false, 0));
        assert_eq!(c.ignore, [".git", "node_modules", "target", ".venv", "__pycache__"], "défaut");
        assert_eq!(parse("ignore= a ;b;").ignore, ["a", "b"]);
        assert_eq!(c.engine, ENGINES[0].1);
        assert!(!c.french && c.theme == 0 && !c.center && c.rows == 8 && c.files && c.typos && c.calc && c.web, "défauts");
        let d = parse("language=fr\ntheme=light\nposition=center\nrows=10\nfiles=0\ntypos=0\ncalc=0\nweb=0\n");
        assert!(d.french && d.theme == 2 && d.center && d.rows == 10 && !d.files && !d.typos && !d.calc && !d.web);
        assert_eq!(parse("rows=42\ntheme=rose").rows, 8);
        assert_eq!(parse("engine=javascript:alert(1)").engine, ENGINES[0].1, "http(s) seulement");
        assert_eq!(parse("engine=https://duckduckgo.com/?q=%s").engine, "https://duckduckgo.com/?q=%s");
    }

    #[test]
    fn web() {
        assert_eq!(web_url(ENGINES[0].1, "café & co"), "https://www.google.com/search?q=caf%C3%A9%20%26%20co");
        assert_eq!(web_url("https://x.org/s?q=", "a+b"), "https://x.org/s?q=a%2Bb", "sans %s : à la fin");
        assert_eq!(engine_name(ENGINES[1].1), "DuckDuckGo");
        assert_eq!(engine_name("https://www.qwant.com/?q=%s"), "Qwant");
        assert_eq!(parse("hotkey=nimporte quoi").hotkey, DEFAULT_HOTKEY);
        let windir = std::env::var("WINDIR").unwrap();
        assert_eq!(parse("pinned= %WINDIR%\\a.exe ;; C:\\b.exe ;").pinned, [format!("{windir}\\a.exe"), "C:\\b.exe".into()]);
    }
}
