//! Index de fichiers des dossiers choisis (`roots=`) : un arbre compact en mémoire (parent + nom dans un blob),
//! enregistré dans `%LOCALAPPDATA%\Lense\files.idx`, tenu à jour par `ReadDirectoryChangesW`.
//! Format de files.idx (petit-boutiste) :
//!   "LENS" | version u16 | nb racines u16 | nb entrées u32 | taille des noms u32
//!   racines : (longueur u16, UTF-16) × nb racines
//!   entrées : parent u32 (NONE = à la racine) | début du nom u32 | longueur u16 | drapeaux u8 (1 = dossier) | racine u8
//!   noms : UTF-8 bout à bout

use crate::fuzzy;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

const NONE: u32 = u32::MAX;
const VERSION: u16 = 1;
/// Au-delà, une rafale de changements (décompression, copie) est traitée par un rescan complet.
const BURST: usize = 200;

#[derive(Clone, Copy, PartialEq, Debug)]
struct Node {
    parent: u32,
    off: u32,
    len: u16,
    dir: bool,
    root: u8,
    dead: bool, // supprimé depuis le chargement ; retiré à la sauvegarde
    mask: u64,  // recalculé au chargement, pas stocké
}

#[derive(Default, PartialEq, Debug)]
pub struct Index {
    pub roots: Vec<String>,
    nodes: Vec<Node>,
    names: String,
}

pub type Shared = Arc<RwLock<Index>>;

/// Racines des réglages : « Desktop », « Documents », « Downloads » désignent les dossiers de Windows (même
/// déplacés dans OneDrive) ; le reste est un chemin. Vide : ces trois-là.
pub fn resolve_roots(roots: &[String]) -> Vec<String> {
    use windows_sys::Win32::{System::Com::CoTaskMemFree, UI::Shell::*};
    let known = |id: &windows_sys::core::GUID| unsafe {
        let mut p = std::ptr::null_mut();
        if SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut p) < 0 {
            return None;
        }
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        CoTaskMemFree(p as _);
        Some(s)
    };
    let default = ["Desktop".to_string(), "Documents".into(), "Downloads".into()];
    let list = if roots.is_empty() { &default[..] } else { roots };
    list.iter().filter_map(|r| match r.to_ascii_lowercase().as_str() {
        "desktop" => known(&FOLDERID_Desktop),
        "documents" => known(&FOLDERID_Documents),
        "downloads" => known(&FOLDERID_Downloads),
        _ => Some(r.trim_end_matches('\\').to_string()),
    }).take(255).collect()
}

fn ignored(name: &str, ignore: &[String]) -> bool {
    ignore.iter().any(|i| i.eq_ignore_ascii_case(name))
}

impl Index {
    pub fn len(&self) -> usize {
        self.nodes.iter().filter(|n| !n.dead).count()
    }

    pub fn name(&self, i: u32) -> &str {
        let n = &self.nodes[i as usize];
        &self.names[n.off as usize..n.off as usize + n.len as usize]
    }

    pub fn is_dir(&self, i: u32) -> bool {
        self.nodes[i as usize].dir
    }

    /// Chemin complet.
    pub fn path(&self, i: u32) -> String {
        let mut parts = vec![self.name(i)];
        let mut p = self.nodes[i as usize].parent;
        while p != NONE {
            parts.push(self.name(p));
            p = self.nodes[p as usize].parent;
        }
        let mut s = self.roots[self.nodes[i as usize].root as usize].clone();
        for part in parts.iter().rev() {
            s.push('\\');
            s.push_str(part);
        }
        s
    }

    fn push(&mut self, parent: u32, name: &str, dir: bool, root: u8) -> u32 {
        let len = name.len().min(u16::MAX as usize) as u16;
        self.nodes.push(Node { parent, off: self.names.len() as u32, len, dir, root, dead: false, mask: fuzzy::mask(name) });
        self.names.push_str(&name[..len as usize]);
        (self.nodes.len() - 1) as u32
    }

    /// Ajoute le contenu de `dir` (récursivement), sauf les dossiers ignorés et les liens (pas de boucle).
    fn scan_dir(&mut self, dir: &Path, parent: u32, root: u8, ignore: &[String]) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let Ok(t) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if t.is_symlink() || ignored(&name, ignore) {
                continue;
            }
            let i = self.push(parent, &name, t.is_dir(), root);
            if t.is_dir() {
                self.scan_dir(&e.path(), i, root, ignore);
            }
        }
    }

    /// Index complet de toutes les racines (lent : en tâche de fond).
    pub fn scan(roots: Vec<String>, ignore: &[String]) -> Index {
        let mut idx = Index { roots, ..Default::default() };
        for r in 0..idx.roots.len() {
            let dir = PathBuf::from(&idx.roots[r]);
            idx.scan_dir(&dir, NONE, r as u8, ignore);
        }
        idx
    }

    fn child(&self, parent: u32, root: u8, name: &str) -> Option<u32> {
        self.nodes.iter().position(|n| !n.dead && n.parent == parent && n.root == root
            && self.names[n.off as usize..n.off as usize + n.len as usize].eq_ignore_ascii_case(name)).map(|i| i as u32)
    }

    /// Nœud d'un chemin relatif à la racine (`a\b\c`) ; `Some(NONE)` pour la racine elle-même.
    // shortcut: recherche linéaire (~1 ms sur 100 k entrées) ; une table (parent, nom) si les rafales gênent
    fn find(&self, root: u8, rel: &str) -> Option<u32> {
        rel.split('\\').filter(|c| !c.is_empty()).try_fold(NONE, |p, c| self.child(p, root, c))
    }

    /// Marque `i` et tout ce qu'il contient comme supprimés. Un enfant est toujours après son parent.
    fn remove(&mut self, i: u32) {
        self.nodes[i as usize].dead = true;
        for j in i as usize + 1..self.nodes.len() {
            let p = self.nodes[j].parent;
            if p != NONE && self.nodes[p as usize].dead {
                self.nodes[j].dead = true;
            }
        }
    }

    /// Un changement signalé par Windows. `action` : 1 ajout, 2 suppression, 4 ancien nom, 5 nouveau nom.
    pub fn apply(&mut self, root: u8, action: u32, rel: &str, ignore: &[String]) {
        if rel.split('\\').any(|c| ignored(c, ignore)) {
            return;
        }
        match action {
            1 | 5 => {
                if self.find(root, rel).is_some() {
                    return;
                }
                let (dir_rel, name) = rel.rsplit_once('\\').unwrap_or(("", rel));
                let Some(parent) = self.find(root, dir_rel) else { return };
                let full = Path::new(&self.roots[root as usize]).join(rel);
                let Ok(meta) = std::fs::symlink_metadata(&full) else { return };
                if meta.file_type().is_symlink() {
                    return;
                }
                let i = self.push(parent, name, meta.is_dir(), root);
                if meta.is_dir() {
                    self.scan_dir(&full, i, root, ignore); // dossier déplacé ici avec son contenu
                }
            }
            2 | 4 => {
                if let Some(i) = self.find(root, rel).filter(|&i| i != NONE) {
                    self.remove(i);
                }
            }
            _ => {}
        }
    }

    /// Les `limit` meilleurs fichiers pour la requête : (note, nœud).
    pub fn search(&self, q: &fuzzy::Key, limit: usize) -> Vec<(f32, u32)> {
        let mut best: Vec<(f32, u32)> = Vec::with_capacity(limit + 1);
        for (i, n) in self.nodes.iter().enumerate() {
            let name = &self.names[n.off as usize..n.off as usize + n.len as usize];
            if n.dead || !q.maybe_in(name, n.mask) {
                continue;
            }
            let Some(s) = fuzzy::score(q, &fuzzy::Key::new(name), false) else { continue };
            if best.len() < limit || s > best[best.len() - 1].0 {
                let at = best.partition_point(|b| b.0 >= s);
                best.insert(at, (s, i as u32));
                best.truncate(limit);
            }
        }
        best
    }

    // ---------- files.idx ----------

    pub fn to_bytes(&self) -> Vec<u8> {
        // seuls les vivants, renumérotés (leurs parents sont avant eux, donc déjà renumérotés)
        let mut map = vec![NONE; self.nodes.len()];
        let (mut nodes, mut names) = (Vec::new(), String::new());
        for (i, n) in self.nodes.iter().enumerate() {
            if n.dead {
                continue;
            }
            map[i] = nodes.len() as u32;
            let parent = if n.parent == NONE { NONE } else { map[n.parent as usize] };
            nodes.push((parent, names.len() as u32, n.len, n.dir, n.root));
            names.push_str(&self.names[n.off as usize..n.off as usize + n.len as usize]);
        }
        let mut b = Vec::with_capacity(16 + nodes.len() * 12 + names.len());
        b.extend_from_slice(b"LENS");
        b.extend_from_slice(&VERSION.to_le_bytes());
        b.extend_from_slice(&(self.roots.len() as u16).to_le_bytes());
        b.extend_from_slice(&(nodes.len() as u32).to_le_bytes());
        b.extend_from_slice(&(names.len() as u32).to_le_bytes());
        for r in &self.roots {
            let u: Vec<u16> = r.encode_utf16().collect();
            b.extend_from_slice(&(u.len() as u16).to_le_bytes());
            u.iter().for_each(|c| b.extend_from_slice(&c.to_le_bytes()));
        }
        for (parent, off, len, dir, root) in nodes {
            b.extend_from_slice(&parent.to_le_bytes());
            b.extend_from_slice(&off.to_le_bytes());
            b.extend_from_slice(&len.to_le_bytes());
            b.push(dir as u8);
            b.push(root);
        }
        b.extend_from_slice(names.as_bytes());
        b
    }

    /// Lit files.idx ; `None` s'il est abîmé ou d'une autre version.
    pub fn from_bytes(b: &[u8]) -> Option<Index> {
        let mut at = 0;
        let mut take = |n: usize| {
            let s = b.get(at..at + n)?;
            at += n;
            Some(s)
        };
        let u16_ = |s: &[u8]| u16::from_le_bytes([s[0], s[1]]);
        let u32_ = |s: &[u8]| u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
        if take(4)? != b"LENS" || u16_(take(2)?) != VERSION {
            return None;
        }
        let (nroots, count, names_len) = (u16_(take(2)?) as usize, u32_(take(4)?) as usize, u32_(take(4)?) as usize);
        let mut roots = Vec::with_capacity(nroots);
        for _ in 0..nroots {
            let len = u16_(take(2)?) as usize;
            let units: Vec<u16> = take(len * 2)?.chunks(2).map(u16_).collect();
            roots.push(String::from_utf16(&units).ok()?);
        }
        let raw = take(count.checked_mul(12)?)?;
        let names = String::from_utf8(take(names_len)?.to_vec()).ok()?;
        let mut nodes = Vec::with_capacity(count);
        for (i, e) in raw.chunks(12).enumerate() {
            let (parent, off, len, root) = (u32_(&e[0..4]), u32_(&e[4..8]), u16_(&e[8..10]), e[11]);
            // un parent est toujours avant ; un nom doit tomber dans le blob, entre deux caractères
            let name = names.get(off as usize..off as usize + len as usize)?;
            if (parent != NONE && parent as usize >= i) || root as usize >= nroots {
                return None;
            }
            nodes.push(Node { parent, off, len, dir: e[10] & 1 != 0, root, dead: false, mask: fuzzy::mask(name) });
        }
        Some(Index { roots, nodes, names })
    }
}

pub fn file() -> Option<PathBuf> {
    Some(crate::settings::dir()?.join("files.idx"))
}

/// Index enregistré, s'il couvre exactement ces racines.
pub fn load(roots: &[String]) -> Option<Index> {
    let idx = Index::from_bytes(&std::fs::read(file()?).ok()?)?;
    (idx.roots == roots).then_some(idx)
}

pub fn save(idx: &Index) {
    let Some(f) = file() else { return };
    let tmp = f.with_extension("idx.tmp");
    // écrit à côté puis remplace : un arrêt brutal ne laisse jamais un index à moitié écrit
    let _ = std::fs::create_dir_all(f.parent().unwrap())
        .and_then(|_| std::fs::write(&tmp, idx.to_bytes()))
        .and_then(|_| std::fs::rename(&tmp, &f));
}

/// Surveille une racine (fil bloqué dans ReadDirectoryChangesW, 0 % CPU au repos) et applique les changements.
/// `changed` est appelé après chaque lot ; `rescan` quand Windows a perdu le fil (tampon plein) ou sur rafale.
pub fn watch(idx: Shared, root: u8, ignore: Vec<String>, changed: impl Fn(), rescan: impl Fn()) {
    use windows_sys::Win32::{Foundation::*, Storage::FileSystem::*};
    let Some(path) = idx.read().ok().and_then(|i| i.roots.get(root as usize).cloned()) else { return };
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let h = CreateFileW(wide.as_ptr(), FILE_LIST_DIRECTORY, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, std::ptr::null_mut());
        if h == INVALID_HANDLE_VALUE {
            return;
        }
        let mut buf = vec![0u32; 16 * 1024]; // 64 Ko, aligné sur 4 octets comme l'exige l'API
        loop {
            let mut got = 0;
            let ok = ReadDirectoryChangesW(h, buf.as_mut_ptr() as _, (buf.len() * 4) as u32, 1,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME, &mut got, std::ptr::null_mut(), None);
            if ok == 0 {
                break; // racine supprimée ou démontée
            }
            let bytes = std::slice::from_raw_parts(buf.as_ptr() as *const u8, got as usize);
            let events = parse_events(bytes);
            if got == 0 || events.len() > BURST {
                rescan();
                continue;
            }
            if let Ok(mut i) = idx.write() {
                if i.roots.get(root as usize) != Some(&path) {
                    break; // racines changées dans les réglages : d'autres fils surveillent les nouvelles
                }
                for (action, rel) in &events {
                    i.apply(root, *action, rel, &ignore);
                }
            }
            changed();
        }
        CloseHandle(h);
    }
}

/// Enregistrements FILE_NOTIFY_INFORMATION : (action, chemin relatif).
fn parse_events(b: &[u8]) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(head) = b.get(at..at + 12) {
        let rd = |k: usize| u32::from_le_bytes([head[k], head[k + 1], head[k + 2], head[k + 3]]);
        let (next, action, len) = (rd(0) as usize, rd(4), rd(8) as usize);
        let Some(name) = b.get(at + 12..at + 12 + len) else { break };
        let units: Vec<u16> = name.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        out.push((action, String::from_utf16_lossy(&units)));
        if next == 0 {
            break;
        }
        at += next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Index {
        let mut i = Index { roots: vec![r"C:\r0".into(), r"D:\r1".into()], ..Default::default() };
        let a = i.push(NONE, "projets", true, 0);
        let b = i.push(a, "lens", true, 0);
        i.push(b, "main.rs", false, 0);
        i.push(a, "notes été.txt", false, 0);
        i.push(NONE, "rapport.pdf", false, 1);
        i
    }

    #[test]
    fn paths_find_remove() {
        let mut i = sample();
        assert_eq!(i.path(2), r"C:\r0\projets\lens\main.rs");
        assert_eq!(i.path(4), r"D:\r1\rapport.pdf");
        assert_eq!(i.find(0, r"PROJETS\Lens\main.rs"), Some(2), "insensible à la casse");
        assert_eq!(i.find(1, r"projets"), None, "autre racine");
        i.remove(1);
        assert!(i.nodes[1].dead && i.nodes[2].dead && !i.nodes[3].dead, "le contenu part avec le dossier");
        assert_eq!(i.len(), 3);
    }

    #[test]
    fn idx_round_trip_drops_dead_nodes() {
        let mut i = sample();
        assert_eq!(Index::from_bytes(&i.to_bytes()), Some(sample()));
        i.remove(1);
        let back = Index::from_bytes(&i.to_bytes()).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back.path(1), r"C:\r0\projets\notes été.txt", "parents renumérotés");
        // abîmé : tronqué, mauvaise signature
        let b = i.to_bytes();
        assert_eq!(Index::from_bytes(&b[..b.len() - 3]), None);
        assert_eq!(Index::from_bytes(b"NOPE"), None);
    }

    #[test]
    fn search_ranks_and_limits() {
        let i = sample();
        let q = fuzzy::Key::new("note ete");
        assert_eq!(i.search(&q, 8).first().map(|r| r.1), Some(3), "accents");
        assert!(i.search(&fuzzy::Key::new("zzz"), 8).is_empty());
        assert_eq!(i.search(&fuzzy::Key::new("r"), 2).len(), 2, "limite");
    }

    #[test]
    fn events_are_parsed() {
        let rec = |last: bool, action: u32, name: &str| {
            let units: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            let size = (12 + units.len()).next_multiple_of(4);
            let mut r = Vec::new();
            r.extend_from_slice(&(if last { 0 } else { size as u32 }).to_le_bytes());
            r.extend_from_slice(&action.to_le_bytes());
            r.extend_from_slice(&(units.len() as u32).to_le_bytes());
            r.extend_from_slice(&units);
            r.resize(size, 0);
            r
        };
        let b = [rec(false, 1, "a\\b"), rec(true, 2, "c")].concat();
        assert_eq!(parse_events(&b), [(1, "a\\b".to_string()), (2, "c".to_string())]);
    }
}
