//! Calculatrice et conversions d'unités dans le champ de recherche : « 12*4+3 », « (3,5+1)/2 », « 2^10 »,
//! « sqrt 2 », « 10 km en mi », « -40 c to f », « 1 Go en Mo », « 72 f » (vers l'unité habituelle).

/// Résultat à afficher (`text`) et à copier (`copy`, le nombre seul).
#[derive(Debug, PartialEq)]
pub struct Answer {
    pub text: String,
    pub copy: String,
    pub conversion: bool,
}

pub fn eval(q: &str) -> Option<Answer> {
    let q = q.trim();
    let comma = q.contains(',');
    if let Some((v, from, to)) = convert(q) {
        let (n, f) = (fmt(v, comma), fmt(number(q)?, comma));
        return Some(Answer { text: format!("{f} {} = {n} {}", from.name, to.name), copy: n, conversion: true });
    }
    // un nombre seul n'est pas un calcul ; il faut un opérateur ou une fonction
    if !q.contains(['+', '-', '*', '/', '^', '%', '(', '×', '÷']) && !q.to_lowercase().contains("sqrt") && !q.eq_ignore_ascii_case("pi") {
        return None;
    }
    let mut p = Parser { s: q.chars().collect(), at: 0 };
    let v = p.expr()?;
    p.space();
    if p.at != p.s.len() || !v.is_finite() {
        return None;
    }
    let n = fmt(v, comma);
    Some(Answer { text: format!("= {n}"), copy: n, conversion: false })
}

/// Au plus 10 chiffres significatifs, sans zéros inutiles ; virgule si la requête en avait une.
fn fmt(v: f64, comma: bool) -> String {
    let a = v.abs();
    let s = if a == 0.0 {
        "0".into()
    } else if !(1e-6..1e15).contains(&a) {
        format!("{v:e}")
    } else {
        let decimals = (9 - a.log10().floor() as i32).clamp(0, 15) as usize;
        let s = format!("{v:.decimals$}");
        let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
        if s == "-0" { "0".into() } else { s }
    };
    if comma { s.replace('.', ",") } else { s }
}

// ---------- expressions ----------

struct Parser {
    s: Vec<char>,
    at: usize,
}

impl Parser {
    fn space(&mut self) {
        while self.s.get(self.at).is_some_and(|c| c.is_whitespace()) {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.space();
        let ok = self.s.get(self.at) == Some(&c);
        self.at += ok as usize;
        ok
    }

    fn word(&mut self, w: &str) -> bool {
        self.space();
        let n = w.chars().count();
        let ok = self.s.get(self.at..self.at + n).is_some_and(|s| s.iter().collect::<String>().eq_ignore_ascii_case(w));
        self.at += if ok { n } else { 0 };
        ok
    }

    // expr = terme (('+' | '-') terme)*
    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            if self.eat('+') {
                v += self.term()?;
            } else if self.eat('-') {
                v -= self.term()?;
            } else {
                return Some(v);
            }
        }
    }

    // terme = unaire (('*' | '/' | '%') unaire)*
    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        loop {
            if self.eat('*') || self.eat('×') {
                v *= self.unary()?;
            } else if self.eat('/') || self.eat('÷') {
                let d = self.unary()?;
                if d == 0.0 {
                    return None;
                }
                v /= d;
            } else if self.eat('%') {
                let d = self.unary()?;
                if d == 0.0 {
                    return None;
                }
                v %= d;
            } else {
                return Some(v);
            }
        }
    }

    // unaire = ('-' | '+') unaire | puissance ; -2^2 = -(2^2)
    fn unary(&mut self) -> Option<f64> {
        if self.eat('-') {
            return Some(-self.unary()?);
        }
        if self.eat('+') {
            return self.unary();
        }
        self.power()
    }

    // puissance = atome ('^' unaire)? ; à droite d'abord : 2^3^2 = 2^9
    fn power(&mut self) -> Option<f64> {
        let base = self.atom()?;
        if self.eat('^') {
            return Some(base.powf(self.unary()?));
        }
        Some(base)
    }

    fn atom(&mut self) -> Option<f64> {
        if self.eat('(') {
            let v = self.expr()?;
            return self.eat(')').then_some(v);
        }
        if self.word("sqrt") {
            let v = self.power()?;
            return (v >= 0.0).then(|| v.sqrt());
        }
        if self.word("pi") {
            return Some(std::f64::consts::PI);
        }
        self.space();
        let start = self.at;
        while self.s.get(self.at).is_some_and(|c| c.is_ascii_digit() || *c == '.' || *c == ',') {
            self.at += 1;
        }
        self.s[start..self.at].iter().collect::<String>().replace(',', ".").parse().ok()
    }
}

// ---------- unités ----------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Length,
    Mass,
    Temp,
    Data,
}

#[derive(Clone, Copy, Debug)]
struct Unit {
    name: &'static str,
    aliases: &'static [&'static str],
    kind: Kind,
    factor: f64,           // vers l'unité de base (m, kg, octet) ; pour les températures, voir `to_kelvin`
    default: &'static str, // conversion proposée quand on n'en demande pas
}

const fn u(name: &'static str, aliases: &'static [&'static str], kind: Kind, factor: f64, default: &'static str) -> Unit {
    Unit { name, aliases, kind, factor, default }
}

const KI: f64 = 1024.0;
const UNITS: [Unit; 24] = [
    u("km", &["kilometre", "kilomètre", "kilometres", "kilomètres"], Kind::Length, 1000.0, "mi"),
    u("m", &["metre", "mètre", "metres", "mètres"], Kind::Length, 1.0, "ft"),
    u("cm", &[], Kind::Length, 0.01, "in"),
    u("mm", &[], Kind::Length, 0.001, "in"),
    u("mi", &["mile", "miles"], Kind::Length, 1609.344, "km"),
    u("yd", &["yard", "yards"], Kind::Length, 0.9144, "m"),
    u("ft", &["pied", "pieds", "feet", "foot"], Kind::Length, 0.3048, "m"),
    u("in", &["pouce", "pouces", "inch", "inches"], Kind::Length, 0.0254, "cm"),
    u("kg", &["kilo", "kilos"], Kind::Mass, 1.0, "lb"),
    u("g", &["gramme", "grammes"], Kind::Mass, 0.001, "oz"),
    u("lb", &["lbs", "livre", "livres", "pound", "pounds"], Kind::Mass, 0.453_592_37, "kg"),
    u("oz", &["once", "onces", "ounce", "ounces"], Kind::Mass, 0.028_349_523_125, "g"),
    u("°C", &["c", "°c", "celsius"], Kind::Temp, 0.0, "°F"),
    u("°F", &["f", "°f", "fahrenheit"], Kind::Temp, 0.0, "°C"),
    u("K", &["k", "kelvin"], Kind::Temp, 0.0, "°C"),
    // octets : Ko, Mo, Go = 1024 (comme l'Explorateur) ; KB, MB, GB = 1000
    u("o", &["octet", "octets"], Kind::Data, 1.0, "Ko"),
    u("Ko", &[], Kind::Data, KI, "Mo"),
    u("Mo", &[], Kind::Data, KI * KI, "Go"),
    u("Go", &[], Kind::Data, KI * KI * KI, "Mo"),
    u("To", &[], Kind::Data, KI * KI * KI * KI, "Go"),
    u("KB", &[], Kind::Data, 1e3, "MB"),
    u("MB", &[], Kind::Data, 1e6, "GB"),
    u("GB", &[], Kind::Data, 1e9, "MB"),
    u("TB", &[], Kind::Data, 1e12, "GB"),
];

fn unit(s: &str) -> Option<Unit> {
    let s = s.trim();
    // insensible à la casse : « mo » = Mo, « mb » = MB (deux unités distinctes, pas de confusion)
    UNITS.iter().find(|u| u.name.eq_ignore_ascii_case(s) || u.aliases.iter().any(|a| a.eq_ignore_ascii_case(s))).copied()
}

fn to_kelvin(v: f64, u: &str) -> f64 {
    match u {
        "°C" => v + 273.15,
        "°F" => (v - 32.0) * 5.0 / 9.0 + 273.15,
        _ => v,
    }
}

fn from_kelvin(k: f64, u: &str) -> f64 {
    match u {
        "°C" => k - 273.15,
        "°F" => (k - 273.15) * 9.0 / 5.0 + 32.0,
        _ => k,
    }
}

/// Nombre en tête de la requête (« -40 », « 3,5 »).
fn number(q: &str) -> Option<f64> {
    let end = q.char_indices().find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || c == ',' || (i == 0 && c == '-'))).map_or(q.len(), |x| x.0);
    q[..end].replace(',', ".").parse().ok()
}

/// « <nombre> <unité> [en|to|in|->|→ <unité>] » → (valeur convertie, unité de départ, unité d'arrivée).
fn convert(q: &str) -> Option<(f64, Unit, Unit)> {
    let v = number(q)?;
    let end = q.char_indices().find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || c == ',' || (i == 0 && c == '-'))).map(|x| x.0)?;
    let rest = &q[end..];
    let lower = rest.to_lowercase();
    let split = [" en ", " to ", " in ", "->", "→", " vers "].iter().find_map(|sep| lower.find(sep).map(|i| (i, sep.len())));
    let (from, to) = match split {
        Some((i, len)) => (unit(&rest[..i])?, unit(&rest[i + len..])?),
        None => {
            let from = unit(rest)?;
            // « 4k », « 3d » : une seule lettre collée au nombre n'est pas une unité (sauf si on précise la cible)
            if from.name.chars().count() <= 2 && !rest.starts_with(' ') && rest.trim().chars().count() == 1 {
                return None;
            }
            (from, unit(from.default)?)
        }
    };
    if from.kind != to.kind {
        return None;
    }
    let out = if from.kind == Kind::Temp { from_kelvin(to_kelvin(v, from.name), to.name) } else { v * from.factor / to.factor };
    Some((out, from, to))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(q: &str) -> Option<String> {
        eval(q).map(|a| a.copy)
    }

    #[test]
    fn expressions() {
        assert_eq!(r("12*4+3").as_deref(), Some("51"));
        assert_eq!(r("2 + 3 * 4").as_deref(), Some("14"), "priorités");
        assert_eq!(r("2^10").as_deref(), Some("1024"));
        assert_eq!(r("2^3^2").as_deref(), Some("512"), "puissance à droite d'abord");
        assert_eq!(r("-2^2").as_deref(), Some("-4"));
        assert_eq!(r("(3,5+1)/2").as_deref(), Some("2,25"), "virgule gardée");
        assert_eq!(r("1/3").as_deref(), Some("0.3333333333"));
        assert_eq!(r("7 % 3").as_deref(), Some("1"));
        assert_eq!(r("sqrt 2").as_deref(), Some("1.414213562"));
        assert_eq!(r("2*pi").as_deref(), Some("6.283185307"));
        assert_eq!(r("0.1+0.2").as_deref(), Some("0.3"), "pas de 0.30000000000000004");
        assert_eq!(r("10^20").as_deref(), Some("1e20"));
        assert_eq!(eval("2*3").unwrap().text, "= 6");
    }

    #[test]
    fn not_calculations() {
        for q in ["10/0", "5 % 0", "42", "2+", "(1+2", "abc", "7zip", "c++", "sqrt -1", "1..2+3", ""] {
            assert_eq!(r(q), None, "{q}");
        }
    }

    #[test]
    fn conversions() {
        assert_eq!(r("10 km en mi").as_deref(), Some("6.213711922"));
        assert_eq!(r("10km to mi").as_deref(), Some("6.213711922"));
        assert_eq!(r("-40 c en f").as_deref(), Some("-40"));
        assert_eq!(r("100 °C").as_deref(), Some("212"), "vers l'unité habituelle");
        assert_eq!(r("0 k en c").as_deref(), Some("-273.15"));
        assert_eq!(r("1 Go en Mo").as_deref(), Some("1024"));
        assert_eq!(r("1 GB en MB").as_deref(), Some("1000"));
        assert_eq!(r("1 kg to lb").as_deref(), Some("2.204622622"));
        assert_eq!(r("3,5 pouces en cm").as_deref(), Some("8,89"));
        assert_eq!(eval("10 km").unwrap().text, "10 km = 6.213711922 mi");
        assert_eq!(r("1 kg en km"), None, "pas la même grandeur");
        assert_eq!(r("4k"), None, "une lettre collée n'est pas une unité");
        assert_eq!(r("3 pommes"), None);
    }
}
