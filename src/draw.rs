//! Dibujo de la pecera con Cairo: agua, arena, decoración, peces procedurales y burbujas.
//! Los peces no son sprites: el cuerpo se construye sobre una columna ondulante, así que
//! cuerpo, cola y aletas se deforman al nadar.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;

use gtk::cairo::{self, Context, LinearGradient, RadialGradient};

use crate::i18n::{tr, trf};
use crate::sim::{BubbleKind, Decor, Fish, Life, Look, Pattern, Pick, Tank, mix64, unit};
use crate::sys::fmt_bytes;
use crate::theme::{Palette, Rgb, Style, darken, lighten, luma, mix, to_hex, tone};

pub struct View<'a> {
    pub tank: &'a Tank,
    pub pal: &'a Palette,
    pub font: &'a str,
    pub hover: Option<Pick>,
    /// 0: nombres de los peces grandes · 1: todos · 2: ninguno
    pub names: u8,
    pub paused: bool,
    pub style: Style,
}

// ---------------------------------------------------------------- helpers de Cairo

pub(crate) trait Quiet {
    fn fill_q(&self);
    fn fill_keep(&self);
    fn stroke_preserve_q(&self);
    fn paint_q(&self);
    fn stroke_q(&self);
    fn save_q(&self);
    fn restore_q(&self);
}

impl Quiet for Context {
    fn fill_q(&self) {
        let _ = self.fill();
    }
    fn fill_keep(&self) {
        let _ = self.fill_preserve();
    }
    fn stroke_preserve_q(&self) {
        let _ = self.stroke_preserve();
    }
    fn paint_q(&self) {
        let _ = self.paint();
    }
    fn stroke_q(&self) {
        let _ = self.stroke();
    }
    fn save_q(&self) {
        let _ = self.save();
    }
    fn restore_q(&self) {
        let _ = self.restore();
    }
}

pub(crate) fn rgba(cr: &Context, c: Rgb, a: f64) {
    cr.set_source_rgba(c.0, c.1, c.2, a);
}

pub(crate) fn lin(x0: f64, y0: f64, x1: f64, y1: f64, stops: &[(f64, Rgb, f64)]) -> LinearGradient {
    let g = LinearGradient::new(x0, y0, x1, y1);
    for &(o, c, a) in stops {
        g.add_color_stop_rgba(o, c.0, c.1, c.2, a);
    }
    g
}

pub(crate) fn desat(c: Rgb, t: f64) -> Rgb {
    let l = luma(c);
    mix(c, (l, l, l), t)
}

/// Escala que evita matrices singulares (Cairo deja de dibujar si la CTM no se puede invertir).
pub(crate) fn nz(v: f64) -> f64 {
    if v.abs() < 0.03 { 0.03f64.copysign(if v == 0.0 { 1.0 } else { v }) } else { v }
}

// ---------------------------------------------------------------- escena

pub fn draw_tank(cr: &Context, v: &View) {
    match v.style {
        Style::Realistic => return crate::reef::scene::draw_tank(cr, v),
        Style::Pixel => return crate::pixel::draw_tank(cr, v),
        Style::Sonar => return crate::sonar::draw_tank(cr, v),
        Style::Cartoon => {}
    }
    let t = v.tank;
    let (w, h) = (t.w, t.h);
    cr.save_q();
    cr.rectangle(0.0, 0.0, w, h);
    cr.clip();

    reset_budget();
    let ds = cr.target().device_scale().0.max(1.0);
    water(cr, v);
    rays(cr, v, t.t, t.boil);
    motes(cr, v);
    floor(cr, v, true);
    for (i, d) in t.decor.iter().enumerate() {
        decor(cr, v, d, v.hover == Some(Pick::Decor(i)));
    }

    // Los compañeros de banco del pez señalado se iluminan suavemente.
    if let Some(Pick::Fish(hi)) = v.hover
        && let Some(hf) = t.fish.get(hi) {
            for f in t.fish.iter().filter(|f| f.kh == hf.kh && f.life == Life::Alive) {
                let g = RadialGradient::new(f.x, f.y, 0.0, f.x, f.y, f.len * 0.75);
                g.add_color_stop_rgba(0.0, v.pal.accent.0, v.pal.accent.1, v.pal.accent.2, 0.22);
                g.add_color_stop_rgba(1.0, v.pal.accent.0, v.pal.accent.1, v.pal.accent.2, 0.0);
                let _ = cr.set_source(&g);
                cr.arc(f.x, f.y, f.len * 0.75, 0.0, 2.0 * PI);
                cr.fill_q();
            }
        }
    for &i in &t.order {
        fish(cr, &t.fish[i], v.pal, v.hover == Some(Pick::Fish(i)), ds, v.style);
    }
    bubbles(cr, v);
    surface(cr, v);
    overlays(cr, v);
    labels(cr, v);
    cr.restore_q();
}

/// Caja lógica (x, y, ancho, alto) que contiene una pieza de decoración.
pub fn decor_box(t: &Tank, d: &Decor) -> (f64, f64, f64, f64) {
    let x = d.xf * t.w;
    let base = t.floor_y(x) + 5.0;
    (x - d.size * 0.7 - 6.0, base - d.size * 1.15 - 6.0, d.size * 1.4 + 12.0, d.size * 1.15 + 20.0)
}

thread_local! {
    static CELL: std::cell::Cell<f64> = const { std::cell::Cell::new(3.0) };
}

/// Tamaño del píxel de la escena del estilo Pixel (burbujas, adornos, rayos): depende del tamaño
/// de la pecera, y lo comparten los dos renderizadores.
pub fn cell() -> f64 {
    CELL.with(|c| c.get())
}

pub fn set_cell(scale: f64) {
    CELL.with(|c| c.set((3.0 * scale).round().clamp(2.0, 5.0)));
}

/// Redondea a la rejilla.
pub fn snap(v: f64) -> f64 {
    (v / cell()).round() * cell()
}

pub fn water_colors(v: &View) -> (Rgb, Rgb) {
    let (p, b) = (v.pal, v.tank.boil);
    let top = tone(mix(p.blue, p.cyan, 0.5), 0.34);
    let bot = tone(mix(p.blue, p.dbg, 0.35), 0.07);
    (mix(top, tone(p.orange, 0.40), 0.16 * b), mix(bot, tone(p.red, 0.22), 0.62 * b))
}

fn water(cr: &Context, v: &View) {
    let t = v.tank;
    let (top, bot) = water_colors(v);
    let _ = cr.set_source(lin(0.0, 0.0, 0.0, t.h, &[(0.0, top, 1.0), (1.0, bot, 1.0)]));
    cr.rectangle(0.0, 0.0, t.w, t.h);
    cr.fill_q();
}

pub fn rays(cr: &Context, v: &View, time: f64, boil: f64) {
    let t = v.tank;
    let (top, _) = water_colors(v);
    let col = lighten(top, 0.6);
    let sy = t.surface_y();
    let slant = t.w * 0.16;
    for i in 0..6 {
        let x0 = t.w * (0.06 + 0.18 * i as f64) + (time * 0.15 + i as f64 * 1.7).sin() * t.w * 0.03;
        let bw = t.w * (0.025 + 0.012 * (i as f64 * 2.3).sin().abs());
        let end = t.h * 0.95;
        let _ = cr.set_source(lin(0.0, sy, 0.0, end, &[(0.0, col, 0.15 * (1.0 - 0.5 * boil)), (1.0, col, 0.0)]));
        cr.move_to(x0 - bw, sy);
        cr.line_to(x0 + bw, sy);
        cr.line_to(x0 + bw * 2.4 + slant, end);
        cr.line_to(x0 - bw * 0.4 + slant, end);
        cr.close_path();
        cr.fill_q();
    }
}

/// Nieve marina: partículas que la corriente (carga del sistema) arrastra.
fn motes(cr: &Context, v: &View) {
    let (top, _) = water_colors(v);
    let col = lighten(top, 0.7);
    for m in &v.tank.motes {
        rgba(cr, col, 0.10 + 0.22 * m.z);
        cr.arc(m.x, m.y, 0.7 + 1.4 * m.z, 0.0, 2.0 * PI);
        cr.fill_q();
    }
}

fn sand_color(p: &Palette) -> Rgb {
    mix(p.yellow, p.orange, 0.3)
}

thread_local! {
    static CAUSTIC: cairo::ImageSurface = crate::reef::noise::caustic_tile(256, 31);
}

/// Red de luz que ondula sobre la arena.
pub(crate) fn sand_caustics(cr: &Context, t: &Tank, col: &Rgb) {
    CAUSTIC.with(|tile| {
        cr.save_q();
        cr.move_to(0.0, t.h);
        cr.line_to(0.0, t.floor_y(0.0));
        let mut x = 0.0;
        while x < t.w + 6.0 {
            cr.line_to(x, t.floor_y(x));
            x += 6.0;
        }
        cr.line_to(t.w, t.h);
        cr.close_path();
        cr.clip();
        let pat = cairo::SurfacePattern::create(tile);
        pat.set_extend(cairo::Extend::Repeat);
        let mut m = cairo::Matrix::identity();
        m.translate(-t.t * 9.0, -t.t * 4.0);
        m.scale(1.0 / 3.0, 1.0 / 1.4);
        pat.set_matrix(m);
        let _ = cr.set_source(&pat);
        cr.set_operator(cairo::Operator::Over);
        cr.push_group();
        let _ = cr.paint_with_alpha(0.13);
        let _ = cr.pop_group_to_source();
        let src = cr.source();
        rgba(cr, *col, 1.0);
        let _ = cr.mask(&src);
        cr.restore_q();
    });
}

/// Arena y piedra de aire. Con `dynamic` también el resplandor del hervor (que el renderizador de
/// GPU pinta aparte, con degradados nativos).
pub fn floor(cr: &Context, v: &View, dynamic: bool) {
    let t = v.tank;
    let (w, h, b) = (t.w, t.h, t.boil);
    let sand = sand_color(v.pal);
    let top_c = tone(sand, 0.30);
    let bot_c = tone(sand, 0.09);
    let fy = t.h * 0.915;

    // Resplandor del fondo cuando el agua se calienta.
    if dynamic && b > 0.01 {
        let glow = mix(v.pal.orange, v.pal.yellow, 0.35);
        let gh = h * 0.55;
        let _ = cr.set_source(lin(0.0, fy, 0.0, fy - gh, &[(0.0, glow, 0.75 * b), (1.0, glow, 0.0)]));
        cr.rectangle(0.0, fy - gh, w, gh);
        cr.fill_q();
    }

    cr.move_to(0.0, h);
    cr.line_to(0.0, t.floor_y(0.0));
    let mut x = 0.0;
    while x < w + 6.0 {
        cr.line_to(x, t.floor_y(x));
        x += 6.0;
    }
    cr.line_to(w, h);
    cr.close_path();
    let _ = cr.set_source(lin(0.0, fy - 8.0, 0.0, h, &[(0.0, top_c, 1.0), (1.0, bot_c, 1.0)]));
    cr.fill_keep();
    cr.save_q();
    cr.clip();
    // Granos de arena.
    for i in 0..140u64 {
        let px = unit(i * 2 + 1) * w;
        let py = t.floor_y(px) + unit(i * 2 + 2) * (h - t.floor_y(px));
        rgba(cr, lighten(top_c, 0.5), 0.06 + 0.12 * unit(i + 900));
        cr.arc(px, py, 0.8 + unit(i + 500) * 0.9, 0.0, 2.0 * PI);
        cr.fill_q();
    }
    // Brillo de los respiraderos.
    if dynamic && b > 0.01 {
        for i in 0..5 {
            let vx = t.vent_x(i);
            let r = w * 0.07;
            let g = RadialGradient::new(vx, t.floor_y(vx), 0.0, vx, t.floor_y(vx), r);
            g.add_color_stop_rgba(0.0, 1.0, 0.85, 0.45, 0.55 * b);
            g.add_color_stop_rgba(1.0, 1.0, 0.6, 0.2, 0.0);
            let _ = cr.set_source(&g);
            cr.rectangle(vx - r, t.floor_y(vx) - r, 2.0 * r, 2.0 * r);
            cr.fill_q();
        }
    }
    cr.restore_q();

    // Borde superior de la arena.
    cr.move_to(0.0, t.floor_y(0.0));
    let mut x = 0.0;
    while x < w + 6.0 {
        cr.line_to(x, t.floor_y(x));
        x += 6.0;
    }
    rgba(cr, lighten(top_c, 0.35), 0.35);
    cr.set_line_width(1.5);
    cr.stroke_q();

    // Luz de cáusticas sobre la arena (solo en el estilo estilizado; el pixel ya la pinta a celdas).
    if dynamic && v.style == Style::Cartoon {
        sand_caustics(cr, t, &lighten(top_c, 0.6));
    }

    // Piedra de aire: sus burbujas siguen la escritura a disco.
    let ax = w * 0.92;
    let ay = t.floor_y(ax);
    let s = t.scale();
    cr.save_q();
    cr.translate(ax, ay);
    cr.scale(1.0, 0.55);
    let g = RadialGradient::new(-4.0 * s, -8.0 * s, 1.0, 0.0, -2.0 * s, 16.0 * s);
    g.add_color_stop_rgba(0.0, 0.55, 0.58, 0.62, 1.0);
    g.add_color_stop_rgba(1.0, 0.16, 0.17, 0.20, 1.0);
    let _ = cr.set_source(&g);
    cr.arc(0.0, -2.0 * s, 13.0 * s, PI, 2.0 * PI);
    cr.close_path();
    cr.fill_q();
    cr.restore_q();
}

// ---------------------------------------------------------------- decoración

pub(crate) fn closed_smooth(cr: &Context, pts: &[(f64, f64)]) {
    let n = pts.len();
    let mid = |a: (f64, f64), b: (f64, f64)| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let s = mid(pts[n - 1], pts[0]);
    cr.move_to(s.0, s.1);
    for i in 0..n {
        let c = pts[i];
        let e = mid(pts[i], pts[(i + 1) % n]);
        let p0 = if i == 0 { s } else { mid(pts[i - 1], pts[i]) };
        cr.curve_to(
            p0.0 + 2.0 / 3.0 * (c.0 - p0.0),
            p0.1 + 2.0 / 3.0 * (c.1 - p0.1),
            e.0 + 2.0 / 3.0 * (c.0 - e.0),
            e.1 + 2.0 / 3.0 * (c.1 - e.1),
            e.0,
            e.1,
        );
    }
    cr.close_path();
}

fn blade(cr: &Context, x: f64, y: f64, hgt: f64, wid: f64, sway: f64, col: Rgb) {
    let _ = cr.set_source(lin(0.0, y, 0.0, y - hgt, &[(0.0, darken(col, 0.45), 1.0), (1.0, lighten(col, 0.15), 1.0)]));
    cr.move_to(x - wid, y);
    cr.curve_to(x - wid + sway * 0.2, y - hgt * 0.35, x + sway * 0.7 - wid * 0.4, y - hgt * 0.7, x + sway, y - hgt);
    cr.curve_to(x + sway * 0.7 + wid * 0.4, y - hgt * 0.7, x + wid + sway * 0.2, y - hgt * 0.35, x + wid, y);
    cr.close_path();
    cr.fill_q();
}

#[allow(clippy::too_many_arguments)]
fn coral_branch(cr: &Context, x: f64, y: f64, ang: f64, len: f64, wid: f64, depth: u32, seed: u64, t: f64, col: Rgb) {
    let a = ang + (t * 0.7 + depth as f64 * 0.8 + unit(seed) * 6.0).sin() * 0.04;
    let (nx, ny) = (x + a.cos() * len, y + a.sin() * len);
    rgba(cr, if depth == 0 { lighten(col, 0.3) } else { col }, 0.96);
    cr.set_line_width(wid);
    cr.move_to(x, y);
    cr.line_to(nx, ny);
    cr.stroke_q();
    if depth > 0 {
        let n = 2 + (mix64(seed) % 2) as usize;
        for i in 0..n {
            let spread = (i as f64 / (n - 1) as f64 - 0.5) * 1.1 + (unit(seed + i as u64 * 7) - 0.5) * 0.4;
            coral_branch(cr, nx, ny, a + spread, len * 0.72, wid * 0.72, depth - 1, mix64(seed + i as u64 + 1), t, col);
        }
    }
}

pub fn decor(cr: &Context, v: &View, d: &Decor, hover: bool) {
    let t = v.tank;
    let p = v.pal;
    let x = d.xf * t.w;
    let y = t.floor_y(x) + 5.0;
    let s = d.size;
    let agit = 1.0 + 2.5 * t.boil;
    let tt = t.t * agit;
    cr.save_q();
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    match d.kind {
        0 => {
            let n = 3 + (d.seed % 3) as usize;
            for i in 0..n {
                let k = i as f64 - (n as f64 - 1.0) / 2.0;
                let hg = s * (0.7 + 0.5 * unit(d.seed + i as u64));
                let sway = (tt * 0.9 + unit(d.seed + 40 + i as u64) * 6.0).sin() * s * 0.10 * agit.sqrt();
                let col = mix(p.green, p.cyan, unit(d.seed + 80 + i as u64) * 0.4);
                blade(cr, x + k * s * 0.13, y, hg, s * 0.045, sway + k * s * 0.06, col);
            }
        }
        1 => {
            let col = [p.magenta, p.red, p.orange][(d.seed % 3) as usize];
            let col = mix(col, p.dbg, 0.15);
            let n = 2 + (d.seed >> 8) as usize % 2;
            for i in 0..n {
                let ang = -PI / 2.0 + (i as f64 - (n as f64 - 1.0) / 2.0) * 0.55;
                coral_branch(cr, x + (i as f64 - 0.5) * s * 0.06, y, ang, s * 0.34, s * 0.075, 3, mix64(d.seed + i as u64), tt, col);
            }
        }
        2 => {
            let col = [p.magenta, p.cyan, p.orange][(d.seed % 3) as usize];
            // Base.
            let g = lin(0.0, y - s * 0.18, 0.0, y, &[(0.0, lighten(col, 0.1), 1.0), (1.0, darken(col, 0.6), 1.0)]);
            let _ = cr.set_source(&g);
            cr.save_q();
            cr.translate(x, y);
            cr.scale(1.0, 0.5);
            cr.arc(0.0, 0.0, s * 0.2, PI, 2.0 * PI);
            cr.fill_q();
            cr.restore_q();
            let n = 15;
            for i in 0..n {
                let a = -0.1 * PI - 0.8 * PI * i as f64 / (n - 1) as f64;
                let sw = (tt * 1.1 + i as f64 * 0.9).sin() * s * 0.05 * agit.sqrt();
                let (tx, ty) = (x + a.cos() * s * 0.5 + sw, y + a.sin() * s * 0.78);
                let wid = (s * 0.04).max(1.5);
                rgba(cr, darken(col, 0.25), 0.95);
                cr.set_line_width(wid);
                cr.move_to(x + a.cos() * s * 0.12, y - s * 0.02);
                cr.curve_to(x + a.cos() * s * 0.25 + sw * 0.3, y + a.sin() * s * 0.3, x + a.cos() * s * 0.4 + sw * 0.7, y + a.sin() * s * 0.55, tx, ty);
                cr.stroke_q();
                rgba(cr, lighten(col, 0.45), 0.95);
                cr.arc(tx, ty, wid * 0.85, 0.0, 2.0 * PI);
                cr.fill_q();
            }
        }
        _ => {
            let n = 9;
            let pts: Vec<(f64, f64)> = (0..n)
                .map(|i| {
                    let a = PI + PI * i as f64 / (n - 1) as f64;
                    let r = 1.0 + (unit(d.seed + i as u64) - 0.5) * 0.35;
                    (x + a.cos() * s * 0.5 * r, y + a.sin() * s * 0.42 * r + 4.0)
                })
                .collect();
            let col = mix(p.dfg, p.bg, 0.35);
            let _ = cr.set_source(lin(0.0, y - s * 0.45, 0.0, y, &[(0.0, lighten(col, 0.25), 1.0), (1.0, darken(col, 0.55), 1.0)]));
            closed_smooth(cr, &pts);
            cr.fill_keep();
            rgba(cr, (0.0, 0.0, 0.0), 0.35);
            cr.set_line_width(1.0);
            cr.stroke_q();
        }
    }
    if hover {
        rgba(cr, p.accent, 0.5);
        cr.set_line_width(1.5);
        cr.rectangle(x - s * 0.4, y - s, s * 0.8, s + 6.0);
        cr.stroke_q();
    }
    cr.restore_q();
}

// ---------------------------------------------------------------- peces

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TailKind {
    Forked,
    Paddle,
    Flowing,
    Crescent,
}

pub(crate) struct FinDef {
    pub u0: f64,
    pub u1: f64,
    pub h: f64,
    pub lean: f64,
    pub flow: f64,
}

#[allow(dead_code)]
pub(crate) struct Species {
    pub ratio: f64,
    pub tail_frac: f64,
    pub tail: TailKind,
    pub spread: f64,
    pub prof: &'static [(f64, f64)],
    pub dorsal: FinDef,
    pub anal: FinDef,
    pub pect: f64,
    pub eye: f64,
}

const fn fin(u0: f64, u1: f64, h: f64, lean: f64, flow: f64) -> FinDef {
    FinDef { u0, u1, h, lean, flow }
}

pub(crate) const SPECIES_DEFS: [Species; 6] = [
    // Tropical clásico
    Species {
        ratio: 0.27,
        tail_frac: 0.24,
        tail: TailKind::Forked,
        spread: 0.38,
        prof: &[(0.0, 0.0), (0.04, 0.34), (0.14, 0.70), (0.34, 0.98), (0.55, 0.88), (0.78, 0.52), (1.0, 0.26)],
        dorsal: fin(0.26, 0.62, 0.55, 0.5, 0.1),
        anal: fin(0.56, 0.80, 0.35, 0.4, 0.1),
        pect: 0.14,
        eye: 1.0,
    },
    // Esbelto
    Species {
        ratio: 0.15,
        tail_frac: 0.22,
        tail: TailKind::Forked,
        spread: 0.42,
        prof: &[(0.0, 0.0), (0.05, 0.42), (0.2, 0.78), (0.45, 1.0), (0.7, 0.80), (0.9, 0.42), (1.0, 0.28)],
        dorsal: fin(0.55, 0.74, 0.7, 0.7, 0.05),
        anal: fin(0.58, 0.76, 0.5, 0.6, 0.05),
        pect: 0.10,
        eye: 0.9,
    },
    // Disco (tipo escalar)
    Species {
        ratio: 0.40,
        tail_frac: 0.20,
        tail: TailKind::Forked,
        spread: 0.30,
        prof: &[(0.0, 0.0), (0.05, 0.40), (0.2, 0.80), (0.42, 1.0), (0.62, 0.82), (0.82, 0.48), (1.0, 0.20)],
        dorsal: fin(0.28, 0.82, 0.95, 1.3, 0.2),
        anal: fin(0.36, 0.84, 0.85, 1.3, 0.2),
        pect: 0.17,
        eye: 1.15,
    },
    // Redondo
    Species {
        ratio: 0.34,
        tail_frac: 0.20,
        tail: TailKind::Paddle,
        spread: 0.55,
        prof: &[(0.0, 0.0), (0.05, 0.5), (0.2, 0.93), (0.45, 1.0), (0.7, 0.82), (0.9, 0.40), (1.0, 0.22)],
        dorsal: fin(0.52, 0.72, 0.35, 0.4, 0.1),
        anal: fin(0.56, 0.74, 0.3, 0.4, 0.1),
        pect: 0.20,
        eye: 1.25,
    },
    // De velos (tipo betta)
    Species {
        ratio: 0.20,
        tail_frac: 0.40,
        tail: TailKind::Flowing,
        spread: 0.75,
        prof: &[(0.0, 0.0), (0.05, 0.45), (0.2, 0.85), (0.42, 1.0), (0.65, 0.78), (0.85, 0.42), (1.0, 0.22)],
        dorsal: fin(0.30, 0.72, 0.9, 0.9, 0.7),
        anal: fin(0.34, 0.82, 1.3, 1.1, 0.8),
        pect: 0.15,
        eye: 1.0,
    },
    // Torpedo (tipo atún)
    Species {
        ratio: 0.21,
        tail_frac: 0.26,
        tail: TailKind::Crescent,
        spread: 0.6,
        prof: &[(0.0, 0.0), (0.05, 0.40), (0.22, 0.86), (0.45, 1.0), (0.70, 0.70), (0.90, 0.30), (1.0, 0.16)],
        dorsal: fin(0.32, 0.48, 0.55, 1.0, 0.1),
        anal: fin(0.62, 0.76, 0.30, 0.8, 0.1),
        pect: 0.13,
        eye: 0.9,
    },
];

pub(crate) fn catmull(p: &[(f64, f64)], u: f64) -> f64 {
    let n = p.len();
    let mut i = 0;
    while i + 2 < n && u > p[i + 1].0 {
        i += 1;
    }
    let (u1, y1) = p[i];
    let (u2, y2) = p[i + 1];
    let y0 = if i > 0 { p[i - 1].1 } else { 2.0 * y1 - y2 };
    let y3 = if i + 2 < n { p[i + 2].1 } else { 2.0 * y2 - y1 };
    let t = ((u - u1) / (u2 - u1)).clamp(0.0, 1.0);
    0.5 * (2.0 * y1
        + (-y0 + y2) * t
        + (2.0 * y0 - 5.0 * y1 + 4.0 * y2 - y3) * t * t
        + (-y0 + 3.0 * y1 - 3.0 * y2 + y3) * t * t * t)
}

pub(crate) const NS: usize = 40;

/// Columna del pez: posiciones a lo largo del cuerpo (de la nariz a la base de la cola),
/// normales hacia arriba y semialtura del cuerpo en cada punto.
pub(crate) struct Spine {
    pub n: usize,
    pub u: [f64; NS + 1],
    pub p: [(f64, f64); NS + 1],
    pub nu: [(f64, f64); NS + 1],
    pub h: [f64; NS + 1],
}

impl Spine {
    pub(crate) fn new(def: &Species, len: f64, phase: f64, amp: f64, n: usize) -> Spine {
        Spine::build(def.prof, def.ratio, def.tail_frac, len, phase, amp, n)
    }

    /// Columna de un pez de longitud `len`: perfil `prof`, semialtura `ratio * len`, la cola ocupa
    /// `tail_frac` de la longitud y el cuerpo ondula con `phase` y `amp`.
    pub(crate) fn build(prof: &[(f64, f64)], ratio: f64, tail_frac: f64, len: f64, phase: f64, amp: f64, n: usize) -> Spine {
        let lb = len * (1.0 - tail_frac);
        let hh = ratio * len;
        let mut sp = Spine { n, u: [0.0; NS + 1], p: [(0.0, 0.0); NS + 1], nu: [(0.0, -1.0); NS + 1], h: [0.0; NS + 1] };
        for i in 0..=n {
            let u = (i as f64 / n as f64).powf(1.5);
            sp.u[i] = u;
            sp.p[i] = (len * 0.5 - u * lb, amp * lb * (0.012 + 0.15 * u * u) * (phase - 4.6 * u).sin());
            sp.h[i] = catmull(prof, u).max(0.0) * hh;
        }
        for i in 0..=n {
            let a = sp.p[i.saturating_sub(1)];
            let b = sp.p[(i + 1).min(n)];
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l = dx.hypot(dy).max(1e-6);
            sp.nu[i] = (-dy / l, dx / l);
        }
        sp
    }

    /// Punto, normal y semialtura interpolados en `u`.
    pub(crate) fn at(&self, u: f64) -> ((f64, f64), (f64, f64), f64) {
        let mut i = 0;
        while i + 1 < self.n && self.u[i + 1] < u {
            i += 1;
        }
        let (u0, u1) = (self.u[i], self.u[i + 1]);
        let k = if u1 > u0 { ((u - u0) / (u1 - u0)).clamp(0.0, 1.0) } else { 0.0 };
        let l = |a: (f64, f64), b: (f64, f64)| (a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k);
        (l(self.p[i], self.p[i + 1]), l(self.nu[i], self.nu[i + 1]), self.h[i] + (self.h[i + 1] - self.h[i]) * k)
    }
}

pub(crate) fn trace_body(cr: &Context, sp: &Spine) {
    for i in 0..=sp.n {
        let (p, nu, h) = (sp.p[i], sp.nu[i], sp.h[i]);
        let q = (p.0 + nu.0 * h, p.1 + nu.1 * h);
        if i == 0 {
            cr.move_to(q.0, q.1);
        } else {
            cr.line_to(q.0, q.1);
        }
    }
    for i in (0..=sp.n).rev() {
        let (p, nu, h) = (sp.p[i], sp.nu[i], sp.h[i] * 1.04);
        cr.line_to(p.0 - nu.0 * h, p.1 - nu.1 * h);
    }
    cr.close_path();
}

/// Cómo se pintan aletas y cola: relleno (estilizado) o contorno con halo (sonar).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Paint {
    Fill,
    Glow,
}

/// Contorno luminoso: tres pasadas (halo ancho, medio y núcleo fino) sobre la ruta actual.
pub(crate) fn glow_stroke(cr: &Context, col: Rgb, len: f64) {
    let k = (len / 60.0).clamp(0.5, 1.6);
    for (wd, a, c) in [(6.0 * k, 0.10, col), (2.6 * k, 0.28, col), (1.0, 1.0, lighten(col, 0.45))] {
        rgba(cr, c, a);
        cr.set_line_width(wd);
        cr.stroke_preserve_q();
    }
    cr.new_path();
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_fin(cr: &Context, sp: &Spine, fd: &FinDef, up: bool, hh: f64, col: Rgb, phase: f64, detail: u8, paint: Paint) {
    let m = if detail == 0 { 6 } else { 12 };
    let mut base = [(0.0, 0.0); 13];
    let mut tip = [(0.0, 0.0); 13];
    let sgn = if up { 1.0 } else { -1.0 };
    for k in 0..=m {
        let s = k as f64 / m as f64;
        let (p, nu, h) = sp.at(fd.u0 + (fd.u1 - fd.u0) * s);
        let on = (nu.0 * sgn, nu.1 * sgn);
        let tl = (nu.1, -nu.0); // hacia la cola
        let dome = (1.0 - (2.0 * s.powf(0.8) - 1.0).powi(2)).max(0.0).powf(0.7);
        let ht = fd.h * hh * dome;
        let b = (p.0 + on.0 * h * 0.85, p.1 + on.1 * h * 0.85);
        let along = fd.lean * ht * 0.55 + (phase - 5.0 * s).sin() * fd.flow * ht * 0.35;
        base[k] = b;
        tip[k] = (b.0 + on.0 * ht + tl.0 * along, b.1 + on.1 * ht + tl.1 * along);
    }
    cr.move_to(base[0].0, base[0].1);
    for k in 1..=m {
        cr.line_to(tip[k].0, tip[k].1);
    }
    for k in (0..=m).rev() {
        cr.line_to(base[k].0, base[k].1);
    }
    cr.close_path();
    if paint == Paint::Glow {
        glow_stroke(cr, col, hh * 4.0);
        return;
    }
    rgba(cr, col, 0.80);
    cr.fill_q();
    if detail > 0 {
        rgba(cr, darken(col, 0.4), 0.25);
        cr.set_line_width(0.8);
        let mut k = 1;
        while k < m {
            cr.move_to(base[k].0, base[k].1);
            cr.line_to(tip[k].0, tip[k].1);
            k += 2;
        }
        cr.stroke_q();
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_tail(cr: &Context, sp: &Spine, def: &Species, len: f64, col: Rgb, phase: f64, amp: f64, detail: u8, paint: Paint) {
    let n = sp.n;
    let (pe, pp) = (sp.p[n], sp.p[n - 1]);
    let ang = (pe.1 - pp.1).atan2(pe.0 - pp.0);
    let swing = 0.5 * amp * (phase - 4.6 - 0.9).sin();
    let tl = def.tail_frac * len;
    let s = tl * def.spread;
    let w0 = (sp.h[n] * 0.9).max(0.6);
    cr.save_q();
    cr.translate(pe.0, pe.1);
    cr.rotate(ang + swing);
    match def.tail {
        TailKind::Forked => {
            cr.move_to(0.0, -w0);
            cr.curve_to(tl * 0.35, -w0 * 1.2, tl * 0.8, -s * 0.9, tl, -s);
            cr.curve_to(tl * 0.78, -s * 0.4, tl * 0.62, -s * 0.12, tl * 0.58, 0.0);
            cr.curve_to(tl * 0.62, s * 0.12, tl * 0.78, s * 0.4, tl, s);
            cr.curve_to(tl * 0.8, s * 0.9, tl * 0.35, w0 * 1.2, 0.0, w0);
        }
        TailKind::Paddle => {
            cr.move_to(0.0, -w0);
            cr.curve_to(tl * 0.45, -s, tl * 1.05, -s * 0.75, tl, 0.0);
            cr.curve_to(tl * 1.05, s * 0.75, tl * 0.45, s, 0.0, w0);
        }
        TailKind::Crescent => {
            cr.move_to(0.0, -w0);
            cr.curve_to(tl * 0.25, -w0 * 1.1, tl * 0.6, -s * 0.9, tl, -s);
            cr.curve_to(tl * 0.66, -s * 0.55, tl * 0.5, -s * 0.15, tl * 0.40, 0.0);
            cr.curve_to(tl * 0.5, s * 0.15, tl * 0.66, s * 0.55, tl, s);
            cr.curve_to(tl * 0.6, s * 0.9, tl * 0.25, w0 * 1.1, 0.0, w0);
        }
        TailKind::Flowing => {
            let m = 10;
            for k in 0..=m {
                let q = k as f64 / m as f64;
                let y = -(w0 + (s - w0) * q.powf(0.8));
                let x = tl * q * 0.92;
                if k == 0 {
                    cr.move_to(x, y);
                } else {
                    cr.line_to(x, y);
                }
            }
            for k in 0..=14 {
                let vv = -1.0 + 2.0 * k as f64 / 14.0;
                let x = tl * (0.86 + 0.14 * (vv * PI * 0.5).cos()) + tl * 0.05 * (vv * 7.0 + phase).sin();
                cr.line_to(x, vv * s);
            }
            for k in (0..=m).rev() {
                let q = k as f64 / m as f64;
                cr.line_to(tl * q * 0.92, w0 + (s - w0) * q.powf(0.8));
            }
        }
    }
    cr.close_path();
    if paint == Paint::Glow {
        glow_stroke(cr, col, len);
        cr.restore_q();
        return;
    }
    let end_a = if def.tail == TailKind::Flowing { 0.3 } else { 0.6 };
    let _ = cr.set_source(lin(0.0, 0.0, tl, 0.0, &[(0.0, darken(col, 0.1), 0.95), (1.0, lighten(col, 0.3), end_a)]));
    cr.fill_keep();
    rgba(cr, darken(col, 0.5), 0.35);
    cr.set_line_width(0.8);
    cr.stroke_q();
    if detail > 0 {
        rgba(cr, darken(col, 0.5), 0.22);
        cr.set_line_width(0.7);
        for k in 0..5 {
            let yy = s * (-0.75 + 1.5 * k as f64 / 4.0);
            cr.move_to(0.0, yy * 0.15);
            cr.line_to(tl * 0.92, yy);
        }
        cr.stroke_q();
    }
    cr.restore_q();
}

/// Todo lo que define el dibujo de un pez en reposo (lo que se cachea como sprite).
pub(crate) struct FishArt {
    pub look: Look,
    pub len: f64,
    pub phase: f64,
    pub amp: f64,
    pub dying: bool,
    pub style: Style,
}

/// Dibuja un pez completo mirando a +x y centrado en el origen, sin transformaciones ni alfa.
fn paint_fish(cr: &Context, a: &FishArt, pal: &Palette) {
    if a.style == Style::Realistic {
        crate::reef::fish::paint(cr, a, pal);
        return;
    }
    if a.style == Style::Sonar {
        crate::sonar::paint(cr, a, pal);
        return;
    }
    let def = &SPECIES_DEFS[a.look.species];
    let len = a.len;
    let detail: u8 = if len < 32.0 { 0 } else if len < 72.0 { 1 } else { 2 };
    let n = match detail {
        0 => 10,
        1 => 18,
        _ => 28,
    };

    let mut c1 = pal.fish[a.look.c1];
    c1 = if a.look.tint > 0.0 { lighten(c1, a.look.tint * 0.16) } else { darken(c1, -a.look.tint * 0.16) };
    let mut c2 = pal.fish[a.look.c2];
    if a.dying {
        c1 = desat(c1, 0.5);
        c2 = desat(c2, 0.5);
    }
    let hh = def.ratio * len;
    let sp = Spine::new(def, len, a.phase, a.amp, n);

    // Cola y aletas dorsal/anal van detrás del cuerpo.
    let fin_col = lighten(mix(c2, c1, 0.25), 0.08);
    draw_tail(cr, &sp, def, len, fin_col, a.phase, a.amp, detail, Paint::Fill);
    draw_fin(cr, &sp, &def.dorsal, true, hh, fin_col, a.phase, detail, Paint::Fill);
    draw_fin(cr, &sp, &def.anal, false, hh, fin_col, a.phase, detail, Paint::Fill);

    // Cuerpo: contraluz (lomo oscuro, vientre claro), tono de la cola y brillo del lomo.
    cr.new_path();
    trace_body(cr, &sp);
    let path = cr.copy_path();
    let repath = |cr: &Context| {
        cr.new_path();
        if let Ok(p) = &path {
            cr.append_path(p);
        }
    };
    let body = lin(
        0.0,
        -hh,
        0.0,
        hh * 1.05,
        &[
            (0.0, darken(c1, 0.45), 1.0),
            (0.32, c1, 1.0),
            (0.64, lighten(c1, 0.25), 1.0),
            (1.0, lighten(c1, 0.70), 1.0),
        ],
    );
    let _ = cr.set_source(&body);
    cr.fill_q();
    let xn = len * 0.5;
    let xt = len * 0.5 - len * (1.0 - def.tail_frac);
    let _ = cr.set_source(lin(xn, 0.0, xt, 0.0, &[(0.0, c2, 0.0), (0.45, c2, 0.0), (1.0, c2, 0.62)]));
    repath(cr);
    cr.fill_q();
    if detail > 0 {
        pattern(cr, &a.look, &sp, hh, c2, detail);
    }
    let _ = cr.set_source(lin(0.0, -hh, 0.0, -hh * 0.15, &[(0.0, (1.0, 1.0, 1.0), 0.30), (1.0, (1.0, 1.0, 1.0), 0.0)]));
    repath(cr);
    cr.fill_q();
    repath(cr);
    rgba(cr, darken(c1, 0.6), 0.75);
    cr.set_line_width((len * 0.008).max(0.8));
    cr.stroke_q();

    // Aleta pectoral, agallas, boca y ojo.
    if detail > 0 {
        let (p, nu, h) = sp.at(0.27);
        cr.save_q();
        cr.translate(p.0 - nu.0 * h * 0.15, p.1 - nu.1 * h * 0.15);
        let pl = def.pect * len;
        cr.rotate(-0.35 + 0.35 * (a.phase + 1.0).sin());
        cr.move_to(0.0, 0.0);
        cr.curve_to(-0.4 * pl, -0.28 * pl, -0.95 * pl, -0.12 * pl, -pl, 0.10 * pl);
        cr.curve_to(-0.7 * pl, 0.30 * pl, -0.25 * pl, 0.22 * pl, 0.0, 0.0);
        rgba(cr, lighten(fin_col, 0.15), 0.72);
        cr.fill_keep();
        rgba(cr, darken(c1, 0.5), 0.4);
        cr.set_line_width(0.8);
        cr.stroke_q();
        cr.restore_q();

        let (p, nu, h) = sp.at(0.22);
        rgba(cr, darken(c1, 0.6), 0.28);
        cr.set_line_width((len * 0.008).max(0.8));
        cr.move_to(p.0 + nu.0 * h * 0.85, p.1 + nu.1 * h * 0.85);
        cr.curve_to(p.0 + nu.0 * h * 0.2 + h * 0.35, p.1 + nu.1 * h * 0.2, p.0 - nu.0 * h * 0.2 + h * 0.35, p.1 - nu.1 * h * 0.2, p.0 - nu.0 * h * 0.85, p.1 - nu.1 * h * 0.85);
        cr.stroke_q();

        let nose = sp.p[0];
        rgba(cr, darken(c1, 0.7), 0.6);
        cr.set_line_width((len * 0.009).max(0.9));
        cr.move_to(nose.0 - len * 0.004, nose.1 + hh * 0.05);
        cr.curve_to(nose.0 - len * 0.03, nose.1 + hh * 0.17, nose.0 - len * 0.06, nose.1 + hh * 0.17, nose.0 - len * 0.085, nose.1 + hh * 0.08);
        cr.stroke_q();
    }

    let (pe, nu, h) = sp.at(0.12);
    let (ex, ey) = (pe.0 + nu.0 * h * 0.30, pe.1 + nu.1 * h * 0.30);
    let r = (len * 0.036 * def.eye + 0.6).clamp(1.3, 10.0 * def.eye);
    if detail == 0 {
        rgba(cr, (0.04, 0.04, 0.06), 0.95);
        cr.arc(ex, ey, r, 0.0, 2.0 * PI);
        cr.fill_q();
    } else if a.dying {
        rgba(cr, (0.05, 0.05, 0.07), 0.95);
        cr.set_line_width((r * 0.35).max(1.0));
        cr.move_to(ex - r * 0.7, ey - r * 0.7);
        cr.line_to(ex + r * 0.7, ey + r * 0.7);
        cr.move_to(ex + r * 0.7, ey - r * 0.7);
        cr.line_to(ex - r * 0.7, ey + r * 0.7);
        cr.stroke_q();
    } else {
        rgba(cr, (0.03, 0.03, 0.05), 0.55);
        cr.arc(ex, ey, r + 0.7, 0.0, 2.0 * PI);
        cr.fill_q();
        rgba(cr, (0.97, 0.97, 0.95), 1.0);
        cr.arc(ex, ey, r, 0.0, 2.0 * PI);
        cr.fill_q();
        rgba(cr, lighten(c2, 0.2), 1.0);
        cr.arc(ex + r * 0.12, ey, r * 0.74, 0.0, 2.0 * PI);
        cr.fill_q();
        rgba(cr, (0.02, 0.02, 0.04), 1.0);
        cr.arc(ex + r * 0.2, ey, r * 0.5, 0.0, 2.0 * PI);
        cr.fill_q();
        rgba(cr, (1.0, 1.0, 1.0), 0.95);
        cr.arc(ex + r * 0.05, ey - r * 0.28, r * 0.17, 0.0, 2.0 * PI);
        cr.fill_q();
    }
}

// ---------------------------------------------------------------- caché de sprites

/// Dibujar cada pez en vectores cuesta ~0,3 ms; los peces se parecen mucho entre fotogramas
/// (misma especie, colores y tamaño, y solo cambia la fase de la cola), así que cada variante se
/// renderiza una vez a un sprite y luego solo se pinta transformado (voltear, girar, escalar).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SKey {
    pub species: u8,
    pub pattern: u8,
    pub c1: u8,
    pub c2: u8,
    pub tint: i8,
    pub size: i16,
    pub phase: u8,
    pub amp: u8,
    pub dying: bool,
    pub style: u8,
}

pub struct Sprite {
    pub surf: cairo::ImageSurface,
    /// Origen del pez dentro del sprite (px lógicos).
    pub ox: f64,
    pub oy: f64,
    /// Longitud con la que se renderizó.
    pub len: f64,
}

#[derive(Default)]
struct SpriteCache {
    map: HashMap<SKey, Sprite>,
    bytes: usize,
    pal: String,
    ds: f64,
}

thread_local! {
    static SPRITES: RefCell<SpriteCache> = RefCell::new(SpriteCache::default());
}

const AMPS: [f64; 4] = [0.45, 0.65, 0.85, 0.12];
const SIZE_STEP: f64 = 1.12;
const MAX_SPRITE_BYTES: usize = 160 << 20;

pub fn nphases(len: f64) -> usize {
    if len > 100.0 { 8 } else { 12 }
}

pub fn sprite_key(f: &Fish, style: Style) -> (SKey, f64) {
    let dying = matches!(f.life, Life::Dying(_));
    if style == Style::Pixel {
        // Sprite dibujado a mano: factor entero y uno de tres fotogramas de cola.
        let k = crate::pixel::scale_of(&f.look, f.len);
        let key = SKey {
            species: crate::pixel::species_of(&f.look) as u8,
            pattern: 0,
            c1: f.look.c1 as u8,
            c2: f.look.c2 as u8,
            tint: (f.look.tint * 2.0).round() as i8,
            size: k as i16,
            phase: crate::pixel::frame_of(f) as u8,
            amp: 0,
            dying,
            style: 1,
        };
        return (key, f.len);
    }
    let size = ((f.len / 12.0).ln() / SIZE_STEP.ln()).round() as i16;
    let blen = 12.0 * SIZE_STEP.powi(size as i32);
    let np = nphases(f.len);
    let ph = ((f.phase / (2.0 * PI)).rem_euclid(1.0) * np as f64) as usize % np;
    let amp = if dying {
        3
    } else if f.amp < 0.55 {
        0
    } else if f.amp < 0.75 {
        1
    } else {
        2
    };
    (
        SKey {
            species: if style == Style::Realistic { f.look.real } else { f.look.species } as u8,
            pattern: f.look.pattern as u8,
            c1: f.look.c1 as u8,
            c2: f.look.c2 as u8,
            tint: (f.look.tint * 2.0).round() as i8,
            size,
            phase: ph as u8,
            amp,
            dying,
            style: match style {
                Style::Realistic => 2,
                Style::Sonar => 3,
                _ => 0,
            },
        },
        blen,
    )
}

/// Pinta el pez en `cr` (ya en coordenadas de la pecera) usando el sprite cacheado.
fn blit_fish(cr: &Context, f: &Fish, pal: &Palette, ds: f64, style: Style) {
    let (key, blen) = sprite_key(f, style);
    SPRITES.with(|c| {
        let mut c = c.borrow_mut();
        if c.pal != pal.raw || (c.ds - ds).abs() > 0.01 {
            c.map.clear();
            c.bytes = 0;
            c.pal = pal.raw.clone();
            c.ds = ds;
        }
        if c.bytes > MAX_SPRITE_BYTES {
            c.map.clear();
            c.bytes = 0;
        }
        if key.style == 1 {
            let k = key.size as f64;
            let spr = c.map.entry(key).or_insert_with(|| crate::pixel::render(&f.look, key.phase as usize, k, pal, key.dying));
            crate::pixel::draw_sprite(cr, f, spr, k, f.alpha.clamp(0.0, 1.0));
            return;
        }
        if !c.map.contains_key(&key) {
            let np = nphases(f.len);
            // Con el sprite de una fase vecina basta; solo se renderiza si no hay ninguna.
            let near = (1..=np / 2).find_map(|d| {
                [(key.phase as usize + d) % np, (key.phase as usize + np - d) % np]
                    .into_iter()
                    .find(|&p| c.map.contains_key(&SKey { phase: p as u8, ..key }))
                    .map(|p| SKey { phase: p as u8, ..key })
            });
            match near {
                Some(k) if c.map.len() > 1 && !budget_left() => {
                    draw_sprite(cr, f, &c.map[&k], style);
                    return;
                }
                _ => {
                    let spr = render_sprite(f, &key, blen, pal, ds);
                    c.bytes += (spr.surf.width() * spr.surf.height() * 4) as usize;
                    c.map.insert(key, spr);
                }
            }
        }
        let _ = blen;
        draw_sprite(cr, f, &c.map[&key], style);
    });
}

thread_local! {
    static BUDGET: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Cuántos sprites nuevos se admiten por fotograma antes de reutilizar los de fases vecinas.
fn budget_left() -> bool {
    BUDGET.with(|b| {
        if b.get() == 0 {
            return false;
        }
        b.set(b.get() - 1);
        true
    })
}

pub(crate) fn reset_budget() {
    BUDGET.with(|b| b.set(6));
}

pub fn render_sprite(f: &Fish, key: &SKey, blen: f64, pal: &Palette, ds: f64) -> Sprite {
    let np = nphases(f.len);
    let style = match key.style {
        2 => Style::Realistic,
        1 => Style::Pixel,
        3 => Style::Sonar,
        _ => Style::Cartoon,
    };
    let art = FishArt {
        look: f.look,
        len: blen,
        phase: (key.phase as f64 + 0.5) / np as f64 * 2.0 * PI,
        amp: AMPS[key.amp as usize],
        dying: key.dying,
        style,
    };
    // Caja que contiene cuerpo, aletas y la ondulación de la cola.
    if style == Style::Pixel {
        return crate::pixel::render(&f.look, key.phase as usize, key.size as f64, pal, key.dying);
    }
    let (half_w, half_h) = if key.style == 2 {
        crate::reef::fish::extent(&f.look, blen, art.amp)
    } else if key.style == 3 {
        crate::sonar::extent(f.look.species, blen, art.amp)
    } else {
        let def = &SPECIES_DEFS[f.look.species];
        let hh = def.ratio * blen;
        let fin_h = def.dorsal.h.max(def.anal.h) * hh * (1.0 + def.dorsal.lean.max(def.anal.lean) * 0.4);
        ((blen * 0.5 + 6.0), (hh * 1.1 + fin_h + blen * 0.17 * art.amp + 6.0).max(blen * 0.35))
    };
    let (wl, hl) = (half_w * 2.0, half_h * 2.0);
    let (wp, hp) = ((wl * ds).ceil().max(2.0) as i32, (hl * ds).ceil().max(2.0) as i32);
    let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, wp, hp).expect("sprite");
    surf.set_device_scale(ds, ds);
    {
        let cx = Context::new(&surf).expect("cairo");
        cx.translate(half_w, half_h);
        paint_fish(&cx, &art, pal);
    }
    Sprite { surf, ox: half_w, oy: half_h, len: blen }
}

/// Atenuación por distancia: en el estilo realista los peces pequeños se funden con el agua.
pub fn depth_alpha(f: &Fish, style: Style) -> f64 {
    match style {
        Style::Cartoon | Style::Pixel => 1.0,
        Style::Realistic | Style::Sonar => {
            let t = ((f.len - 16.0) / 74.0).clamp(0.0, 1.0);
            0.62 + 0.38 * t * t * (3.0 - 2.0 * t)
        }
    }
}

fn draw_sprite(cr: &Context, f: &Fish, s: &Sprite, style: Style) {
    cr.save_q();
    cr.translate(f.x, f.y);
    cr.scale(nz(f.face), nz(f.flipv));
    cr.rotate(-f.pitch);
    let k = f.len / s.len;
    cr.scale(k, k);
    let _ = cr.set_source_surface(&s.surf, -s.ox, -s.oy);
    let _ = cr.paint_with_alpha((f.alpha * depth_alpha(f, style)).clamp(0.0, 1.0));
    cr.restore_q();
}

/// Semialtura del cuerpo del pez en px.
pub fn body_half_height(f: &Fish, style: Style) -> f64 {
    match style {
        Style::Cartoon => SPECIES_DEFS[f.look.species].ratio * f.len,
        Style::Pixel => f.len * 0.32,
        Style::Sonar => SPECIES_DEFS[f.look.species].ratio * f.len,
        Style::Realistic => crate::reef::species::SPECIES[f.look.real].ratio * f.len,
    }
}

pub fn fish(cr: &Context, f: &Fish, pal: &Palette, hl: bool, ds: f64, style: Style) {
    if f.alpha <= 0.01 || f.len < 4.0 {
        return;
    }
    blit_fish(cr, f, pal, ds, style);
    if hl {
        // Anillo de selección alrededor del pez señalado.
        let hh = body_half_height(f, style);
        cr.save_q();
        cr.translate(f.x, f.y);
        cr.rotate(-f.pitch * f.face.signum());
        cr.scale(1.0, ((hh * 1.5 + 6.0) / (f.len * 0.55 + 6.0)).max(0.3));
        cr.arc(0.0, 0.0, f.len * 0.55 + 6.0, 0.0, 2.0 * PI);
        cr.restore_q();
        rgba(cr, pal.accent, 0.95);
        cr.set_line_width(2.0);
        cr.stroke_q();
    }
}

fn pattern(cr: &Context, look: &Look, sp: &Spine, hh: f64, c2: Rgb, detail: u8) {
    let seed = look.seed;
    match look.pattern {
        Pattern::Plain => {}
        Pattern::Bands => {
            let nb = 2 + (seed % 2) as usize;
            for k in 0..nb {
                let ub = 0.2 + 0.2 * k as f64 + 0.18 * unit(seed + k as u64 * 3);
                let wb = 0.04 + 0.02 * unit(seed + 50 + k as u64);
                for (grow, a) in [(0.0, 0.85)] {
                    let w2 = wb + grow * 0.02;
                    let m = 6;
                    for i in 0..=m {
                        let (p, nu, h) = sp.at(ub - w2 + 2.0 * w2 * i as f64 / m as f64);
                        let q = (p.0 + nu.0 * h, p.1 + nu.1 * h);
                        if i == 0 {
                            cr.move_to(q.0, q.1);
                        } else {
                            cr.line_to(q.0, q.1);
                        }
                    }
                    for i in (0..=m).rev() {
                        let (p, nu, h) = sp.at(ub - w2 + 2.0 * w2 * i as f64 / m as f64);
                        cr.line_to(p.0 - nu.0 * h * 1.04, p.1 - nu.1 * h * 1.04);
                    }
                    cr.close_path();
                    rgba(cr, c2, a);
                    cr.fill_q();
                }
            }
        }
        Pattern::Spots => {
            let ns = 5 + (seed % 5) as usize;
            for k in 0..ns {
                let u = 0.2 + 0.65 * unit(seed + k as u64 * 7);
                let vv = (unit(seed + 100 + k as u64 * 13) * 2.0 - 1.0) * 0.55;
                let (p, nu, h) = sp.at(u);
                rgba(cr, darken(c2, 0.2), 0.62);
                cr.arc(p.0 + nu.0 * h * vv, p.1 + nu.1 * h * vv, hh * (0.06 + 0.09 * unit(seed + 200 + k as u64)), 0.0, 2.0 * PI);
                cr.fill_q();
            }
        }
        Pattern::Stripe => {
            for (vv, wid, col, a) in [(0.0, hh * 0.22, c2, 0.66), (0.42, hh * 0.05, (1.0, 1.0, 1.0), 0.45)] {
                let m = 14;
                for i in 0..=m {
                    let (p, nu, h) = sp.at(0.12 + 0.85 * i as f64 / m as f64);
                    if i == 0 {
                        cr.move_to(p.0 + nu.0 * h * vv, p.1 + nu.1 * h * vv);
                    } else {
                        cr.line_to(p.0 + nu.0 * h * vv, p.1 + nu.1 * h * vv);
                    }
                }
                rgba(cr, col, a);
                cr.set_line_width(wid);
                cr.set_line_cap(cairo::LineCap::Round);
                cr.stroke_q();
            }
        }
    }
    // Escamas (solo en peces grandes).
    if detail == 2 {
        let sc = hh * 0.17;
        for iu in 0..10 {
            for iv in 0..5 {
                let u = 0.18 + 0.075 * iu as f64;
                let vv = -0.62 + 0.28 * iv as f64 + if iu % 2 == 1 { 0.14 } else { 0.0 };
                let (p, nu, h) = sp.at(u);
                let (cx, cy) = (p.0 + nu.0 * h * vv, p.1 + nu.1 * h * vv);
                cr.new_sub_path();
                cr.arc(cx + sc * 0.35, cy, sc, 0.55 * PI, 1.45 * PI);
            }
        }
        rgba(cr, (0.0, 0.0, 0.0), 0.13);
        cr.set_line_width(0.8);
        cr.stroke_q();
    }
}

// ---------------------------------------------------------------- burbujas y superficie

pub(crate) fn bubbles(cr: &Context, v: &View) {
    let t = v.tank;
    let (top, _) = water_colors(v);
    let col = lighten(top, 0.85);
    rgba(cr, col, 0.10);
    for b in t.bubbles.iter().filter(|b| b.kind == BubbleKind::Air) {
        cr.new_sub_path();
        cr.arc(b.x, b.y, b.r, 0.0, 2.0 * PI);
    }
    cr.fill_q();
    rgba(cr, col, 0.55);
    cr.set_line_width(1.0);
    for b in t.bubbles.iter().filter(|b| b.kind == BubbleKind::Air && b.r > 1.8) {
        cr.new_sub_path();
        cr.arc(b.x, b.y, b.r, 0.0, 2.0 * PI);
    }
    cr.stroke_q();
    rgba(cr, (1.0, 1.0, 1.0), 0.7);
    for b in t.bubbles.iter().filter(|b| b.kind == BubbleKind::Air && b.r > 3.5) {
        cr.new_sub_path();
        cr.arc(b.x - b.r * 0.3, b.y - b.r * 0.3, b.r * 0.22, 0.0, 2.0 * PI);
    }
    cr.fill_q();
    // Espuma en la superficie.
    for b in t.bubbles.iter().filter(|b| b.kind == BubbleKind::Foam) {
        let k = (b.life / b.max_life).clamp(0.0, 1.0);
        rgba(cr, (1.0, 1.0, 1.0), 0.40 * k.sqrt());
        cr.arc(b.x, b.y, b.r, 0.0, 2.0 * PI);
        cr.fill_q();
    }
}

pub(crate) fn surface_at(t: &Tank, x: f64) -> f64 {
    let b = t.boil;
    t.surface_y()
        + (2.0 + 3.0 * b) * (x * 0.018 + t.t * 1.3).sin()
        + (1.2 + 2.5 * b) * (x * 0.047 - t.t * 1.9).sin()
        + b * 2.0 * (x * 0.16 + t.t * 7.0).sin()
}

pub(crate) fn surface_path(cr: &Context, t: &Tank) {
    let mut x = 0.0;
    cr.move_to(0.0, surface_at(t, 0.0));
    while x < t.w + 8.0 {
        cr.line_to(x, surface_at(t, x));
        x += 8.0;
    }
}

pub fn surface(cr: &Context, v: &View) {
    let t = v.tank;
    let p = v.pal;
    // Aire por encima del agua.
    surface_path(cr, t);
    cr.line_to(t.w, 0.0);
    cr.line_to(0.0, 0.0);
    cr.close_path();
    rgba(cr, p.dbg, 1.0);
    cr.fill_q();
    let (top, _) = water_colors(v);
    surface_path(cr, t);
    rgba(cr, lighten(top, 0.75), 0.55);
    cr.set_line_width(2.0);
    cr.stroke_q();
    cr.save_q();
    cr.translate(0.0, 5.0);
    surface_path(cr, t);
    rgba(cr, lighten(top, 0.6), 0.14);
    cr.set_line_width(3.0);
    cr.stroke_q();
    cr.restore_q();
    // Ondas donde revientan las burbujas.
    for &(x, age) in &t.ripples {
        let k = age / 0.9;
        rgba(cr, lighten(top, 0.8), 0.5 * (1.0 - k));
        cr.set_line_width(1.2);
        cr.save_q();
        cr.translate(x, surface_at(t, x) + 1.5);
        cr.scale(1.0, 0.28);
        cr.arc(0.0, 0.0, 3.0 + 18.0 * k, 0.0, 2.0 * PI);
        cr.restore_q();
        cr.stroke_q();
    }
}

pub(crate) fn overlays(cr: &Context, v: &View) {
    let t = v.tank;
    let (w, h) = (t.w, t.h);
    // Turbidez: con la RAM llena el agua se enturbia.
    let k = ((t.ram_frac - 0.5) / 0.5).clamp(0.0, 1.0);
    if k > 0.01 {
        let (top, _) = water_colors(v);
        let murk = mix(top, v.pal.bg, 0.4);
        rgba(cr, murk, 0.34 * k * k);
        cr.rectangle(0.0, t.surface_y(), w, h);
        cr.fill_q();
    }
    // Viñeta y tinte de calor: degradados a pantalla completa, se pintan una vez y se reutilizan.
    if let Some(l) = layer("vignette".into(), w, h, |c| {
        let g = RadialGradient::new(w / 2.0, h / 2.0, w.min(h) * 0.35, w / 2.0, h / 2.0, w.max(h) * 0.75);
        g.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, 0.0);
        g.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, 0.32);
        let _ = c.set_source(&g);
        c.rectangle(0.0, 0.0, w, h);
        c.fill_q();
    }) {
        let _ = cr.set_source_surface(&l, 0.0, 0.0);
        let _ = cr.paint();
    }
    if t.boil > 0.3 {
        let hot = mix(v.pal.orange, v.pal.red, 0.5);
        if let Some(l) = layer(format!("heat{}", to_hex(hot)), w, h, |c| {
            let g = RadialGradient::new(w / 2.0, h * 1.05, h * 0.3, w / 2.0, h * 1.05, w.max(h) * 0.85);
            g.add_color_stop_rgba(0.0, hot.0, hot.1, hot.2, 0.0);
            g.add_color_stop_rgba(1.0, hot.0, hot.1, hot.2, 0.30);
            let _ = c.set_source(&g);
            c.rectangle(0.0, 0.0, w, h);
            c.fill_q();
        }) {
            let _ = cr.set_source_surface(&l, 0.0, 0.0);
            let _ = cr.paint_with_alpha(t.boil);
        }
    }
}

thread_local! {
    static LAYERS: RefCell<HashMap<String, (i32, i32, cairo::ImageSurface)>> = RefCell::new(HashMap::new());
}

/// Capa cacheada por clave y tamaño (se vuelve a pintar solo si cambia el tamaño o el color).
fn layer(key: String, w: f64, h: f64, paint: impl FnOnce(&Context)) -> Option<cairo::ImageSurface> {
    let (wi, hi) = (w.ceil() as i32, h.ceil() as i32);
    LAYERS.with(|c| {
        let mut c = c.borrow_mut();
        if let Some((cw, ch, s)) = c.get(&key)
            && *cw == wi && *ch == hi {
                return Some(s.clone());
            }
        let s = cairo::ImageSurface::create(cairo::Format::ARgb32, wi, hi).ok()?;
        paint(&Context::new(&s).ok()?);
        c.insert(key, (wi, hi, s.clone()));
        Some(s)
    })
}

// ---------------------------------------------------------------- textos

/// Color de los textos que van sobre el agua: claro siempre (aunque el tema sea claro, el agua es
/// azul medio) con sombra oscura.
pub(crate) fn label_color(p: &Palette) -> Rgb {
    if luma(p.bfg) < 0.55 { (0.96, 0.98, 1.0) } else { lighten(p.bfg, 0.1) }
}

pub(crate) fn text_at(cr: &Context, txt: &str, x: f64, y: f64, col: Rgb) {
    rgba(cr, (0.0, 0.0, 0.0), 0.6);
    cr.move_to(x + 1.0, y + 1.0);
    let _ = cr.show_text(txt);
    rgba(cr, col, 1.0);
    cr.move_to(x, y);
    let _ = cr.show_text(txt);
}

/// Etiqueta de un banco: `x` es el centro y `y` la línea base.
pub struct NameLabel {
    pub text: String,
    pub x: f64,
    pub y: f64,
}

/// Ficha del elemento señalado; `anchor` = (x, y, radio) del elemento.
pub struct Card {
    pub lines: Vec<String>,
    pub anchor: (f64, f64, f64),
}

/// Qué texto va dónde. Lo comparten el renderizador Cairo y el de GPU: solo cambia cómo se pinta.
pub struct LabelPlan {
    pub fs: f64,
    pub names: Vec<NameLabel>,
    pub hidden: Option<String>,
    pub paused: bool,
    pub card: Option<Card>,
}

pub fn plan_labels(v: &View) -> LabelPlan {
    let t = v.tank;
    let fs = (11.0 * t.scale()).clamp(10.0, 14.0);
    let mut plan = LabelPlan { fs, names: vec![], hidden: None, paused: v.paused, card: None };

    if v.names < 2 {
        // Una etiqueta por banco, sobre su pez más grande, con el número de miembros.
        let mut lead: HashMap<u64, (usize, usize)> = HashMap::new();
        for (i, f) in t.fish.iter().enumerate() {
            if f.life != Life::Alive || f.alpha < 0.9 {
                continue;
            }
            let e = lead.entry(f.kh).or_insert((i, 0));
            e.1 += 1;
            if f.len > t.fish[e.0].len {
                e.0 = i;
            }
        }
        for &i in &t.order {
            let Some(&(li, n)) = lead.get(&t.fish[i].kh) else { continue };
            let f = &t.fish[i];
            if li != i || f.len < if v.names == 0 { 70.0 } else { 20.0 } {
                continue;
            }
            plan.names.push(NameLabel {
                text: if n > 1 { format!("{} ×{}", f.key, n) } else { f.key.clone() },
                x: f.x,
                y: f.y + f.len * 0.26 + fs + 2.0,
            });
        }
    }
    if t.hidden > 0 {
        plan.hidden = Some(trf("hidden", &[t.hidden.to_string()]));
    }
    plan.card = match v.hover {
        Some(Pick::Fish(i)) => t.fish.get(i).map(|f| {
            let mut l = vec![
                format!("{}  ·  PID {}", f.key, f.pid),
                format!("RAM {}  ·  CPU {:.0}%  ·  {}", fmt_bytes(f.mem), f.cpu, trf("write_s", &[fmt_bytes(f.wbps)])),
            ];
            let n = t.fish.iter().filter(|g| g.kh == f.kh && g.life == Life::Alive).count();
            if n > 1 {
                l.push(trf("shoal", &[n.to_string()]));
            }
            Card { lines: l, anchor: (f.x, f.y, f.len * 0.3) }
        }),
        Some(Pick::Decor(i)) => t.decor.get(i).map(|d| {
            let x = d.xf * t.w;
            Card {
                lines: vec![format!("{}  ·  PID {}", d.key, d.pid), format!("RAM {}  ·  {}", fmt_bytes(d.mem), tr("protected_tag"))],
                anchor: (x, t.floor_y(x) - d.size, 0.0),
            }
        }),
        None => None,
    };
    plan
}

/// Rectángulo de la ficha (x, y, ancho, alto) y alto de línea, dado el ancho del texto más largo.
pub fn card_rect(c: &Card, text_w: f64, fs: f64, t: &Tank) -> (f64, f64, f64, f64, f64) {
    let lh = fs + 5.0;
    let (bw, bh) = (text_w + 20.0, c.lines.len() as f64 * lh + 10.0);
    let bx = (c.anchor.0 - bw / 2.0).clamp(6.0, (t.w - bw - 6.0).max(6.0));
    let mut by = c.anchor.1 - c.anchor.2 - bh - 14.0;
    if by < 6.0 {
        by = c.anchor.1 + c.anchor.2 + 14.0;
    }
    (bx, by, bw, bh, lh)
}

pub(crate) fn labels(cr: &Context, v: &View) {
    let t = v.tank;
    let p = v.pal;
    let plan = plan_labels(v);
    let fs = plan.fs;
    cr.select_font_face(v.font, cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(fs);
    for n in &plan.names {
        if let Ok(e) = cr.text_extents(&n.text) {
            // Píldora translúcida detrás del nombre: legible sobre cualquier pez.
            rgba(cr, p.dbg, 0.55);
            cr.rectangle(n.x - e.width() / 2.0 - 5.0, n.y - fs - 1.0, e.width() + 10.0, fs + 6.0);
            cr.fill_q();
            text_at(cr, &n.text, n.x - e.width() / 2.0, n.y, label_color(p));
        }
    }
    if let Some(h) = &plan.hidden {
        text_at(cr, h, 12.0, t.h - 12.0, mix(label_color(p), (0.5, 0.6, 0.7), 0.35));
    }
    if plan.paused {
        cr.set_font_size(fs + 3.0);
        let s = tr("paused");
        if let Ok(e) = cr.text_extents(s) {
            text_at(cr, s, t.w - e.width() - 14.0, t.surface_y() + fs + 12.0, p.yellow);
        }
        cr.set_font_size(fs);
    }
    let Some(card) = &plan.card else { return };
    let tw = card.lines.iter().filter_map(|l| cr.text_extents(l).ok()).fold(0.0f64, |m, e| m.max(e.width()));
    let (bx, by, bw, bh, lh) = card_rect(card, tw, fs, t);
    rgba(cr, p.dbg, 0.92);
    cr.rectangle(bx, by, bw, bh);
    cr.fill_keep();
    rgba(cr, p.accent, 0.9);
    cr.set_line_width(1.0);
    cr.stroke_q();
    for (i, l) in card.lines.iter().enumerate() {
        rgba(cr, if i == 0 { p.bfg } else { p.fg }, 1.0);
        cr.move_to(bx + 10.0, by + lh * (i as f64 + 1.0));
        let _ = cr.show_text(l);
    }
}
