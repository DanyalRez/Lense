//! Index des apps : raccourcis du menu Démarrer (utilisateur et commun), apps du Store (`shell:AppsFolder`),
//! raccourcis épinglés à la barre des tâches et .exe de `pinned=`. Lancement et dossier d'une app.
//! Les quelques interfaces COM du shell sont appelées directement par leur vtable (windows-sys n'en a pas).

use crate::win::{self, w};
use std::{
    ffi::c_void,
    mem::zeroed,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};
use windows_sys::{
    core::{GUID, HRESULT, PCWSTR, PWSTR},
    Win32::{
        System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER},
        UI::{
            Shell::{Common::{ITEMIDLIST, STRRET}, *},
            WindowsAndMessaging::{DestroyIcon, SW_SHOWNORMAL},
        },
    },
};

pub struct Entry {
    pub name: String,
    pub key: crate::fuzzy::Key,
    /// Chemin du raccourci / de l'exe, ou `shell:AppsFolder\<AUMID>` pour une app du Store ; ShellExecute ouvre les deux.
    pub path: String,
    pub icon: Vec<u32>, // n×n BGRA prémultiplié, vide si pas d'icône
    pub icon_n: usize,
}

impl Entry {
    pub fn store(&self) -> bool {
        self.path.starts_with("shell:")
    }
}

const APPS_FOLDER: &str = "shell:AppsFolder\\";

/// Tout l'index, icônes comprises (`n` px). Lent (~1 s) : à appeler hors du fil de l'interface, COM initialisé.
pub unsafe fn index(pinned: &[String], n: usize) -> Vec<Entry> {
    let env = |k| std::env::var_os(k).map(PathBuf::from);
    let mut found: Vec<(String, String)> = Vec::new();
    for base in [env("APPDATA"), env("PROGRAMDATA")].into_iter().flatten() {
        walk(&base.join(r"Microsoft\Windows\Start Menu\Programs"), &mut found);
    }
    if let Some(a) = env("APPDATA") {
        walk(&a.join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar"), &mut found);
    }
    for p in pinned {
        let path = Path::new(p);
        if let Some(stem) = path.file_stem() {
            found.push((stem.to_string_lossy().into(), p.clone()));
        }
    }
    store_apps(&mut found);
    // même nom : on garde le premier, donc le raccourci (il a un chemin : dossier et admin marchent)
    let mut seen = std::collections::HashSet::new();
    found.retain(|(name, _)| seen.insert(name.to_lowercase()));
    // icônes : ~6 ms chacune, bien plus au premier passage (cache du shell froid). Plusieurs fils n'aident
    // pas : le shell les sérialise et en perd quelques-unes.
    found.into_iter().map(|(name, path)| {
        let icon = icon(&path, n);
        Entry { key: crate::fuzzy::Key::new(&name), name, path, icon_n: if icon.is_empty() { 0 } else { n }, icon }
    }).collect()
}

/// Raccourcis (.lnk, .url : jeux Steam…) d'un dossier et de ses sous-dossiers, sauf les désinstalleurs.
fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            walk(&p, out);
            continue;
        }
        let ext = p.extension().map(|x| x.to_ascii_lowercase());
        if !matches!(ext.as_ref().and_then(|x| x.to_str()), Some("lnk" | "url")) {
            continue;
        }
        let name = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let low = name.to_lowercase();
        if !low.contains("uninstall") && !low.contains("désinstall") {
            out.push((name, p.to_string_lossy().into()));
        }
    }
}

// ---------- COM minimal ----------

type Release = unsafe extern "system" fn(*mut c_void) -> u32;

#[repr(C)]
struct Unknown {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: usize,
    release: Release,
}

#[repr(C)]
struct ShellFolder {
    base: Unknown,
    parse_display_name: usize,
    enum_objects: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut *mut c_void) -> HRESULT,
    bind_to_object: usize,
    bind_to_storage: usize,
    compare_ids: usize,
    create_view_object: usize,
    get_attributes_of: usize,
    get_ui_object_of: usize,
    get_display_name_of: unsafe extern "system" fn(*mut c_void, *const ITEMIDLIST, u32, *mut STRRET) -> HRESULT,
}

#[repr(C)]
struct EnumIdList {
    base: Unknown,
    next: unsafe extern "system" fn(*mut c_void, u32, *mut *mut ITEMIDLIST, *mut u32) -> HRESULT,
}

#[repr(C)]
struct ShellLink {
    base: Unknown,
    get_path: unsafe extern "system" fn(*mut c_void, PWSTR, i32, *mut c_void, u32) -> HRESULT,
}

#[repr(C)]
struct PersistFile {
    base: Unknown,
    get_class_id: usize,
    is_dirty: usize,
    load: unsafe extern "system" fn(*mut c_void, PCWSTR, u32) -> HRESULT,
}

const IID_ISHELLFOLDER: GUID = GUID::from_u128(0x000214e6_0000_0000_c000_000000000046);
const CLSID_SHELLLINK: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
const IID_ISHELLLINKW: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
const IID_IPERSISTFILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);

/// Table de fonctions d'un objet COM.
unsafe fn vt<'a, T>(obj: *mut c_void) -> &'a T {
    &**(obj as *const *const T)
}

/// Nom affiché (`flags` 0) ou nom d'analyse (0x8000) d'un élément d'un dossier du shell.
unsafe fn display(folder: *mut c_void, child: *const ITEMIDLIST, flags: u32) -> Option<String> {
    let mut sr: STRRET = zeroed();
    if (vt::<ShellFolder>(folder).get_display_name_of)(folder, child, flags, &mut sr) < 0 {
        return None;
    }
    let mut p: PWSTR = null_mut();
    if StrRetToStrW(&mut sr, child, &mut p) < 0 || p.is_null() {
        return None;
    }
    let len = (0..).take_while(|&i| *p.add(i) != 0).count();
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
    CoTaskMemFree(p as _);
    Some(s)
}

/// Apps du Store : les éléments de `shell:AppsFolder` dont le nom d'analyse est un AUMID (`Paquet!App`).
/// Les autres éléments du dossier sont des raccourcis déjà trouvés dans le menu Démarrer.
unsafe fn store_apps(out: &mut Vec<(String, String)>) {
    let mut pidl: *mut ITEMIDLIST = null_mut();
    if SHParseDisplayName(w("shell:AppsFolder").as_ptr(), null_mut(), &mut pidl, 0, null_mut()) < 0 {
        return;
    }
    let mut folder: *mut c_void = null_mut();
    let bound = SHBindToObject(null_mut(), pidl, null_mut(), &IID_ISHELLFOLDER, &mut folder) >= 0;
    CoTaskMemFree(pidl as _);
    if !bound || folder.is_null() {
        return;
    }
    let mut en: *mut c_void = null_mut();
    // 0x20 | 0x40 : dossiers et éléments
    if (vt::<ShellFolder>(folder).enum_objects)(folder, null_mut(), 0x60, &mut en) == 0 && !en.is_null() {
        let mut child: *mut ITEMIDLIST = null_mut();
        let mut got = 0;
        while (vt::<EnumIdList>(en).next)(en, 1, &mut child, &mut got) == 0 && got == 1 {
            let (name, id) = (display(folder, child, 0), display(folder, child, 0x8000));
            CoTaskMemFree(child as _);
            if let (Some(name), Some(id)) = (name, id) {
                if id.contains('!') {
                    out.push((name, format!("{APPS_FOLDER}{id}")));
                }
            }
        }
        (vt::<Unknown>(en).release)(en);
    }
    (vt::<Unknown>(folder).release)(folder);
}

/// Cible d'un raccourci .lnk (vide pour un raccourci vers une app du Store ou une URL).
unsafe fn lnk_target(lnk: &str) -> Option<String> {
    let mut link: *mut c_void = null_mut();
    if CoCreateInstance(&CLSID_SHELLLINK, null_mut(), CLSCTX_INPROC_SERVER, &IID_ISHELLLINKW, &mut link) < 0 {
        return None;
    }
    let mut file: *mut c_void = null_mut();
    let mut buf = [0u16; 1024];
    let mut ok = false;
    if (vt::<Unknown>(link).query_interface)(link, &IID_IPERSISTFILE, &mut file) >= 0 {
        ok = (vt::<PersistFile>(file).load)(file, w(lnk).as_ptr(), 0) >= 0
            && (vt::<ShellLink>(link).get_path)(link, buf.as_mut_ptr(), buf.len() as i32, null_mut(), 0) == 0;
        (vt::<Unknown>(file).release)(file);
    }
    (vt::<Unknown>(link).release)(link);
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    (ok && len > 0).then(|| String::from_utf16_lossy(&buf[..len]))
}

// ---------- icônes, lancement ----------

/// Grande icône du shell, rendue en `n`×`n` (vide si aucune).
pub unsafe fn icon(path: &str, n: usize) -> Vec<u32> {
    let mut sfi: SHFILEINFOW = zeroed();
    let size = size_of::<SHFILEINFOW>() as u32;
    if path.starts_with("shell:") {
        let mut pidl: *mut ITEMIDLIST = null_mut();
        if SHParseDisplayName(w(path).as_ptr(), null_mut(), &mut pidl, 0, null_mut()) < 0 {
            return Vec::new();
        }
        SHGetFileInfoW(pidl as PCWSTR, 0, &mut sfi, size, SHGFI_PIDL | SHGFI_ICON | SHGFI_LARGEICON);
        CoTaskMemFree(pidl as _);
    } else {
        SHGetFileInfoW(w(path).as_ptr(), 0, &mut sfi, size, SHGFI_ICON | SHGFI_LARGEICON);
    }
    if sfi.hIcon.is_null() {
        return Vec::new();
    }
    let px = win::icon_pixels(sfi.hIcon, n);
    DestroyIcon(sfi.hIcon);
    px
}

/// Icône générique d'un type de fichier (par extension, sans lire le disque) ou d'un dossier.
pub unsafe fn type_icon(ext: &str, dir: bool, n: usize) -> Vec<u32> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
    let mut sfi: SHFILEINFOW = zeroed();
    let attr = if dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };
    SHGetFileInfoW(w(&format!("x.{ext}")).as_ptr(), attr, &mut sfi, size_of::<SHFILEINFOW>() as u32,
        SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES);
    if sfi.hIcon.is_null() {
        return Vec::new();
    }
    let px = win::icon_pixels(sfi.hIcon, n);
    DestroyIcon(sfi.hIcon);
    px
}

/// Entrée : ouvrir ; Maj : en admin ; Ctrl : son dossier, l'élément sélectionné. Pour une app du Store
/// (`shell:AppsFolder\…`), seul « ouvrir » a un sens.
pub unsafe fn launch(path: &str, parent: bool, admin: bool) {
    let store = path.starts_with("shell:");
    let exec = |verb: &str, file: &str, params: Option<&str>| {
        let p = params.map(w);
        ShellExecuteW(null_mut(), w(verb).as_ptr(), w(file).as_ptr(), p.as_ref().map_or(null(), |p| p.as_ptr()), null(), SW_SHOWNORMAL);
    };
    if parent {
        if store {
            return;
        }
        // le dossier de l'app plutôt que celui du raccourci, quand la cible est un fichier
        let target = if path.to_lowercase().ends_with(".lnk") { lnk_target(path) } else { None };
        exec("open", "explorer.exe", Some(&format!("/select,\"{}\"", target.as_deref().unwrap_or(path))));
    } else if admin {
        if !store {
            exec("runas", path, None);
        }
    } else {
        exec("open", path, None);
    }
}
