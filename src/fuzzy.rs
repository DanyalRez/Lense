//! Correspondance floue : préfiltre par masque de caractères, sous-séquence notée façon fzy (débuts de mots,
//! lettres consécutives, écarts), et repli tolérant aux fautes de frappe (Damerau-Levenshtein sur les mots).
//! Insensible à la casse et aux accents.

/// Texte préparé une fois (nom d'app, plus tard de fichier) pour être comparé à chaque frappe.
pub struct Key {
    chars: Vec<char>, // minuscules sans accents
    bonus: Vec<f32>,  // bonus d'une correspondance à cette place (début de mot…)
    mask: u64,
}

// barème de fzy
const GAP_LEADING: f32 = -0.005;
const GAP_INNER: f32 = -0.01;
const CONSECUTIVE: f32 = 1.0;
const AFTER_SEP: f32 = 0.9; // après espace, -, _, /, \
const AFTER_DOT: f32 = 0.6;
const CAPITAL: f32 = 0.7; // CamelCase
const PREFIX: f32 = 2.0; // la requête est le début du nom
const TYPO: f32 = -2.0; // une faute classe toujours après les vraies correspondances

/// Minuscule sans accent (lettres latines courantes).
fn fold(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'œ' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        _ => c,
    }
}

fn bit(c: char) -> u64 {
    match c {
        'a'..='z' => 1 << (c as u32 - 'a' as u32),
        '0'..='9' => 1 << (26 + c as u32 - '0' as u32),
        _ => 1 << (36 + c as u32 % 28),
    }
}

impl Key {
    pub fn new(s: &str) -> Key {
        let orig: Vec<char> = s.chars().collect();
        let chars: Vec<char> = orig.iter().map(|&c| fold(c)).collect();
        let bonus = (0..orig.len()).map(|j| {
            let Some(&p) = j.checked_sub(1).and_then(|k| orig.get(k)) else { return AFTER_SEP };
            match p {
                ' ' | '-' | '_' | '/' | '\\' => AFTER_SEP,
                '.' => AFTER_DOT,
                _ if p.is_lowercase() && orig[j].is_uppercase() => CAPITAL,
                _ => 0.0,
            }
        }).collect();
        let mask = chars.iter().fold(0, |m, &c| m | bit(c));
        Key { chars, bonus, mask }
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    /// Préfiltre sans allocation (index de fichiers) : `name`, de masque `mask`, contient-il les lettres de la
    /// requête, dans l'ordre ? Seuls les noms qui passent sont préparés et notés.
    pub fn maybe_in(&self, name: &str, mask: u64) -> bool {
        if self.mask & !mask != 0 {
            return false;
        }
        let mut q = self.chars.iter().peekable();
        for c in name.chars() {
            if q.peek() == Some(&&fold(c)) {
                q.next();
            }
        }
        q.peek().is_none()
    }
}

/// Masque des caractères d'un nom (à garder à côté du nom pour `Key::maybe_in`).
pub fn mask(s: &str) -> u64 {
    s.chars().fold(0, |m, c| m | bit(fold(c)))
}

/// Note de `t` pour la requête `q` (plus haut = mieux), `None` si ça ne correspond pas.
/// `typos` : essayer aussi les fautes de frappe (réservé aux petites listes, c'est plus coûteux).
pub fn score(q: &Key, t: &Key, typos: bool) -> Option<f32> {
    let (m, n) = (q.chars.len(), t.chars.len());
    if m == 0 || m > n + 2 {
        return None;
    }
    if q.mask & !t.mask == 0 && m <= n {
        if let Some(s) = subsequence(q, t) {
            let prefix = t.chars.starts_with(&q.chars);
            return Some(s + if prefix { PREFIX } else { 0.0 } + if m == n && prefix { PREFIX } else { 0.0 });
        }
    }
    if !typos || m < 4 {
        return None;
    }
    let allowed = if m >= 8 { 2 } else { 1 };
    let d = typo_distance(&q.chars, t)?;
    (d <= allowed).then_some(TYPO - d as f32 + 0.1 * m as f32)
}

/// fzy : meilleur placement de la requête comme sous-séquence. `d` : la lettre i finit sur j ; `best` : le
/// meilleur pour les i premières lettres placées avant ou en j. Deux lignes suffisent.
fn subsequence(q: &Key, t: &Key) -> Option<f32> {
    let n = t.chars.len();
    let mut prev_d = vec![f32::NEG_INFINITY; n];
    let mut prev_best = vec![f32::NEG_INFINITY; n];
    let (mut d, mut best) = (vec![0.0; n], vec![0.0; n]);
    for (i, &qc) in q.chars.iter().enumerate() {
        let mut run = f32::NEG_INFINITY;
        for j in 0..n {
            d[j] = if j < i || t.chars[j] != qc {
                f32::NEG_INFINITY
            } else if i == 0 {
                j as f32 * GAP_LEADING + t.bonus[j]
            } else if j == 0 {
                f32::NEG_INFINITY
            } else {
                (prev_best[j - 1] + t.bonus[j]).max(prev_d[j - 1] + CONSECUTIVE)
            };
            run = d[j].max(run + GAP_INNER);
            best[j] = run;
        }
        std::mem::swap(&mut d, &mut prev_d);
        std::mem::swap(&mut best, &mut prev_best);
    }
    let s = prev_best[n - 1];
    s.is_finite().then_some(s)
}

/// Plus petite distance entre la requête et le début d'un des mots de `t` (longueur ±1, pour les lettres
/// oubliées ou en trop).
fn typo_distance(q: &[char], t: &Key) -> Option<usize> {
    let m = q.len();
    (0..t.chars.len()).filter(|&j| t.bonus[j] >= AFTER_SEP || t.bonus[j] == CAPITAL).flat_map(|j| {
        (m.saturating_sub(1)..=m + 1).filter_map(move |len| t.chars.get(j..j + len)).map(|w| osa(q, w))
    }).min()
}

/// Damerau-Levenshtein restreint (une inversion de deux lettres = une faute).
fn osa(a: &[char], b: &[char]) -> usize {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0; m + 1]; n + 1];
    (0..=n).for_each(|i| d[i][0] = i);
    (0..=m).for_each(|j| d[0][j] = j);
    for i in 1..=n {
        for j in 1..=m {
            let cost = (a[i - 1] != b[j - 1]) as usize;
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m]
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 11] = ["Visual Studio Code", "Visual Studio 2022", "Google Chrome", "Paint", "PowerShell",
        "Photoshop", "Spotify", "Steam", "Calculatrice", "Paramètres", "Explorateur de fichiers"];

    fn best(q: &str) -> Option<&'static str> {
        let q = Key::new(q);
        NAMES.iter().filter_map(|n| Some((score(&q, &Key::new(n), true)?, *n))).max_by(|a, b| a.0.total_cmp(&b.0)).map(|b| b.1)
    }

    #[test]
    fn ranking() {
        assert_eq!(best("vsc"), Some("Visual Studio Code"));
        assert_eq!(best("vs 20"), Some("Visual Studio 2022"));
        assert_eq!(best("chrme"), Some("Google Chrome"), "lettre oubliée");
        assert_eq!(best("crhome"), Some("Google Chrome"), "lettres inversées");
        assert_eq!(best("spotfiy"), Some("Spotify"));
        assert_eq!(best("parametres"), Some("Paramètres"), "accents");
        assert_eq!(best("PARAM"), Some("Paramètres"), "casse");
        assert_eq!(best("st"), Some("Steam"), "début du nom avant le milieu");
        assert_eq!(best("ps"), Some("PowerShell"), "CamelCase");
        assert_eq!(best("explo fich"), Some("Explorateur de fichiers"));
        assert_eq!(best("xyz"), None);
        assert_eq!(best("pnt"), Some("Paint"));
    }

    #[test]
    fn typos_need_enough_letters() {
        let t = Key::new("Steam");
        assert!(score(&Key::new("stem"), &t, true).is_some(), "sous-séquence");
        assert!(score(&Key::new("staem"), &t, true).is_some(), "1 faute à 5 lettres");
        assert!(score(&Key::new("sae"), &t, true).is_none(), "pas de faute sous 4 lettres");
        assert!(score(&Key::new("staem"), &t, false).is_none(), "fautes désactivées");
        assert!(score(&Key::new("sxyzm"), &t, true).is_none(), "trop de fautes");
        // une vraie correspondance passe toujours devant une faute
        let real = score(&Key::new("paint"), &Key::new("Paint"), true).unwrap();
        let typo = score(&Key::new("piant"), &Key::new("Paint"), true).unwrap();
        assert!(real > typo);
    }
}
