//! Estilo Sonar: la pecera como pantalla de radar. Fondo oscuro con rejilla y líneas de barrido,
//! peces como siluetas de luz (contorno con halo, columna y costillas, como un eco), una sola gama
//! de color por banco sacada del tema, y los procesos protegidos como infraestructura apagada.
//! Reutiliza la anatomía animada del estilo estilizado: solo cambia cómo se pinta.

use std::f64::consts::PI;

use gtk::cairo::{self, Context};

use crate::draw::{FinDef, FishArt, Paint, Quiet, SPECIES_DEFS, Spine, View, draw_fin, draw_tail, glow_stroke, labels, lin, rgba, surface_path, trace_body};
use crate::sim::{BubbleKind, Life, Pick, Tank};
use crate::theme::{Palette, Rgb, lighten, mix};

/// Color del eco de un pez: el primario del tema, llevado hacia el claro.
pub fn fish_color(look: &crate::sim::Look, pal: &Palette, dying: bool) -> Rgb {
    let c = pal.fish[look.c1];
    if dying { mix(c, pal.dfg, 0.7) } else { lighten(c, 0.12) }
}

/// Color de las líneas de la escena (rejilla, arena, superficie).
pub fn line_color(pal: &Palette, boil: f64) -> Rgb {
    mix(pal.accent, pal.red, boil * 0.8)
}

pub fn paint(cr: &Context, a: &FishArt, pal: &Palette) {
    let def = &SPECIES_DEFS[a.look.species];
    let len = a.len;
    let n = if len < 32.0 { 12 } else { 28 };
    let col = fish_color(&a.look, pal, a.dying);
    let hh = def.ratio * len;
    let sp = Spine::new(def, len, a.phase, a.amp, n);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.set_line_cap(cairo::LineCap::Round);

    // Cola y aletas como contornos con halo.
    draw_tail(cr, &sp, def, len, col, a.phase, a.amp, 0, Paint::Glow);
    draw_fin(cr, &sp, &def.dorsal, true, hh, col, a.phase, 0, Paint::Glow);
    draw_fin(cr, &sp, &def.anal, false, hh, col, a.phase, 0, Paint::Glow);

    // Cuerpo: relleno casi transparente (el eco) y contorno brillante.
    cr.new_path();
    trace_body(cr, &sp);
    rgba(cr, col, 0.07);
    cr.fill_keep();
    glow_stroke(cr, col, len);

    // Columna y costillas: lo que lo convierte en un barrido y no en un dibujo.
    if len >= 22.0 {
        rgba(cr, col, 0.45);
        cr.set_line_width((len * 0.006).max(0.6));
        for i in 0..=sp.n {
            let p = sp.p[i];
            if i == 0 {
                cr.move_to(p.0, p.1);
            } else {
                cr.line_to(p.0, p.1);
            }
        }
        cr.stroke_q();
        let ribs = ((len / 14.0) as usize).clamp(3, 9);
        rgba(cr, col, 0.30);
        for k in 1..=ribs {
            let u = 0.18 + 0.62 * k as f64 / (ribs as f64 + 1.0);
            let (p, nu, h) = sp.at(u);
            cr.move_to(p.0 + nu.0 * h * 0.8, p.1 + nu.1 * h * 0.8);
            cr.line_to(p.0 - nu.0 * h * 0.8, p.1 - nu.1 * h * 0.8);
        }
        cr.stroke_q();
    }

    // Ojo: un punto de luz (o una cruz si está muriendo).
    let (pe, nu, h) = sp.at(0.12);
    let (ex, ey) = (pe.0 + nu.0 * h * 0.30, pe.1 + nu.1 * h * 0.30);
    let r = (len * 0.03).clamp(1.0, 5.0);
    if a.dying {
        rgba(cr, col, 0.9);
        cr.set_line_width(1.0);
        cr.move_to(ex - r, ey - r);
        cr.line_to(ex + r, ey + r);
        cr.move_to(ex + r, ey - r);
        cr.line_to(ex - r, ey + r);
        cr.stroke_q();
    } else {
        rgba(cr, col, 0.35);
        cr.arc(ex, ey, r * 2.2, 0.0, 2.0 * PI);
        cr.fill_q();
        rgba(cr, lighten(col, 0.6), 1.0);
        cr.arc(ex, ey, r, 0.0, 2.0 * PI);
        cr.fill_q();
    }
    let _: &FinDef = &def.dorsal;
}

/// Caja del sprite (como el estilizado, con margen para el halo).
pub fn extent(species: usize, len: f64, amp: f64) -> (f64, f64) {
    let def = &SPECIES_DEFS[species];
    let hh = def.ratio * len;
    let fin_h = def.dorsal.h.max(def.anal.h) * hh * (1.0 + def.dorsal.lean.max(def.anal.lean) * 0.4);
    (len * 0.5 + 10.0, (hh * 1.1 + fin_h + len * 0.17 * amp + 10.0).max(len * 0.35))
}

// ---------------------------------------------------------------- escena (Cairo de referencia)

/// Líneas de la rejilla: (x0, y0, x1, y1, brillante).
pub fn grid_lines(t: &Tank) -> Vec<(f64, f64, f64, f64, bool)> {
    let step = (40.0 * t.scale()).round().max(24.0);
    let mut v = vec![];
    let mut i = 0;
    let mut x = 0.0;
    while x <= t.w {
        v.push((x, 0.0, x, t.h, i % 5 == 0));
        x += step;
        i += 1;
    }
    i = 0;
    let mut y = 0.0;
    while y <= t.h {
        v.push((0.0, y, t.w, y, i % 5 == 0));
        y += step;
        i += 1;
    }
    v
}

/// Perfil de la arena como polilínea.
pub fn floor_path(cr: &Context, t: &Tank) {
    let mut x = 0.0;
    cr.move_to(0.0, t.floor_y(0.0));
    while x < t.w + 6.0 {
        cr.line_to(x, t.floor_y(x));
        x += 6.0;
    }
}

/// Parpadeo de la pantalla cuando hierve.
pub fn flicker(t: &Tank) -> f64 {
    1.0 - t.boil * 0.12 * (0.5 + 0.5 * (t.t * 23.0).sin())
}

pub fn draw_tank(cr: &Context, v: &View) {
    let t = v.tank;
    let p = v.pal;
    let (w, h) = (t.w, t.h);
    let lc = line_color(p, t.boil);
    cr.save_q();
    cr.rectangle(0.0, 0.0, w, h);
    cr.clip();
    rgba(cr, p.dbg, 1.0);
    cr.paint_q();
    // Fondo: un leve degradado hacia el color de línea por abajo (el calor sube desde el fondo).
    let _ = cr.set_source(lin(0.0, 0.0, 0.0, h, &[(0.0, lc, 0.0), (1.0, lc, 0.06 + 0.16 * t.boil)]));
    cr.rectangle(0.0, 0.0, w, h);
    cr.fill_q();
    // Rejilla.
    cr.set_line_width(1.0);
    for (x0, y0, x1, y1, bright) in grid_lines(t) {
        rgba(cr, lc, if bright { 0.16 } else { 0.07 });
        cr.move_to(x0 + 0.5, y0 + 0.5);
        cr.line_to(x1 + 0.5, y1 + 0.5);
        cr.stroke_q();
    }
    // Líneas de barrido horizontales.
    rgba(cr, (0.0, 0.0, 0.0), 0.18);
    let mut y = 0.0;
    while y < h {
        cr.rectangle(0.0, y, w, 1.0);
        y += 3.0;
    }
    cr.fill_q();
    // Nieve: puntos tenues.
    for m in &t.motes {
        rgba(cr, lc, 0.08 + 0.25 * m.z);
        cr.rectangle(m.x.round(), m.y.round(), 1.0, 1.0);
        cr.fill_q();
    }
    // Arena: perfil con halo y relleno tenue debajo.
    floor_path(cr, t);
    cr.line_to(w, h);
    cr.line_to(0.0, h);
    cr.close_path();
    rgba(cr, lc, 0.10);
    cr.fill_q();
    floor_path(cr, t);
    glow_stroke(cr, lc, 120.0);
    // Decoración: siluetas apagadas de infraestructura.
    for (i, d) in t.decor.iter().enumerate() {
        cr.push_group();
        crate::draw::decor(cr, v, d, false);
        let _ = cr.pop_group_to_source();
        let pat = cr.source();
        rgba(cr, mix(p.dfg, lc, 0.3), 0.55);
        let _ = cr.mask(&pat);
        if v.hover == Some(Pick::Decor(i)) {
            let (bx, by, bw, bh) = crate::draw::decor_box(t, d);
            rgba(cr, lc, 0.8);
            cr.set_line_width(1.0);
            cr.rectangle(bx, by, bw, bh);
            cr.stroke_q();
        }
    }
    // Compañeros de banco del pez señalado: un círculo de rastreo.
    if let Some(Pick::Fish(hi)) = v.hover
        && let Some(hf) = t.fish.get(hi)
    {
        for f in t.fish.iter().filter(|f| f.kh == hf.kh && f.life == Life::Alive) {
            rgba(cr, lc, 0.5);
            cr.set_line_width(1.0);
            cr.arc(f.x, f.y, f.len * 0.65, 0.0, 2.0 * PI);
            cr.stroke_q();
        }
    }
    let ds = cr.target().device_scale().0.max(1.0);
    crate::draw::reset_budget();
    cr.push_group();
    for &i in &t.order {
        crate::draw::fish(cr, &t.fish[i], p, v.hover == Some(Pick::Fish(i)), ds, v.style);
    }
    let _ = cr.pop_group_to_source();
    let _ = cr.paint_with_alpha(flicker(t));
    // Burbujas: anillos finos.
    cr.set_line_width(1.0);
    for b in &t.bubbles {
        match b.kind {
            BubbleKind::Air if b.r < 2.0 => {
                rgba(cr, lc, 0.5);
                cr.rectangle(b.x.round(), b.y.round(), 1.0, 1.0);
                cr.fill_q();
            }
            BubbleKind::Air => {
                rgba(cr, lc, 0.55);
                cr.arc(b.x, b.y, b.r, 0.0, 2.0 * PI);
                cr.stroke_q();
            }
            BubbleKind::Foam => {
                rgba(cr, lc, 0.5 * (b.life / b.max_life).clamp(0.0, 1.0));
                cr.rectangle(b.x - b.r, b.y, 2.0 * b.r, 1.0);
                cr.fill_q();
            }
        }
    }
    // Superficie: polilínea brillante.
    surface_path(cr, t);
    glow_stroke(cr, lc, 120.0);
    // Viñeta de pantalla.
    let g = cairo::RadialGradient::new(w / 2.0, h / 2.0, w.min(h) * 0.4, w / 2.0, h / 2.0, w.max(h) * 0.8);
    g.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, 0.0);
    g.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, 0.45);
    let _ = cr.set_source(&g);
    cr.rectangle(0.0, 0.0, w, h);
    cr.fill_q();
    labels(cr, v);
    cr.restore_q();
}
