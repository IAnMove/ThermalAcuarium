//! Decoración del arrecife (los procesos protegidos de la sesión): algas, corales duros, abanicos
//! de mar, anémonas y roca viva. Cada pieza se pinta a un sprite; las blandas tienen varias fases
//! de balanceo y la GPU mezcla las dos más cercanas para que el movimiento sea suave.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;

use gtk::cairo::{self, Context, ImageSurface, RadialGradient};

use super::noise;
use crate::draw::{Quiet, closed_smooth, lin, rgba};
use crate::sim::{Decor, mix64, unit};
use crate::theme::{Rgb, darken, lighten, mix};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Kelp,
    Acropora,
    Brain,
    SeaFan,
    Anemone,
    Rock,
}

pub fn kind_of(d: &Decor) -> Kind {
    match d.kind {
        0 => Kind::Kelp,
        1 => match d.seed % 3 {
            0 => Kind::Acropora,
            1 => Kind::Brain,
            _ => Kind::SeaFan,
        },
        2 => Kind::Anemone,
        _ => Kind::Rock,
    }
}

/// Fases de balanceo del sprite (1 = rígido).
pub fn phases(k: Kind) -> usize {
    match k {
        Kind::Kelp | Kind::SeaFan | Kind::Anemone => 16,
        _ => 1,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DKey {
    pub kind: Kind,
    pub seed: u64,
    pub size: i16,
    pub phase: u8,
    pub agit: u8,
}

pub struct DecorSprite {
    pub surf: ImageSurface,
    /// Origen (base del elemento, centrado) dentro del sprite, en px lógicos.
    pub ox: f64,
    pub oy: f64,
    /// Tamaño con el que se pintó.
    pub size: f64,
}

const STEP: f64 = 1.14;

/// Cubo de tamaño y tamaño redondeado de una pieza de `size` px.
pub fn bucket(size: f64) -> (i16, f64) {
    let b = ((size.max(8.0) / 20.0).ln() / STEP.ln()).round() as i16;
    (b, 20.0 * STEP.powi(b as i32))
}

pub fn key(d: &Decor, phase: usize, agit: bool) -> (DKey, f64) {
    let (b, bs) = bucket(d.size);
    (DKey { kind: kind_of(d), seed: d.seed, size: b, phase: phase as u8, agit: agit as u8 }, bs)
}

thread_local! {
    static ROUGH: ImageSurface = noise::grey_tile(256, 8, 4, 7, 1.8);
}

pub fn render(k: &DKey, size: f64, ds: f64) -> DecorSprite {
    let s = size;
    let (hw, top, bot) = (s * 0.72 + 8.0, s * 1.35 + 10.0, 10.0);
    let (w, h) = (hw * 2.0, top + bot);
    let (wp, hp) = ((w * ds).ceil().max(2.0) as i32, (h * ds).ceil().max(2.0) as i32);
    let surf = ImageSurface::create(cairo::Format::ARgb32, wp, hp).expect("decor");
    surf.set_device_scale(ds, ds);
    {
        let cr = Context::new(&surf).expect("cairo");
        cr.translate(hw, top);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Round);
        let np = phases(k.kind);
        let ph = 2.0 * PI * k.phase as f64 / np as f64;
        let agit = if k.agit == 1 { 1.8 } else { 1.0 };
        match k.kind {
            Kind::Kelp => kelp(&cr, s, k.seed, ph, agit),
            Kind::Acropora => acropora(&cr, s, k.seed),
            Kind::Brain => brain(&cr, s, k.seed),
            Kind::SeaFan => sea_fan(&cr, s, k.seed, ph, agit),
            Kind::Anemone => anemone(&cr, s, k.seed, ph, agit),
            Kind::Rock => ROUGH.with(|r| super::scene::rock(&cr, 0.0, 0.0, s * 1.05, s * 0.72, k.seed, r)),
        }
    }
    DecorSprite { surf, ox: hw, oy: top, size }
}

// ---------------------------------------------------------------- piezas

fn shade(c: Rgb, v: f64) -> Rgb {
    if v >= 0.0 { lighten(c, v) } else { darken(c, -v) }
}

/// Macroalgas: láminas con nervio central, verdes oliva o rojas.
fn kelp(cr: &Context, s: f64, seed: u64, ph: f64, agit: f64) {
    let n = 4 + (seed % 3) as usize;
    let red = seed.is_multiple_of(4);
    for i in 0..n {
        let sd = seed.wrapping_add(i as u64 * 17);
        let k = i as f64 - (n as f64 - 1.0) / 2.0;
        let hgt = s * (0.75 + 0.5 * unit(sd));
        let wid = s * (0.045 + 0.04 * unit(sd ^ 0x55));
        let x0 = k * s * 0.12;
        let lean = (unit(sd ^ 0x99) - 0.5) * s * 0.22;
        let sway = (ph + i as f64 * 1.3).sin() * s * 0.09 * agit;
        let col = if red {
            mix((0.62, 0.14, 0.20), (0.80, 0.30, 0.30), unit(sd ^ 3))
        } else {
            mix((0.30, 0.42, 0.10), (0.46, 0.58, 0.18), unit(sd ^ 3))
        };
        let tip = (x0 + lean + sway, -hgt);
        let m1 = (x0 + lean * 0.2 + sway * 0.2, -hgt * 0.35);
        let m2 = (x0 + lean * 0.7 + sway * 0.7, -hgt * 0.70);
        cr.move_to(x0 - wid, 0.0);
        cr.curve_to(m1.0 - wid * 1.3, m1.1, m2.0 - wid * 0.9, m2.1, tip.0, tip.1);
        cr.curve_to(m2.0 + wid * 0.9, m2.1, m1.0 + wid * 1.3, m1.1, x0 + wid, 0.0);
        cr.close_path();
        let _ = cr.set_source(lin(0.0, 0.0, 0.0, -hgt, &[(0.0, darken(col, 0.45), 1.0), (0.6, col, 0.97), (1.0, lighten(col, 0.25), 0.9)]));
        cr.fill_keep();
        rgba(cr, darken(col, 0.6), 0.35);
        cr.set_line_width(0.8);
        cr.stroke_q();
        // Nervio central.
        cr.move_to(x0, 0.0);
        cr.curve_to(m1.0, m1.1, m2.0, m2.1, tip.0, tip.1);
        rgba(cr, lighten(col, 0.4), 0.35);
        cr.set_line_width((wid * 0.18).max(0.6));
        cr.stroke_q();
    }
}

/// Coral ramificado (Acropora): ramas cónicas con pólipos y puntas claras.
fn acropora(cr: &Context, s: f64, seed: u64) {
    let (col, tip) = match seed % 3 {
        0 => ((0.62, 0.34, 0.66), (0.92, 0.82, 0.95)),
        1 => ((0.50, 0.58, 0.26), (0.88, 0.92, 0.70)),
        _ => ((0.74, 0.50, 0.38), (0.98, 0.90, 0.80)),
    };
    // Roca de base.
    rgba(cr, (0.22, 0.20, 0.24), 0.95);
    cr.save_q();
    cr.translate(0.0, 2.0);
    cr.scale(1.0, 0.45);
    cr.arc(0.0, 0.0, s * 0.18, 0.0, 2.0 * PI);
    cr.restore_q();
    cr.fill_q();
    let main = 3 + (seed >> 8) as usize % 2;
    for i in 0..main {
        let a = -PI / 2.0 + (i as f64 - (main as f64 - 1.0) / 2.0) * 0.55 + (unit(seed + i as u64) - 0.5) * 0.2;
        acro(cr, (i as f64 - (main as f64 - 1.0) / 2.0) * s * 0.07, 0.0, a, s * 0.30, s * 0.075, 4, mix64(seed + i as u64), col, tip);
    }
}

#[allow(clippy::too_many_arguments)]
fn acro(cr: &Context, x: f64, y: f64, ang: f64, len: f64, w: f64, depth: u32, seed: u64, col: Rgb, tip: Rgb) {
    let (nx, ny) = (x + ang.cos() * len, y + ang.sin() * len);
    let w1 = (w * 0.72).max(0.8);
    // Sombra, cuerpo, luz y pólipos: da volumen a cada rama con tres trazos.
    cr.set_line_width(w * 1.2);
    rgba(cr, darken(col, 0.55), 0.95);
    cr.move_to(x, y);
    cr.line_to(nx, ny);
    cr.stroke_q();
    let _ = w1;
    cr.set_line_width(w);
    rgba(cr, col, 1.0);
    cr.move_to(x, y);
    cr.line_to(nx, ny);
    cr.stroke_q();
    cr.set_line_width((w * 0.32).max(0.6));
    rgba(cr, lighten(col, 0.4), 0.55);
    cr.move_to(x - w * 0.18, y - w * 0.12);
    cr.line_to(nx - w * 0.18, ny - w * 0.12);
    cr.stroke_q();
    let polyps = (len / (w * 0.9)).max(2.0) as u32;
    rgba(cr, lighten(col, 0.55), 0.55);
    for k in 0..polyps {
        let t = (k as f64 + 0.5) / polyps as f64;
        let side = if k % 2 == 0 { 1.0 } else { -1.0 };
        let (px, py) = (x + (nx - x) * t + ang.sin() * w * 0.25 * side, y + (ny - y) * t - ang.cos() * w * 0.25 * side);
        cr.arc(px, py, (w * 0.13).max(0.5), 0.0, 2.0 * PI);
        cr.fill_q();
    }
    if depth > 0 {
        let n = 2 + (mix64(seed) % 2) as usize;
        for i in 0..n {
            let spread = (i as f64 / (n - 1) as f64 - 0.5) * 1.05 + (unit(seed + i as u64 * 7) - 0.5) * 0.45;
            acro(cr, nx, ny, ang + spread, len * 0.74, w * 0.74, depth - 1, mix64(seed + i as u64 + 1), col, tip);
        }
    } else {
        rgba(cr, tip, 0.95);
        cr.arc(nx, ny, w * 0.62, 0.0, 2.0 * PI);
        cr.fill_q();
    }
}

/// Coral cerebro: cúpula con surcos sinuosos.
fn brain(cr: &Context, s: f64, seed: u64) {
    let col = match seed % 3 {
        0 => (0.52, 0.58, 0.28),
        1 => (0.66, 0.54, 0.30),
        _ => (0.40, 0.55, 0.45),
    };
    let (rx, ry) = (s * 0.52, s * 0.40);
    rgba(cr, (0.18, 0.16, 0.20), 0.95);
    cr.save_q();
    cr.translate(0.0, 3.0);
    cr.scale(1.0, 0.3);
    cr.arc(0.0, 0.0, rx * 0.95, 0.0, 2.0 * PI);
    cr.restore_q();
    cr.fill_q();
    cr.save_q();
    cr.new_path();
    cr.save_q();
    cr.translate(0.0, 0.0);
    cr.scale(rx, ry);
    cr.arc(0.0, 0.0, 1.0, PI, 2.0 * PI);
    cr.close_path();
    cr.restore_q();
    cr.clip();
    let g = RadialGradient::new(-rx * 0.3, -ry * 0.7, 0.0, 0.0, -ry * 0.2, rx * 1.1);
    g.add_color_stop_rgba(0.0, lighten(col, 0.35).0, lighten(col, 0.35).1, lighten(col, 0.35).2, 1.0);
    g.add_color_stop_rgba(0.55, col.0, col.1, col.2, 1.0);
    g.add_color_stop_rgba(1.0, darken(col, 0.55).0, darken(col, 0.55).1, darken(col, 0.55).2, 1.0);
    let _ = cr.set_source(&g);
    let _ = cr.paint();
    // Surcos: curvas que serpentean siguiendo la cúpula, con luz y sombra.
    for pass in 0..2 {
        for r in 0..16 {
            let base = -ry * (0.04 + 0.062 * r as f64);
            let amp = ry * 0.05;
            for k in 0..=40 {
                let x = -rx + 2.0 * rx * k as f64 / 40.0;
                let dome = (1.0 - (x / rx).powi(2)).max(0.0).sqrt();
                let y = base * dome.max(0.2) * 1.15 + amp * (noise::fbm(x * 0.05 + r as f64, r as f64 * 1.7, 3, seed as u32, 0) - 0.5) * 3.0 + if pass == 1 { 1.2 } else { 0.0 };
                if k == 0 {
                    cr.move_to(x, y);
                } else {
                    cr.line_to(x, y);
                }
            }
            if pass == 0 {
                rgba(cr, darken(col, 0.65), 0.55);
                cr.set_line_width((s * 0.016).max(1.0));
            } else {
                rgba(cr, lighten(col, 0.5), 0.22);
                cr.set_line_width((s * 0.008).max(0.7));
            }
            cr.stroke_q();
        }
    }
    cr.restore_q();
}

/// Abanico de mar (gorgonia): rejilla de ramas finas con forma de abanico.
fn sea_fan(cr: &Context, s: f64, seed: u64, ph: f64, agit: f64) {
    let col = match seed % 3 {
        0 => (0.66, 0.20, 0.52),
        1 => (0.82, 0.34, 0.18),
        _ => (0.55, 0.16, 0.30),
    };
    let sway = ph.sin() * 0.06 * agit;
    cr.save_q();
    cr.rotate(sway);
    // Tronco.
    rgba(cr, darken(col, 0.5), 1.0);
    cr.set_line_width((s * 0.035).max(1.5));
    cr.move_to(0.0, 4.0);
    cr.line_to(0.0, -s * 0.14);
    cr.stroke_q();
    let m = 13;
    let r_max = s * 1.02;
    let span = 1.25;
    let ring = |frac: f64, i: usize| -> (f64, f64) {
        let a = -PI / 2.0 - span + 2.0 * span * i as f64 / (m - 1) as f64;
        let bend = (ph + frac * 2.0 + i as f64 * 0.2).sin() * s * 0.012 * agit * frac;
        let r = r_max * frac * (0.88 + 0.12 * (PI * i as f64 / (m - 1) as f64).sin());
        (a.cos() * r + bend, -s * 0.12 + a.sin() * r)
    };
    // Ramas radiales.
    cr.set_line_width((s * 0.014).max(0.8));
    rgba(cr, col, 0.95);
    for i in 0..m {
        let steps = 10;
        for k in 0..=steps {
            let p = ring(0.12 + 0.88 * k as f64 / steps as f64, i);
            if k == 0 {
                cr.move_to(p.0, p.1);
            } else {
                cr.line_to(p.0, p.1);
            }
        }
    }
    cr.stroke_q();
    // Celosía: uniones entre ramas.
    cr.set_line_width((s * 0.008).max(0.6));
    rgba(cr, lighten(col, 0.15), 0.8);
    for r in 0..9 {
        let frac = 0.22 + 0.78 * r as f64 / 8.0;
        let off = if r % 2 == 0 { 0 } else { 1 };
        for i in (off..m - 1).step_by(1) {
            let (a, b) = (ring(frac, i), ring(frac + 0.05, i + 1));
            cr.move_to(a.0, a.1);
            cr.line_to(b.0, b.1);
        }
    }
    cr.stroke_q();
    cr.restore_q();
}

/// Anémona de punta de burbuja: tentáculos con bulbos claros.
fn anemone(cr: &Context, s: f64, seed: u64, ph: f64, agit: f64) {
    let (body, tip) = match seed % 3 {
        0 => ((0.58, 0.46, 0.30), (0.92, 0.42, 0.58)),
        1 => ((0.40, 0.52, 0.28), (0.72, 0.36, 0.70)),
        _ => ((0.66, 0.52, 0.34), (0.95, 0.80, 0.55)),
    };
    // Pie.
    let _ = cr.set_source(lin(0.0, -s * 0.12, 0.0, 2.0, &[(0.0, lighten(body, 0.1), 1.0), (1.0, darken(body, 0.6), 1.0)]));
    cr.save_q();
    cr.translate(0.0, -s * 0.03);
    cr.scale(1.0, 0.45);
    cr.arc(0.0, 0.0, s * 0.19, 0.0, 2.0 * PI);
    cr.restore_q();
    cr.fill_q();
    let n = 46;
    // Los de atrás primero para que los de delante los tapen.
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| {
        let za = (a as f64 * 2.399).sin();
        let zb = (b as f64 * 2.399).sin();
        za.partial_cmp(&zb).unwrap_or(std::cmp::Ordering::Equal)
    });
    for i in idx {
        let sd = seed.wrapping_add(i as u64 * 13);
        let ang = i as f64 * 2.399; // ángulo áureo: reparto uniforme
        let rr = 0.35 + 0.65 * unit(sd);
        let bx = ang.cos() * s * 0.15 * rr;
        let by = -s * 0.07 + ang.sin() * s * 0.025 * rr;
        let spread = bx / (s * 0.15).max(1.0);
        let len = s * (0.40 + 0.22 * unit(sd ^ 0x31)) * (1.0 - 0.25 * spread.abs());
        let sw = (ph + i as f64 * 0.7).sin() * s * 0.05 * agit + (ph * 0.5 + i as f64).cos() * s * 0.02 * agit;
        let tx = bx + spread * s * 0.30 + sw;
        let ty = by - len;
        let c1 = (bx + spread * s * 0.10, by - len * 0.35);
        let c2 = (tx - sw * 0.6, ty + len * 0.30);
        let wd = (s * 0.030).max(1.4);
        cr.move_to(bx, by);
        cr.curve_to(c1.0, c1.1, c2.0, c2.1, tx, ty);
        rgba(cr, darken(body, 0.25), 0.92);
        cr.set_line_width(wd);
        cr.stroke_q();
        cr.move_to(bx, by);
        cr.curve_to(c1.0, c1.1, c2.0, c2.1, tx, ty);
        rgba(cr, lighten(body, 0.25), 0.40);
        cr.set_line_width(wd * 0.4);
        cr.stroke_q();
        // Bulbo de la punta.
        let br = wd * 1.25;
        let g = RadialGradient::new(tx - br * 0.3, ty - br * 0.3, 0.0, tx, ty, br * 1.1);
        g.add_color_stop_rgba(0.0, lighten(tip, 0.45).0, lighten(tip, 0.45).1, lighten(tip, 0.45).2, 1.0);
        g.add_color_stop_rgba(1.0, shade(tip, -0.2).0, shade(tip, -0.2).1, shade(tip, -0.2).2, 0.95);
        let _ = cr.set_source(&g);
        cr.arc(tx, ty, br, 0.0, 2.0 * PI);
        cr.fill_q();
    }
    let _ = closed_smooth;
}

// ---------------------------------------------------------------- caché para el renderizador Cairo

#[derive(Default)]
struct Cache {
    map: HashMap<DKey, DecorSprite>,
    ds: f64,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// Pinta una pieza con el sprite más cercano a la fase actual (renderizador Cairo de referencia).
pub fn draw(cr: &Context, d: &Decor, x: f64, base: f64, time: f64, boil: f64, ds: f64) {
    let kind = kind_of(d);
    let np = phases(kind);
    let agit = boil > 0.35;
    let f = (time * (0.16 + 0.12 * boil)).rem_euclid(1.0) * np as f64;
    let ph = (f.round() as usize) % np;
    let (k, bs) = key(d, ph, agit);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if (c.ds - ds).abs() > 0.01 {
            c.map.clear();
            c.ds = ds;
        }
        let spr = c.map.entry(k).or_insert_with(|| render(&k, bs, ds));
        let sc = d.size / spr.size;
        cr.save_q();
        cr.translate(x, base);
        cr.scale(sc, sc);
        let _ = cr.set_source_surface(&spr.surf, -spr.ox, -spr.oy);
        let _ = cr.paint();
        cr.restore_q();
    });
}
