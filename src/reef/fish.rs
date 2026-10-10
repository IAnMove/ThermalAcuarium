//! Pintor de peces del estilo realista. Cada pez se renderiza una vez a un sprite (ver `draw.rs`),
//! así que aquí se puede gastar: cuerpo suavizado, contraluz, escamas con reflejo, textura de piel,
//! aletas con radios y membranas translúcidas, agallas y un ojo con iris.

use std::f64::consts::PI;

use gtk::cairo::{self, Context, RadialGradient};

use super::noise;
use super::species::{Mark, RFin, RSpecies, SPECIES};
use crate::draw::{FishArt, NS, Quiet, Spine, TailKind, closed_smooth, desat, lin, rgba};
use crate::sim::{Look, unit};
use crate::theme::{Rgb, darken, lighten, mix};

thread_local! {
    /// Textura de piel: ruido que se mezcla con "luz suave" sobre el cuerpo.
    static MOTTLE: cairo::ImageSurface = noise::grey_tile(128, 6, 3, 4242, 1.7);
}

/// Mitad del ancho y del alto de la caja del sprite de un pez de longitud `len`.
pub fn extent(look: &Look, len: f64, amp: f64) -> (f64, f64) {
    let s = &SPECIES[look.real];
    let hh = s.ratio * len;
    let fh = |f: &RFin| f.h * hh * (1.0 + f.lean * 0.3);
    let mut fins = fh(&s.dorsal).max(fh(&s.anal));
    if let Some(d) = &s.dorsal2 {
        fins = fins.max(fh(d));
    }
    let tail = len * s.tail_frac * s.spread;
    let wave = len * 0.17 * amp * s.bend;
    let pect = s.pect * len * s.pect_h;
    let half_h = (hh * 1.1 + fins).max(tail + wave).max(hh * 0.6 + pect * 1.2) + 8.0;
    (len * 0.5 + 8.0 + hh * 0.8, half_h.max(len * 0.25))
}

/// Punto del cuerpo en (u, v): `u` nariz→cola, `v` panza (-1) → lomo (+1).
fn at(sp: &Spine, u: f64, v: f64) -> (f64, f64) {
    let (p, nu, h) = sp.at(u.clamp(0.0, 1.0));
    (p.0 + nu.0 * h * v, p.1 + nu.1 * h * v)
}

/// Curva cerrada suave que pasa por los puntos (Catmull-Rom → Bézier).
fn spline_closed(cr: &Context, pts: &[(f64, f64)]) {
    let n = pts.len();
    for i in 0..n {
        let (p0, p1, p2, p3) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n], pts[(i + 2) % n]);
        if i == 0 {
            cr.move_to(p1.0, p1.1);
        }
        cr.curve_to(
            p1.0 + (p2.0 - p0.0) / 6.0,
            p1.1 + (p2.1 - p0.1) / 6.0,
            p2.0 - (p3.0 - p1.0) / 6.0,
            p2.1 - (p3.1 - p1.1) / 6.0,
            p2.0,
            p2.1,
        );
    }
    cr.close_path();
}

pub fn paint(cr: &Context, a: &FishArt) {
    let s = &SPECIES[a.look.real];
    let len = a.len;
    let hh = s.ratio * len;
    let lb = len * (1.0 - s.tail_frac);
    let detail: u8 = if len < 26.0 { 0 } else if len < 60.0 { 1 } else { 2 };
    let n = match detail {
        0 => 14,
        1 => 26,
        _ => NS,
    };
    let sp = Spine::build(s.prof, s.ratio, s.tail_frac, len, a.phase, a.amp * s.bend, n);

    // Pequeña variación de tono por pez; apagado si está muerto.
    let tint = a.look.tint * 0.07;
    let adj = |c: Rgb| {
        let c = if tint > 0.0 { lighten(c, tint) } else { darken(c, -tint) };
        if a.dying { darken(desat(c, 0.6), 0.12) } else { c }
    };
    let (back, midc, belly) = (adj(s.back), adj(s.mid), adj(s.belly));
    let fin_c = adj(s.fin);
    let edge = s.fin_edge.map(adj);

    // --- cola y aletas por detrás del cuerpo
    tail(cr, &sp, s, len, adj(s.tail_col), edge, a.phase, a.amp * s.bend, detail);
    if let Some(d) = &s.dorsal2 {
        fin(cr, &sp, d, true, hh, fin_c, s.fin_a, edge, a.phase, detail);
    }
    fin(cr, &sp, &s.dorsal, true, hh, fin_c, s.fin_a, edge, a.phase, detail);
    fin(cr, &sp, &s.anal, false, hh, fin_c, s.fin_a, edge, a.phase, detail);

    // --- cuerpo
    let mut pts = Vec::with_capacity(2 * (n + 1));
    for i in 0..=n {
        pts.push((sp.p[i].0 + sp.nu[i].0 * sp.h[i], sp.p[i].1 + sp.nu[i].1 * sp.h[i]));
    }
    for i in (0..=n).rev() {
        pts.push((sp.p[i].0 - sp.nu[i].0 * sp.h[i] * 1.04, sp.p[i].1 - sp.nu[i].1 * sp.h[i] * 1.04));
    }
    cr.new_path();
    closed_smooth(cr, &pts);
    let path = cr.copy_path().ok();

    cr.save_q();
    cr.clip();
    // Contraluz: lomo oscuro, flancos con el color de la especie, vientre claro.
    let _ = cr.set_source(lin(
        0.0,
        -hh,
        0.0,
        hh * 1.05,
        &[(0.0, back, 1.0), (0.40, midc, 1.0), (0.78, mix(midc, belly, 0.55), 1.0), (1.0, belly, 1.0)],
    ));
    let _ = cr.paint();

    marks(cr, &sp, s, hh, len, lb, &a.look, a.dying);

    if let Some((c, ia)) = s.irid {
        cr.set_operator(cairo::Operator::Screen);
        let (xn, xt) = (len * 0.5, len * 0.5 - lb);
        let _ = cr.set_source(lin(xn, 0.0, xt, 0.0, &[(0.0, c, ia * 0.2), (0.5, c, ia), (1.0, c, ia * 0.4)]));
        let _ = cr.paint();
        cr.set_operator(cairo::Operator::Over);
    }

    if detail == 2 && s.scale > 0.0 {
        scales(cr, &sp, s, hh, lb, midc);
    }
    if detail > 0 {
        mottle(cr, len, if detail == 2 { 0.55 } else { 0.35 });
    }

    // Volumen: oscurece lomo, vientre y extremos; brillo en el flanco y luz de borde en el lomo.
    let _ = cr.set_source(lin(
        0.0,
        -hh,
        0.0,
        hh * 1.05,
        &[(0.0, (0.0, 0.02, 0.06), 0.34), (0.20, (0.0, 0.02, 0.06), 0.0), (0.72, (0.0, 0.02, 0.06), 0.0), (1.0, (0.0, 0.02, 0.06), 0.30)],
    ));
    let _ = cr.paint();
    let _ = cr.set_source(lin(
        len * 0.5,
        0.0,
        len * 0.5 - lb,
        0.0,
        &[(0.0, (0.0, 0.02, 0.06), 0.16), (0.18, (0.0, 0.02, 0.06), 0.0), (0.80, (0.0, 0.02, 0.06), 0.0), (1.0, (0.0, 0.02, 0.06), 0.14)],
    ));
    let _ = cr.paint();
    if detail > 0 {
        let (cx, cy) = at(&sp, 0.36, 0.48);
        cr.save_q();
        cr.translate(cx, cy);
        cr.scale(len * 0.20, hh * 0.20);
        let g = RadialGradient::new(0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
        g.add_color_stop_rgba(0.0, 1.0, 1.0, 1.0, 0.28 * s.gloss.min(1.3));
        g.add_color_stop_rgba(1.0, 1.0, 1.0, 1.0, 0.0);
        let _ = cr.set_source(&g);
        cr.arc(0.0, 0.0, 1.0, 0.0, 2.0 * PI);
        cr.fill_q();
        cr.restore_q();
        // Luz de borde a lo largo del lomo.
        for i in 0..=14 {
            let p = at(&sp, 0.10 + 0.78 * i as f64 / 14.0, 0.93);
            if i == 0 {
                cr.move_to(p.0, p.1);
            } else {
                cr.line_to(p.0, p.1);
            }
        }
        rgba(cr, (0.8, 0.95, 1.0), 0.20 * s.gloss.min(1.2));
        cr.set_line_width((hh * 0.06).max(0.8));
        cr.set_line_cap(cairo::LineCap::Round);
        cr.stroke_q();
    }
    cr.restore_q();

    // Contorno suave.
    cr.new_path();
    if let Some(p) = &path {
        cr.append_path(p);
    }
    rgba(cr, darken(midc, 0.62), 0.55);
    cr.set_line_width((len * 0.0045).max(0.6));
    cr.stroke_q();

    // --- aletas delanteras, agalla, boca y ojo
    if detail > 0 {
        let flap = 0.22 + 0.42 * (1.0 - s.bend);
        fan(cr, &sp, s, 0.27, -0.12, 0.40 - flap * 0.3 + flap * (a.phase + 1.0).sin(), s.pect * len, s.pect_h, fin_c, (s.fin_a * 0.85).clamp(0.45, 0.8), edge, detail);
        if s.pelvic > 0.0 {
            fan(cr, &sp, s, 0.30, -0.90, 1.05 + 0.10 * (a.phase + 2.0).sin(), s.pelvic * len, 0.5, fin_c, 0.75, edge, detail);
        }
        gill(cr, &sp, s, hh, len, midc);
        mouth(cr, &sp, hh, len, midc);
    }
    eye(cr, &sp, s, hh, len, detail, a.dying, adj(s.iris));
}

// ---------------------------------------------------------------- marcas, escamas y piel

#[allow(clippy::too_many_arguments)]
fn marks(cr: &Context, sp: &Spine, s: &RSpecies, hh: f64, len: f64, lb: f64, look: &Look, dying: bool) {
    let adjc = |c: Rgb| if dying { darken(desat(c, 0.6), 0.1) } else { c };
    for (mi, m) in s.marks.iter().enumerate() {
        match m {
            Mark::Bar { u, w, col, edge, slant, a } => {
                let mut left = [(0.0, 0.0); 9];
                let mut right = [(0.0, 0.0); 9];
                for k in 0..=8 {
                    let v = 1.25 - 2.5 * k as f64 / 8.0;
                    let sl = slant * v;
                    left[k] = at(sp, u - w / 2.0 + sl, v);
                    right[k] = at(sp, u + w / 2.0 + sl, v);
                }
                cr.move_to(left[0].0, left[0].1);
                for p in &left[1..] {
                    cr.line_to(p.0, p.1);
                }
                for p in right.iter().rev() {
                    cr.line_to(p.0, p.1);
                }
                cr.close_path();
                rgba(cr, adjc(*col), *a);
                cr.fill_q();
                if let Some(e) = edge {
                    rgba(cr, adjc(*e), 0.95);
                    cr.set_line_width((hh * 0.045).max(0.7));
                    for side in [&left, &right] {
                        cr.move_to(side[0].0, side[0].1);
                        for p in &side[1..] {
                            cr.line_to(p.0, p.1);
                        }
                    }
                    cr.stroke_q();
                }
            }
            Mark::Patch { pts, col, a } => {
                let mapped: Vec<(f64, f64)> = pts.iter().map(|&(u, v)| at(sp, u, v)).collect();
                spline_closed(cr, &mapped);
                rgba(cr, adjc(*col), *a);
                cr.fill_q();
            }
            Mark::Stripe { v, w, u0, u1, col, a } => {
                for k in 0..=16 {
                    let p = at(sp, u0 + (u1 - u0) * k as f64 / 16.0, *v);
                    if k == 0 {
                        cr.move_to(p.0, p.1);
                    } else {
                        cr.line_to(p.0, p.1);
                    }
                }
                rgba(cr, adjc(*col), *a);
                cr.set_line_width((w * hh * 1.6).max(0.8));
                cr.set_line_cap(cairo::LineCap::Round);
                cr.stroke_q();
            }
            Mark::Spots { n, r, u0, u1, v0, v1, col, a } => {
                for i in 0..*n {
                    let sd = look.seed.wrapping_add(mi as u64 * 1000 + i as u64 * 7);
                    let u = u0 + (u1 - u0) * unit(sd);
                    let v = v0 + (v1 - v0) * unit(sd ^ 0x5555);
                    let p = at(sp, u, v);
                    let rr = (r * len * (0.6 + 0.8 * unit(sd ^ 0x77))).max(0.5);
                    rgba(cr, adjc(*col), *a);
                    cr.arc(p.0, p.1, rr, 0.0, 2.0 * PI);
                    cr.fill_q();
                }
            }
            Mark::TwoTone { u, soft, col, a } => {
                let (x0, x1) = (at(sp, u - soft, 0.0).0, at(sp, u + soft, 0.0).0);
                let c = adjc(*col);
                let _ = cr.set_source(lin(x0, 0.0, x1, 0.0, &[(0.0, c, 0.0), (1.0, c, *a)]));
                let _ = cr.paint();
                let _ = lb;
            }
            Mark::Lines { n, u0, u1, slope, w, col, a } => {
                for i in 0..*n {
                    let uc = u0 + (u1 - u0) * i as f64 / (*n - 1).max(1) as f64;
                    for k in 0..=6 {
                        let v = 1.2 - 2.4 * k as f64 / 6.0;
                        let p = at(sp, uc + slope * (-v) * 0.5, v);
                        if k == 0 {
                            cr.move_to(p.0, p.1);
                        } else {
                            cr.line_to(p.0, p.1);
                        }
                    }
                }
                rgba(cr, adjc(*col), *a);
                cr.set_line_width((w * len).max(0.6));
                cr.stroke_q();
            }
        }
    }
}

/// Escamas: cada una con su arco de sombra y un brillo que cae del lomo al vientre.
fn scales(cr: &Context, sp: &Spine, s: &RSpecies, hh: f64, lb: f64, midc: Rgb) {
    let sc = (hh * s.scale * 2.7).max(2.5);
    let rows = (2.0 / (sc * 0.85 / hh)).ceil() as i32;
    let cols = ((0.80 * lb) / (sc * 0.95)).ceil() as i32;
    let tint = s.scale_col;
    cr.set_line_cap(cairo::LineCap::Round);
    for j in 0..cols {
        for i in 0..rows {
            let u = 0.16 + 0.80 * j as f64 / cols as f64;
            let v = -0.95 + 1.9 * (i as f64 + if j % 2 == 1 { 0.5 } else { 0.0 }) / rows as f64;
            let (cx, cy) = at(sp, u, v);
            let shade = 0.5 + 0.5 * v; // más brillo hacia el lomo
            cr.new_path();
            cr.arc(cx + sc * 0.2, cy, sc * 0.62, 0.55 * PI, 1.45 * PI);
            if let Some(t) = tint {
                rgba(cr, t, 0.55);
                cr.set_line_width((sc * 0.16).max(0.6));
            } else {
                rgba(cr, darken(midc, 0.7), 0.11);
                cr.set_line_width((sc * 0.09).max(0.5));
            }
            cr.stroke_q();
            cr.new_path();
            cr.arc(cx + sc * 0.12, cy - sc * 0.06, sc * 0.55, 0.62 * PI, 1.38 * PI);
            rgba(cr, (1.0, 1.0, 1.0), 0.04 + 0.08 * shade);
            cr.set_line_width((sc * 0.10).max(0.5));
            cr.stroke_q();
        }
    }
}

/// Textura de piel (mancha de ruido mezclada con luz suave).
fn mottle(cr: &Context, len: f64, alpha: f64) {
    MOTTLE.with(|tile| {
        let pat = cairo::SurfacePattern::create(tile);
        pat.set_extend(cairo::Extend::Repeat);
        pat.set_filter(cairo::Filter::Bilinear);
        let f = (len / 70.0).max(0.5);
        let mut m = cairo::Matrix::identity();
        m.scale(1.0 / f, 1.0 / f);
        pat.set_matrix(m);
        cr.set_operator(cairo::Operator::SoftLight);
        let _ = cr.set_source(&pat);
        let _ = cr.paint_with_alpha(alpha);
        cr.set_operator(cairo::Operator::Over);
    });
}

// ---------------------------------------------------------------- aletas

#[allow(clippy::too_many_arguments)]
fn fin(cr: &Context, sp: &Spine, f: &RFin, up: bool, hh: f64, col: Rgb, a: f64, edge: Option<Rgb>, phase: f64, detail: u8) {
    let m = if detail == 0 { 8 } else { 20 };
    let sgn = if up { 1.0 } else { -1.0 };
    let mut base = [(0.0, 0.0); 21];
    let mut tip = [(0.0, 0.0); 21];
    for k in 0..=m {
        let s = k as f64 / m as f64;
        let (p, nu, h) = sp.at(f.u0 + (f.u1 - f.u0) * s);
        let on = (nu.0 * sgn, nu.1 * sgn);
        let tl = (nu.1, -nu.0);
        // Perfil de la membrana: sube rápido por delante y baja suave por detrás.
        let dome = (1.0 - (2.0 * s.powf(0.8) - 1.0).powi(2)).max(0.0).powf(0.7);
        let ht = f.h * hh * 1.3 * dome * if f.spiny > s { 1.0 } else { 0.92 };
        let along = f.lean * ht * 0.55 + (phase - 5.0 * s).sin() * ht * 0.08;
        let b = (p.0 + on.0 * h * 0.85, p.1 + on.1 * h * 0.85);
        base[k] = b;
        tip[k] = (b.0 + on.0 * ht + tl.0 * along, b.1 + on.1 * ht + tl.1 * along);
    }
    // Membrana translúcida: más densa en la base, casi transparente en el borde.
    let (b0, t0) = (base[m / 2], tip[m / 2]);
    let _ = cr.set_source(lin(b0.0, b0.1, t0.0, t0.1, &[(0.0, darken(col, 0.1), a), (1.0, lighten(col, 0.15), a * 0.45)]));
    cr.move_to(base[0].0, base[0].1);
    for p in &tip[1..=m] {
        cr.line_to(p.0, p.1);
    }
    for p in base[..=m].iter().rev() {
        cr.line_to(p.0, p.1);
    }
    cr.close_path();
    cr.fill_q();
    if detail > 0 {
        // Radios: los espinosos son más gruesos y oscuros.
        for k in 1..m {
            let spiny = (k as f64 / m as f64) < f.spiny;
            rgba(cr, darken(col, if spiny { 0.55 } else { 0.4 }), if spiny { 0.7 } else { 0.30 });
            cr.set_line_width(if spiny { 1.1 } else { 0.6 });
            cr.move_to(base[k].0, base[k].1);
            cr.line_to(tip[k].0, tip[k].1);
            cr.stroke_q();
        }
    }
    if let Some(e) = edge {
        rgba(cr, e, 0.9);
        cr.set_line_width((hh * 0.05).max(0.7));
        cr.move_to(tip[0].0, tip[0].1);
        for p in &tip[1..=m] {
            cr.line_to(p.0, p.1);
        }
        cr.stroke_q();
    }
}

/// Aleta en abanico (pectoral o pélvica) pegada al cuerpo en (u, v).
#[allow(clippy::too_many_arguments)]
fn fan(cr: &Context, sp: &Spine, _s: &RSpecies, u: f64, v: f64, rot: f64, length: f64, spread: f64, col: Rgb, a: f64, edge: Option<Rgb>, detail: u8) {
    if length < 2.0 {
        return;
    }
    let (px, py) = at(sp, u, v);
    cr.save_q();
    cr.translate(px, py);
    cr.rotate(rot);
    let half = 0.30 + 0.55 * spread;
    let m = 10;
    // Radios desde la base hasta un borde redondeado.
    cr.move_to(0.0, 0.0);
    for k in 0..=m {
        let t = k as f64 / m as f64;
        let ang = PI + (-half + 2.0 * half * t); // hacia la cola (-x)
        let r = length * (0.80 + 0.20 * (PI * t).sin());
        cr.line_to(ang.cos() * r, ang.sin() * r);
    }
    cr.close_path();
    let _ = cr.set_source(lin(0.0, 0.0, -length, 0.0, &[(0.0, darken(col, 0.1), a), (1.0, lighten(col, 0.25), a * 0.35)]));
    cr.fill_keep();
    if detail > 0 {
        rgba(cr, darken(col, 0.5), 0.3);
        cr.set_line_width(0.6);
        for k in 0..=m {
            let t = k as f64 / m as f64;
            let ang = PI + (-half + 2.0 * half * t);
            let r = length * (0.80 + 0.20 * (PI * t).sin());
            cr.move_to(0.0, 0.0);
            cr.line_to(ang.cos() * r, ang.sin() * r);
        }
        cr.stroke_q();
    }
    if let Some(e) = edge {
        rgba(cr, e, 0.5);
        cr.set_line_width(0.8);
        cr.new_path();
        for k in 0..=m {
            let t = k as f64 / m as f64;
            let ang = PI + (-half + 2.0 * half * t);
            let r = length * (0.80 + 0.20 * (PI * t).sin());
            if k == 0 {
                cr.move_to(ang.cos() * r, ang.sin() * r);
            } else {
                cr.line_to(ang.cos() * r, ang.sin() * r);
            }
        }
        cr.stroke_q();
    }
    cr.restore_q();
}

#[allow(clippy::too_many_arguments)]
fn tail(cr: &Context, sp: &Spine, s: &RSpecies, len: f64, col: Rgb, edge: Option<Rgb>, phase: f64, amp: f64, detail: u8) {
    let n = sp.n;
    let (pe, pp) = (sp.p[n], sp.p[n - 1]);
    let ang = (pe.1 - pp.1).atan2(pe.0 - pp.0);
    let swing = 0.5 * amp * (phase - 4.6 - 0.9).sin();
    let tl = s.tail_frac * len;
    let sp_h = tl * s.spread;
    let w0 = (sp.h[n] * 0.9).max(0.6);
    cr.save_q();
    cr.translate(pe.0, pe.1);
    cr.rotate(ang + swing);
    match s.tail {
        TailKind::Forked => {
            cr.move_to(0.0, -w0);
            cr.curve_to(tl * 0.35, -w0 * 1.2, tl * 0.8, -sp_h * 0.9, tl, -sp_h);
            cr.curve_to(tl * 0.78, -sp_h * 0.4, tl * 0.62, -sp_h * 0.12, tl * 0.56, 0.0);
            cr.curve_to(tl * 0.62, sp_h * 0.12, tl * 0.78, sp_h * 0.4, tl, sp_h);
            cr.curve_to(tl * 0.8, sp_h * 0.9, tl * 0.35, w0 * 1.2, 0.0, w0);
        }
        TailKind::Crescent => {
            cr.move_to(0.0, -w0);
            cr.curve_to(tl * 0.25, -w0 * 1.1, tl * 0.6, -sp_h * 0.9, tl, -sp_h);
            cr.curve_to(tl * 0.66, -sp_h * 0.55, tl * 0.5, -sp_h * 0.15, tl * 0.40, 0.0);
            cr.curve_to(tl * 0.5, sp_h * 0.15, tl * 0.66, sp_h * 0.55, tl, sp_h);
            cr.curve_to(tl * 0.6, sp_h * 0.9, tl * 0.25, w0 * 1.1, 0.0, w0);
        }
        _ => {
            cr.move_to(0.0, -w0);
            cr.curve_to(tl * 0.45, -sp_h, tl * 1.05, -sp_h * 0.75, tl, 0.0);
            cr.curve_to(tl * 1.05, sp_h * 0.75, tl * 0.45, sp_h, 0.0, w0);
        }
    }
    cr.close_path();
    let _ = cr.set_source(lin(0.0, 0.0, tl, 0.0, &[(0.0, darken(col, 0.05), s.fin_a), (1.0, lighten(col, 0.10), s.fin_a * 0.8)]));
    cr.fill_keep();
    rgba(cr, edge.unwrap_or(darken(col, 0.5)), if edge.is_some() { 0.9 } else { 0.35 });
    cr.set_line_width(if edge.is_some() { (len * 0.006).max(0.7) } else { 0.7 });
    cr.stroke_q();
    if detail > 0 {
        rgba(cr, darken(col, 0.5), 0.26);
        cr.set_line_width(0.6);
        for k in 0..9 {
            let yy = sp_h * (-0.85 + 1.7 * k as f64 / 8.0);
            cr.move_to(0.0, yy * 0.12);
            cr.line_to(tl * 0.93, yy);
        }
        cr.stroke_q();
    }
    cr.restore_q();
}

// ---------------------------------------------------------------- cara

fn gill(cr: &Context, sp: &Spine, s: &RSpecies, hh: f64, len: f64, midc: Rgb) {
    let ug = s.eye_u + 0.115;
    let mut pts = [(0.0, 0.0); 9];
    for (k, p) in pts.iter_mut().enumerate() {
        let v = 0.92 - 1.84 * k as f64 / 8.0;
        *p = at(sp, ug + 0.03 * (1.0 - v * v) + 0.01 * v, v);
    }
    for (off, col, a) in [(0.0, darken(midc, 0.7), 0.5), (1.3, (1.0, 1.0, 1.0), 0.16)] {
        cr.new_path();
        cr.move_to(pts[0].0 - off, pts[0].1);
        for p in &pts[1..] {
            cr.line_to(p.0 - off, p.1);
        }
        rgba(cr, col, a);
        cr.set_line_width((len * 0.0055).max(0.7));
        cr.stroke_q();
    }
    let _ = hh;
}

fn mouth(cr: &Context, sp: &Spine, hh: f64, len: f64, midc: Rgb) {
    let nose = sp.p[0];
    rgba(cr, darken(midc, 0.75), 0.65);
    cr.set_line_width((len * 0.006).max(0.7));
    cr.set_line_cap(cairo::LineCap::Round);
    cr.move_to(nose.0 - len * 0.003, nose.1 + hh * 0.06);
    cr.curve_to(nose.0 - len * 0.02, nose.1 + hh * 0.15, nose.0 - len * 0.045, nose.1 + hh * 0.16, nose.0 - len * 0.07, nose.1 + hh * 0.10);
    cr.stroke_q();
}

fn eye(cr: &Context, sp: &Spine, s: &RSpecies, hh: f64, len: f64, detail: u8, dying: bool, iris: Rgb) {
    let (ex, ey) = at(sp, s.eye_u, 0.26);
    // Los ojos reales son mucho más pequeños y oscuros que los de dibujo.
    let r = (len * 0.026 * s.eye + 0.5).clamp(1.2, 9.0);
    let _ = hh;
    if detail == 0 {
        rgba(cr, (0.03, 0.03, 0.05), 0.95);
        cr.arc(ex, ey, r, 0.0, 2.0 * PI);
        cr.fill_q();
        return;
    }
    if dying {
        rgba(cr, (0.05, 0.05, 0.07), 0.9);
        cr.set_line_width((r * 0.3).max(0.9));
        cr.move_to(ex - r * 0.7, ey - r * 0.7);
        cr.line_to(ex + r * 0.7, ey + r * 0.7);
        cr.move_to(ex + r * 0.7, ey - r * 0.7);
        cr.line_to(ex - r * 0.7, ey + r * 0.7);
        cr.stroke_q();
        return;
    }
    // Párpado/órbita sombreada, iris con degradado, pupila, brillo.
    rgba(cr, (0.02, 0.02, 0.04), 0.45);
    cr.arc(ex, ey, r * 1.18, 0.0, 2.0 * PI);
    cr.fill_q();
    let g = RadialGradient::new(ex - r * 0.2, ey - r * 0.25, r * 0.1, ex, ey, r);
    g.add_color_stop_rgba(0.0, lighten(iris, 0.35).0, lighten(iris, 0.35).1, lighten(iris, 0.35).2, 1.0);
    g.add_color_stop_rgba(0.7, iris.0, iris.1, iris.2, 1.0);
    g.add_color_stop_rgba(1.0, darken(iris, 0.6).0, darken(iris, 0.6).1, darken(iris, 0.6).2, 1.0);
    let _ = cr.set_source(&g);
    cr.arc(ex, ey, r, 0.0, 2.0 * PI);
    cr.fill_q();
    rgba(cr, (0.01, 0.01, 0.02), 1.0);
    cr.arc(ex + r * 0.08, ey, r * 0.58, 0.0, 2.0 * PI);
    cr.fill_q();
    rgba(cr, (1.0, 1.0, 1.0), 0.9);
    cr.arc(ex - r * 0.22, ey - r * 0.30, r * 0.20, 0.0, 2.0 * PI);
    cr.fill_q();
    rgba(cr, (1.0, 1.0, 1.0), 0.35);
    cr.arc(ex + r * 0.30, ey + r * 0.28, r * 0.10, 0.0, 2.0 * PI);
    cr.fill_q();
}
