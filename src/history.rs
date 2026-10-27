//! Frecency : combien de fois et quand un élément a été lancé, dans `%LOCALAPPDATA%\Lense\history.txt`
//! (`clé\tlancements\tdernier lancement unix`). Réécrit à chaque lancement : c'est rare et le fichier est petit.

use std::{collections::HashMap, path::PathBuf, time::SystemTime};

const HALF_LIFE_DAYS: f64 = 14.0;

#[derive(Default)]
pub struct History {
    map: HashMap<String, (u32, u64)>,
    file: Option<PathBuf>,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl History {
    pub fn load() -> History {
        let file = crate::settings::dir().map(|d| d.join("history.txt"));
        let text = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
        History { map: parse(&text), file }
    }

    /// `ln(1 + lancements) × ½^(jours depuis le dernier / 14)` ; 0 si jamais lancé.
    pub fn frecency(&self, key: &str, now: u64) -> f32 {
        self.map.get(key).map_or(0.0, |&(count, last)| {
            let days = now.saturating_sub(last) as f64 / 86_400.0;
            ((1.0 + count as f64).ln() * 0.5f64.powf(days / HALF_LIFE_DAYS)) as f32
        })
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Oublie tout (panneau Paramètres).
    pub fn clear(&mut self) {
        self.map.clear();
        self.write();
    }

    pub fn bump(&mut self, key: &str, now: u64) {
        let e = self.map.entry(key.to_string()).or_insert((0, now));
        *e = (e.0.saturating_add(1), now);
        self.write();
    }

    fn write(&self) {
        let Some(f) = &self.file else { return };
        let _ = std::fs::create_dir_all(f.parent().unwrap()).and_then(|_| std::fs::write(f, format(&self.map)));
    }
}

fn parse(s: &str) -> HashMap<String, (u32, u64)> {
    s.lines().filter_map(|l| {
        let mut p = l.split('\t');
        let (k, c, t) = (p.next()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?);
        Some((k.to_string(), (c, t)))
    }).collect()
}

fn format(map: &HashMap<String, (u32, u64)>) -> String {
    map.iter().map(|(k, (c, t))| format!("{k}\t{c}\t{t}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_and_round_trip() {
        let now = 1_800_000_000;
        let mut h = History::default(); // sans fichier : rien n'est écrit
        assert_eq!(h.frecency("a", now), 0.0);
        h.bump("a", now - 14 * 86_400);
        let old = h.frecency("a", now);
        assert!((old - 2f32.ln() / 2.0).abs() < 1e-4, "moitié après 14 jours");
        h.bump("a", now);
        assert!(h.frecency("a", now) > old * 2.0, "relancé aujourd'hui");
        assert_eq!(parse(&format(&h.map)), h.map);
        assert!(parse("cassé\nx\t1\n").is_empty());
    }
}
