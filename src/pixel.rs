//! Estilo Pixel: peces de pixel art dibujados a mano, uno por especie, con la paleta del tema.
//! Cada sprite se dibuja a un píxel por celda y se amplía por un factor entero (el tamaño del
//! proceso), sin suavizado: un proceso de 1 MB es un pez de 1×, uno de 8 GB uno de 8×.
//! El contorno de tinta se añade solo (dilatación de una celda), como en el logo de Omarchy.

use std::f64::consts::PI;

use gtk::cairo::{self, Context, ImageSurface};

use crate::draw::{Quiet, Sprite, View, cell, decor, decor_box, labels, rays, rgba, set_cell, snap, surface_at, water_colors};
use crate::sim::{BubbleKind, Fish, Life, Look, Pick, mix64};
use crate::theme::{Palette, Rgb, darken, lighten, mix};

// ---------------------------------------------------------------- sprites

/// Leyenda: `.` nada · `b` cuerpo · `d` cuerpo oscuro · `l` vientre · `f` aleta · `p` patrón
/// (color secundario) · `w` blanco del ojo · `e` pupila · `#` tinta. El contorno se añade solo.
pub struct Species {
    pub name: &'static str,
    /// Cuerpo mirando a la derecha; la columna 0 es donde se pega la cola.
    pub body: &'static [&'static str],
    /// Fila del cuerpo donde se centra la cola (y el origen del pez).
    pub anchor: usize,
    pub tail: &'static [&'static str],
    /// Cuánto se cimbrea la cola (cizalla horizontal por fila).
    pub wag: f64,
}

const FORK: &[&str] = &["f....", "ff...", "fff..", "ffff.", "fffff", "ffff.", "fff..", "ff...", "f...."];
const SMALL: &[&str] = &["f..", "ff.", "fff", "ff.", "f.."];
const FAN: &[&str] = &["ff..", "fff.", "ffff", "ffff", "ffff", "fff.", "ff.."];
const CRESCENT: &[&str] = &["ff...", "fff..", ".fff.", "..fff", "...ff", "..fff", ".fff.", "fff..", "ff..."];
const VEIL: &[&str] = &[
    "ff.....", "fff....", "ffff...", "fffff..", "ffffff.", "fffffff", "fffffff", "ffffff.", "fffff..", "ffff...", "fff....", "ff.....",
];

pub const SPECIES: [Species; 7] = [
    Species {
        name: "tropical",
        body: &[
            "......ffff..........",
            ".....ffffff.........",
            "....ffffffff........",
            "...bbbbbbbbbbb......",
            "..bbbbbbbbbdbbbb....",
            ".bbbbbbbbbbdbbbbb...",
            "bbbbbbbbbbbdbbwwbb..",
            "lbbbbbbbbbbdbbewbb..",
            "llbbbbbbbbbdbbbbb#..",
            ".lllllllllllllll#...",
            "..llllllllllll......",
            "...llllfffll........",
            ".....fffff..........",
        ],
        anchor: 7,
        tail: FORK,
        wag: 0.35,
    },
    Species {
        name: "tetra",
        body: &[
            ".....ffff.............",
            "..bbbbbbbbbbbbbbbbb...",
            ".bbbbbbbbbbbbbbbwwbb..",
            "bpppppppppppppppewbb..",
            ".llllllllllllllllll#..",
            "..llllllllllllllll....",
            ".....ff....ff.........",
        ],
        anchor: 3,
        tail: SMALL,
        wag: 0.5,
    },
    Species {
        name: "angelfish",
        body: &[
            ".......ff.........",
            "......fff.........",
            ".....ffff.........",
            "....fffff.........",
            "...ffffff.........",
            "..bbbbbbbbb.......",
            ".bbbpbbbbpbbbb....",
            "bbbbpbbbbpbbbwwb..",
            "bbbbpbbbbpbbbewb..",
            "lbbbpbbbbpbbbbb#..",
            ".lllpllllpllll#...",
            "..lllllllll.......",
            "...ffffff.........",
            "....fffff.........",
            ".....ffff.........",
            "......fff.........",
        ],
        anchor: 8,
        tail: FAN,
        wag: 0.3,
    },
    Species {
        name: "puffer",
        body: &[
            ".....bbbbb........",
            "...bbbbbbbbb......",
            "..bbpbbbbbpbbb....",
            ".bbbbbbpbbbbbbb...",
            ".bbpbbbbbbbbbwwb..",
            "bbbbbbpbbbbpbewb..",
            "bbpbbbbbbbbbbbb#..",
            ".llbbbpbblbllll#..",
            ".lllllllllllll....",
            "..lllllllllll.....",
            "...lllllllll......",
            ".....fffff........",
        ],
        anchor: 5,
        tail: SMALL,
        wag: 0.5,
    },
    Species {
        name: "betta",
        body: &[
            ".....fffffff........",
            "...fffffffffff......",
            "..ffffffffffff......",
            ".ffbbbbbbbbbbbb.....",
            "fffbbbbbbbbbbbwwb...",
            "ffbbbbbbbbbbbbewb...",
            "ffbbbbbbbbbbbbbb#...",
            "fflllllllllllll#....",
            ".fflllllllll........",
            "..ffffffffff........",
            "...fffffffff........",
            "....ffffff..........",
        ],
        anchor: 5,
        tail: VEIL,
        wag: 0.25,
    },
    Species {
        name: "torpedo",
        body: &[
            ".......ff...............",
            "......fff...............",
            "bbbbbbbbbbbbbbbbbbbb....",
            "bbbbbbbbbbbbbbbbbbbwwb..",
            "dddbbbbbbbbbbbbbbbbewbb.",
            "lllllllllllllllllllll#..",
            ".llllllllllllllll.......",
            "......ff................",
            ".......f................",
        ],
        anchor: 4,
        tail: CRESCENT,
        wag: 0.3,
    },
    Species {
        name: "discus",
        body: &[
            ".....bbbbbb.......",
            "...bbbbbbbbbb.....",
            "..bbpbbbbpbbbb....",
            ".bbbpbbbbpbbbbb...",
            "bbbbpbbbbpbbbwwb..",
            "bbbbpbbbbpbbbewb..",
            "lbbbpbbbbpbbbbb#..",
            "llllpllllpllll#...",
            ".lllplllllllll....",
            "..llllllllllll....",
            "...llllllllll.....",
            ".....llllll.......",
        ],
        anchor: 5,
        tail: FAN,
        wag: 0.3,
    },
];

pub fn species_of(look: &Look) -> usize {
    (mix64(look.seed ^ 0x5151) % SPECIES.len() as u64) as usize
}


/// Rejilla de celdas del pez completo (cola + cuerpo) para un fotograma.
fn grid(sp: &Species, frame: usize, dying: bool) -> (Vec<Vec<char>>, usize, usize) {
    let tw = sp.tail[0].len();
    let th = sp.tail.len();
    let bw = sp.body.iter().map(|r| r.len()).max().unwrap_or(0);
    let bh = sp.body.len();
    // La cola se cimbrea: cada fila se desplaza según su distancia al centro.
    let k = match frame {
        0 => -sp.wag,
        2 => sp.wag,
        _ => 0.0,
    };
    let shift = |r: usize| (k * (r as f64 - (th as f64 - 1.0) / 2.0)).round() as i64;
    let max_shift = (0..th).map(shift).map(|s| s.abs()).max().unwrap_or(0) as usize;
    let tail_x0 = max_shift; // margen para el cimbreo
    let body_x0 = tail_x0 + tw - 1;
    let tail_y0 = sp.anchor as i64 - (th as i64 - 1) / 2;
    let top_pad = (-tail_y0).max(0) as usize;
    let w = body_x0 + bw + max_shift + 2;
    let h = top_pad + bh.max((tail_y0 + th as i64).max(0) as usize) + 2;
    let mut g = vec![vec!['.'; w + 2]; h + 2];
    for (r, row) in sp.tail.iter().enumerate() {
        let y = (tail_y0 + r as i64 + top_pad as i64 + 1) as usize;
        let dx = shift(r);
        for (c, ch) in row.chars().enumerate() {
            if ch != '.' {
                let x = (tail_x0 as i64 + c as i64 + dx + 1) as usize;
                g[y][x] = ch;
            }
        }
    }
    for (r, row) in sp.body.iter().enumerate() {
        let y = r + top_pad + 1;
        for (c, ch) in row.chars().enumerate() {
            if ch != '.' {
                let ch = if dying && (ch == 'w' || ch == 'e') { '#' } else { ch };
                g[y][body_x0 + c + 1] = ch;
            }
        }
    }
    // Contorno: toda celda vacía pegada a una llena se vuelve tinta.
    let mut out = g.clone();
    for y in 0..g.len() {
        for x in 0..g[y].len() {
            if g[y][x] != '.' {
                continue;
            }
            let filled = |yy: i64, xx: i64| yy >= 0 && xx >= 0 && (yy as usize) < g.len() && (xx as usize) < g[0].len() && g[yy as usize][xx as usize] != '.';
            if filled(y as i64 - 1, x as i64) || filled(y as i64 + 1, x as i64) || filled(y as i64, x as i64 - 1) || filled(y as i64, x as i64 + 1) {
                out[y][x] = '#';
            }
        }
    }
    // Origen: fila del ancla del cuerpo, centro horizontal.
    (out, (w + 2) / 2, sp.anchor + top_pad + 1)
}

/// Colores de un pez: cuerpo con el color primario, patrón con el secundario.
fn colors(look: &Look, pal: &Palette, dying: bool) -> [(char, Rgb); 8] {
    let mut c1 = mix(pal.fish[look.c1], pal.bg, 0.10);
    c1 = if look.tint > 0.0 { lighten(c1, look.tint * 0.10) } else { darken(c1, -look.tint * 0.10) };
    let mut c2 = mix(pal.fish[look.c2], pal.bg, 0.10);
    if dying {
        c1 = mix(c1, pal.dfg, 0.6);
        c2 = mix(c2, pal.dfg, 0.6);
    }
    [
        ('b', c1),
        ('d', darken(c1, 0.28)),
        ('l', lighten(c1, 0.28)),
        ('f', mix(darken(c1, 0.22), c2, 0.35)),
        ('p', c2),
        ('w', pal.bfg),
        ('e', pal.dbg),
        ('#', pal.dbg),
    ]
}

/// Sprite de un pez a un píxel por celda, que se dibuja ampliado por el factor entero `k`.
pub fn render(look: &Look, frame: usize, k: f64, pal: &Palette, dying: bool) -> Sprite {
    let sp = &SPECIES[species_of(look)];
    let (g, ox, oy) = grid(sp, frame, dying);
    let (w, h) = (g[0].len(), g.len());
    let cols = colors(look, pal, dying);
    let mut surf = ImageSurface::create(cairo::Format::ARgb32, w as i32, h as i32).expect("pixel sprite");
    let stride = surf.stride() as usize;
    {
        let mut data = surf.data().expect("pixel data");
        for (y, row) in g.iter().enumerate() {
            for (x, &ch) in row.iter().enumerate() {
                if let Some(&(_, c)) = cols.iter().find(|(k, _)| *k == ch) {
                    let i = y * stride + x * 4;
                    data[i] = (c.2 * 255.0) as u8;
                    data[i + 1] = (c.1 * 255.0) as u8;
                    data[i + 2] = (c.0 * 255.0) as u8;
                    data[i + 3] = 255;
                }
            }
        }
    }
    Sprite { surf, ox: ox as f64 * k, oy: oy as f64 * k, len: w as f64 * k }
}

/// Fotograma de cola según la fase de nado: arriba, centro, abajo, centro.
pub fn frame_of(f: &Fish) -> usize {
    if matches!(f.life, Life::Dying(_)) {
        return 1;
    }
    match ((f.phase / (2.0 * PI)).rem_euclid(1.0) * 4.0) as usize {
        0 => 0,
        2 => 2,
        _ => 1,
    }
}

/// Factor entero con el que se dibuja un pez de longitud `len`.
pub fn scale_of(look: &Look, len: f64) -> f64 {
    let sp = &SPECIES[species_of(look)];
    let (g, _, _) = grid(sp, 1, false);
    (len / g[0].len() as f64).round().clamp(1.0, 12.0)
}

/// Pinta el sprite en coordenadas de la pecera (renderizador Cairo).
pub fn draw_sprite(cr: &Context, f: &Fish, s: &Sprite, k: f64, alpha: f64) {
    cr.save_q();
    cr.translate(snap(f.x), snap(f.y));
    cr.scale(f.face.signum() * k, f.flipv.signum() * k);
    let _ = cr.set_source_surface(&s.surf, -s.ox / k, -s.oy / k);
    cr.source().set_filter(cairo::Filter::Nearest);
    let _ = cr.paint_with_alpha(alpha);
    cr.restore_q();
}

// ---------------------------------------------------------------- escena (Cairo)

/// Agua posterizada en franjas planas (de arriba abajo).
pub fn water_bands(v: &View) -> Vec<(f64, f64, Rgb)> {
    let t = v.tank;
    let (top, bot) = water_colors(v);
    let n = 9;
    (0..n)
        .map(|i| {
            let (y0, y1) = (t.h * i as f64 / n as f64, t.h * (i + 1) as f64 / n as f64);
            (y0, y1, mix(top, bot, (i as f64 + 0.5) / n as f64))
        })
        .collect()
}

/// Pinta `paint` a una celda por píxel y lo amplía sin suavizado.
pub fn pixelated(cr: &Context, x: f64, y: f64, w: f64, h: f64, paint: impl FnOnce(&Context)) {
    let c = cell();
    let (wp, hp) = ((w / c).ceil().max(1.0) as i32, (h / c).ceil().max(1.0) as i32);
    let Ok(surf) = ImageSurface::create(cairo::Format::ARgb32, wp, hp) else { return };
    {
        let Ok(cx) = Context::new(&surf) else { return };
        cx.set_antialias(cairo::Antialias::None);
        cx.scale(1.0 / c, 1.0 / c);
        cx.translate(-x, -y);
        paint(&cx);
    }
    cr.save_q();
    cr.translate(snap(x), snap(y));
    cr.scale(c, c);
    let _ = cr.set_source_surface(&surf, 0.0, 0.0);
    cr.source().set_filter(cairo::Filter::Nearest);
    let _ = cr.paint();
    cr.restore_q();
}

pub fn bubbles(cr: &Context, v: &View) {
    let t = v.tank;
    let (top, _) = water_colors(v);
    let col = lighten(top, 0.85);
    let c = cell();
    for b in &t.bubbles {
        let k = (b.r * 2.0 / c).round().clamp(1.0, 4.0);
        let (x, y) = (snap(b.x - b.r), snap(b.y - b.r));
        match b.kind {
            BubbleKind::Air if k <= 1.0 => {
                rgba(cr, col, 0.55);
                cr.rectangle(x, y, c, c);
                cr.fill_q();
            }
            BubbleKind::Air => {
                rgba(cr, col, 0.12);
                cr.rectangle(x, y, k * c, k * c);
                cr.fill_q();
                rgba(cr, col, 0.45);
                cr.rectangle(x, y, k * c, c);
                cr.rectangle(x, y + (k - 1.0) * c, k * c, c);
                cr.rectangle(x, y, c, k * c);
                cr.rectangle(x + (k - 1.0) * c, y, c, k * c);
                cr.fill_q();
            }
            BubbleKind::Foam => {
                let a = (b.life / b.max_life).clamp(0.0, 1.0);
                rgba(cr, v.pal.bfg, 0.40 * a.sqrt());
                cr.rectangle(x, y, k * c, c);
                cr.fill_q();
            }
        }
    }
}

pub fn surface(cr: &Context, v: &View) {
    let t = v.tank;
    let p = v.pal;
    let c = cell();
    cr.set_antialias(cairo::Antialias::None);
    let (top, _) = water_colors(v);
    let mut x = 0.0;
    while x < t.w {
        let y = snap(surface_at(t, x + c / 2.0));
        rgba(cr, p.dbg, 1.0);
        cr.rectangle(x, 0.0, c, y);
        cr.fill_q();
        rgba(cr, lighten(top, 0.75), 0.7);
        cr.rectangle(x, y, c, c);
        cr.fill_q();
        x += c;
    }
    cr.set_antialias(cairo::Antialias::Default);
}

pub fn draw_tank(cr: &Context, v: &View) {
    let t = v.tank;
    let (w, h) = (t.w, t.h);
    set_cell(t.scale());
    cr.save_q();
    cr.rectangle(0.0, 0.0, w, h);
    cr.clip();
    for (y0, y1, c) in water_bands(v) {
        rgba(cr, c, 1.0);
        cr.rectangle(0.0, y0, w, y1 - y0 + 0.5);
        cr.fill_q();
    }
    pixelated(cr, 0.0, 0.0, w, h, |c| rays(c, v, t.t, t.boil));
    let (top, _) = water_colors(v);
    let c = cell();
    for m in &t.motes {
        rgba(cr, lighten(top, 0.7), 0.10 + 0.22 * m.z);
        cr.rectangle(snap(m.x), snap(m.y), c, c);
        cr.fill_q();
    }
    crate::draw::floor(cr, v, true);
    for (i, d) in t.decor.iter().enumerate() {
        let (bx, by, bw, bh) = decor_box(t, d);
        pixelated(cr, bx, by, bw, bh, |c| decor(c, v, d, false));
        if v.hover == Some(Pick::Decor(i)) {
            rgba(cr, v.pal.accent, 0.6);
            cr.set_line_width(1.5);
            cr.rectangle(bx, by, bw, bh);
            cr.stroke_q();
        }
    }
    if let Some(Pick::Fish(hi)) = v.hover
        && let Some(hf) = t.fish.get(hi)
    {
        for f in t.fish.iter().filter(|f| f.kh == hf.kh && f.life == Life::Alive) {
            rgba(cr, v.pal.accent, 0.16);
            cr.rectangle(snap(f.x - f.len * 0.6), snap(f.y - f.len * 0.35), snap(f.len * 1.2), snap(f.len * 0.7));
            cr.fill_q();
        }
    }
    for &i in &t.order {
        crate::draw::fish(cr, &t.fish[i], v.pal, v.hover == Some(Pick::Fish(i)), 1.0, v.style);
    }
    bubbles(cr, v);
    surface(cr, v);
    crate::draw::overlays(cr, v);
    labels(cr, v);
    cr.restore_q();
}

/// Galería de las especies de píxeles: `--snapshot out.png --gallery --style pixel [--len 160]`.
pub fn gallery(cr: &Context, w: f64, h: f64, len: f64, font: &str, pal: &Palette) {
    use crate::sim::Pattern;
    rgba(cr, mix(pal.blue, pal.dbg, 0.6), 1.0);
    cr.rectangle(0.0, 0.0, w, h);
    cr.fill_q();
    cr.select_font_face(font, cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(12.0);
    let n = SPECIES.len();
    let cols = 4;
    let rows = n.div_ceil(cols);
    for i in 0..n {
        let (cx, cy) = (((i % cols) as f64 + 0.5) * w / cols as f64, ((i / cols) as f64 + 0.5) * h / rows as f64);
        let look = Look { species: 0, real: 0, pattern: Pattern::Plain, c1: i % 8, c2: (i + 3) % 8, tint: 0.0, seed: 0 };
        // Las tres fases de la cola, una al lado de otra.
        for (j, frame) in [0usize, 1, 2].iter().enumerate() {
            let mut lk = look;
            lk.seed = (0..1000).find(|&s| species_of(&Look { seed: s, ..look }) == i).unwrap_or(0);
            let k = scale_of(&lk, len / 3.0);
            let spr = render(&lk, *frame, k, pal, false);
            cr.save_q();
            cr.translate(cx + (j as f64 - 1.0) * len * 0.36, cy);
            cr.scale(k, k);
            let _ = cr.set_source_surface(&spr.surf, -spr.ox / k, -spr.oy / k);
            cr.source().set_filter(cairo::Filter::Nearest);
            let _ = cr.paint();
            cr.restore_q();
        }
        rgba(cr, pal.bfg, 0.9);
        cr.move_to(cx - 30.0, cy + h / rows as f64 * 0.42);
        let _ = cr.show_text(SPECIES[i].name);
    }
}
