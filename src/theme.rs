//! Paleta y estilo: lee el tema activo de Omarchy y lo aplica a GTK y a Cairo.

use std::collections::HashMap;
use std::fs;

pub type Rgb = (f64, f64, f64);

const CSS: &str = r#"
window.ta { background: @ta_dbg; color: @ta_fg; }
window.ta label, window.ta button { font-family: "FONTFAMILY", monospace; }
window.ta headerbar { background: @ta_dbg; color: @ta_fg; border-bottom: 1px solid @ta_muted; box-shadow: none;
                      border-radius: 0; min-height: 38px; }
window.ta headerbar windowcontrols button { color: @ta_fg; }
.title { color: @ta_accent; font-weight: 800; letter-spacing: 3px; font-size: 12px; }
.chip { padding: 0 10px; font-size: 12px; }
.chipsep { color: @ta_muted; font-size: 12px; }
button.stylebtn { color: @ta_accent; font-size: 10px; font-weight: 800; letter-spacing: 2px; border-radius: 0;
                  min-height: 0; padding: 2px 10px; box-shadow: none; }
button.stylebtn:hover { background: @ta_sel; }
"#;

pub fn parse_hex(s: &str) -> Rgb {
    let s = s.trim().trim_start_matches('#');
    if s.len() < 6 {
        return (1.0, 1.0, 1.0);
    }
    let c = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(255) as f64 / 255.0;
    (c(0), c(2), c(4))
}

pub fn to_hex(c: Rgb) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c.0), b(c.1), b(c.2))
}

pub fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t)
}

pub fn lighten(c: Rgb, t: f64) -> Rgb {
    mix(c, (1.0, 1.0, 1.0), t)
}

pub fn darken(c: Rgb, t: f64) -> Rgb {
    mix(c, (0.0, 0.0, 0.0), t)
}

pub fn luma(c: Rgb) -> f64 {
    0.2126 * c.0 + 0.7152 * c.1 + 0.0722 * c.2
}

/// Conserva el matiz de `c` pero lo lleva a la luminosidad `l`: así el agua y la arena
/// siguen la paleta del tema sin perder el aspecto de acuario en temas claros.
pub fn tone(c: Rgb, l: f64) -> Rgb {
    let k = l / luma(c).max(0.05);
    ((c.0 * k).min(1.0), (c.1 * k).min(1.0), (c.2 * k).min(1.0))
}

#[derive(Clone)]
pub struct Palette {
    pub raw: String,
    pub dark: bool,
    hex: HashMap<String, String>,
    pub bg: Rgb,
    pub dbg: Rgb,
    pub fg: Rgb,
    pub dfg: Rgb,
    pub bfg: Rgb,
    pub accent: Rgb,
    pub red: Rgb,
    pub orange: Rgb,
    pub yellow: Rgb,
    pub green: Rgb,
    pub cyan: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    /// Colores de los peces (el índice sale del hash del nombre del proceso).
    pub fish: [Rgb; 8],
    stops: [(f64, Rgb); 5],
}

impl Palette {
    pub fn load() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let base = format!("{home}/.local/state/omarchy/current");
        let txt = fs::read_to_string(format!("{base}/theme/colors.toml")).unwrap_or_default();
        let name = fs::read_to_string(format!("{base}/theme.name")).unwrap_or_default();
        let mut m: HashMap<String, String> = [
            ("background", "#1a1b26"), ("dark_background", "#13141c"), ("selection", "#292e42"),
            ("muted", "#414868"), ("accent", "#7aa2f7"), ("foreground", "#a9b1d6"),
            ("dark_foreground", "#565f89"), ("bright_foreground", "#c0caf5"), ("red", "#f7768e"),
            ("yellow", "#e0af68"), ("orange", "#eb927b"), ("green", "#9ece6a"), ("cyan", "#449dab"),
            ("blue", "#7aa2f7"), ("magenta", "#ad8ee6"), ("bright_cyan", "#0db9d7"), ("mode", "dark"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        for line in txt.lines() {
            if let Some((k, v)) = line.split_once('=') {
                m.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
            }
        }
        let c = |k: &str| parse_hex(m.get(k).map_or("#ffffff", |s| s.as_str()));
        let (blue, green, yellow, orange, red) = (c("blue"), c("green"), c("yellow"), c("orange"), c("red"));
        let (magenta, cyan) = (c("magenta"), c("cyan"));
        Palette {
            raw: format!("{txt}{name}"),
            dark: m.get("mode").is_none_or(|s| s != "light"),
            bg: c("background"),
            dbg: c("dark_background"),
            fg: c("foreground"),
            dfg: c("dark_foreground"),
            bfg: c("bright_foreground"),
            accent: c("accent"),
            red,
            orange,
            yellow,
            green,
            cyan,
            blue,
            magenta,
            fish: [blue, orange, green, magenta, yellow, c("bright_cyan"), red, cyan],
            stops: [(40.0, blue), (58.0, green), (74.0, yellow), (88.0, orange), (100.0, red)],
            hex: m,
        }
    }

    pub fn css(&self, font: &str) -> String {
        let mut out = String::new();
        for (n, k) in [
            ("ta_bg", "background"), ("ta_dbg", "dark_background"), ("ta_fg", "foreground"),
            ("ta_dfg", "dark_foreground"), ("ta_bfg", "bright_foreground"), ("ta_muted", "muted"),
            ("ta_sel", "selection"), ("ta_accent", "accent"), ("ta_red", "red"),
        ] {
            out += &format!("@define-color {n} {};\n", self.hex.get(k).cloned().unwrap_or_default());
        }
        out + &CSS.replace("FONTFAMILY", font)
    }

    /// Color de la escala térmica (frío → azul, crítico → rojo).
    pub fn heat(&self, t: f64) -> Rgb {
        let s = &self.stops;
        if t <= s[0].0 {
            return s[0].1;
        }
        for w in s.windows(2) {
            let ((t0, c0), (t1, c1)) = (w[0], w[1]);
            if t <= t1 {
                return mix(c0, c1, (t - t0) / (t1 - t0));
            }
        }
        s[4].1
    }
}

pub fn current_font() -> String {
    std::process::Command::new("omarchy")
        .args(["font", "current"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "JetBrainsMono Nerd Font".into())
}

/// Estilo visual de la pecera. El realista no sigue la paleta de Omarchy (un pez payaso no es
/// morado Tokyo Night): solo el marco de la ventana y los diálogos la siguen.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Style {
    Cartoon,
    Realistic,
}

impl Style {
    pub fn parse(s: &str) -> Option<Style> {
        match s.trim().to_lowercase().as_str() {
            "real" | "realistic" | "realista" => Some(Style::Realistic),
            "stylized" | "cartoon" | "estilizado" => Some(Style::Cartoon),
            _ => None,
        }
    }

    pub fn toggled(self) -> Style {
        match self {
            Style::Cartoon => Style::Realistic,
            Style::Realistic => Style::Cartoon,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Style::Cartoon => "stylized",
            Style::Realistic => "real",
        }
    }

    fn file() -> std::path::PathBuf {
        let home = std::env::var("HOME").unwrap_or_default();
        let base = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
        std::path::PathBuf::from(base).join("thermalacuarium").join("style")
    }

    /// Estilo guardado la última vez (por defecto, el estilizado).
    pub fn load() -> Style {
        fs::read_to_string(Self::file()).ok().and_then(|s| Style::parse(&s)).unwrap_or(Style::Cartoon)
    }

    pub fn save(self) {
        let f = Self::file();
        if let Some(dir) = f.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(f, self.id());
    }
}
