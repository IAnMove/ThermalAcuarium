//! Pestaña Discos, skin Arcade: ríos. Cada disco es un río que baja serpenteando por la meseta y
//! se despeña en cascada hasta su embalse. El caudal del río es la actividad del disco (lectura y
//! escritura), la cascada es la escritura, el nivel del embalse es la ocupación (con su escala de
//! 0 a 100 % en la presa) y los programas que más leen o escriben son los afluentes que se le unen.

use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::cairo::{self, Context, ImageSurface, RadialGradient};
use gtk::glib;
use gtk::prelude::*;

use crate::draw::{Quiet, lin, rgba};
use crate::i18n::trf;
use crate::reef::noise;
use crate::sim::Rng;
use crate::sys::{Disk, Group, fmt_bytes};
use crate::theme::{Palette, Rgb, darken, lighten, mix};

#[derive(Clone)]
pub struct Tributary {
    pub name: String,
    pub bps: f64,
    pub write: bool,
}

#[derive(Clone)]
pub struct River {
    pub name: String,
    pub mounts: Vec<(String, f64, f64)>,
    rbps: f64,
    wbps: f64,
    /// Suavizados.
    flow: f64,
    fall: f64,
    level: f64,
    level_goal: f64,
}

struct Drop {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    life: f64,
}

pub struct Rivers {
    pub w: f64,
    pub h: f64,
    t: f64,
    pub rivers: Vec<River>,
    pub tribs: Vec<Tributary>,
    drops: Vec<Drop>,
    drop_acc: f64,
    rng: Rng,
    cache_key: RefCell<Option<(i32, i32, u64, usize)>>,
    terrain: RefCell<Option<ImageSurface>>,
    water_tile: RefCell<Option<ImageSurface>>,
}

/// Caudal (bytes/s) → 0–1 en escala logarítmica (1 KB/s → 0, 1 GB/s → 1).
fn flow01(bps: f64) -> f64 {
    ((bps.max(1.0).log10() - 3.0) / 6.0).clamp(0.0, 1.0)
}

/// Baldosa de agua: ruido que se usa como brillo de la superficie.
fn water_tile() -> ImageSurface {
    noise::grey_tile(256, 14, 3, 515, 1.5)
}

impl Rivers {
    pub fn new() -> Rivers {
        Rivers {
            w: 1000.0,
            h: 600.0,
            t: 0.0,
            rivers: vec![],
            tribs: vec![],
            drops: vec![],
            drop_acc: 0.0,
            rng: Rng::new(0x5EA),
            cache_key: RefCell::new(None),
            terrain: RefCell::new(None),
            water_tile: RefCell::new(None),
        }
    }

    pub fn set_disks(&mut self, disks: &[Disk]) {
        let mut out = vec![];
        for d in disks {
            let total = d.total().max(1.0);
            let goal = (d.used() / total).clamp(0.0, 1.0);
            let prev = self.rivers.iter().find(|r| r.name == d.name);
            out.push(River {
                name: d.name.clone(),
                mounts: d.mounts.clone(),
                rbps: d.rbps,
                wbps: d.wbps,
                flow: prev.map_or(flow01(d.rbps + d.wbps), |p| p.flow),
                fall: prev.map_or(flow01(d.wbps), |p| p.fall),
                level: prev.map_or(goal, |p| p.level),
                level_goal: goal,
            });
        }
        self.rivers = out;
    }

    pub fn set_groups(&mut self, groups: &[Group]) {
        let mut v: Vec<Tributary> = groups
            .iter()
            .filter_map(|g| {
                let w: f64 = g.procs.iter().map(|p| p.wbps).sum();
                let r: f64 = g.procs.iter().map(|p| p.rbps).sum();
                if w.max(r) < 2e4 {
                    return None;
                }
                Some(Tributary { name: g.key.clone(), bps: w.max(r), write: w >= r })
            })
            .collect();
        v.sort_by(|a, b| b.bps.partial_cmp(&a.bps).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(6);
        self.tribs = v;
    }

    pub fn resize(&mut self, w: f64, h: f64) {
        if (w - self.w).abs() > 0.5 || (h - self.h).abs() > 0.5 {
            self.w = w;
            self.h = h;
            self.drops.clear();
        }
    }

    // --- geometría: cada disco ocupa un carril vertical

    fn lanes(&self) -> usize {
        self.rivers.len().max(1)
    }
    fn lane_x(&self, i: usize) -> (f64, f64) {
        let lw = self.w / self.lanes() as f64;
        (lw * i as f64, lw * (i + 1) as f64)
    }
    /// Borde del acantilado y fondo del embalse.
    fn cliff_y(&self) -> f64 {
        self.h * 0.58
    }
    fn basin_top(&self) -> f64 {
        self.h * 0.66
    }
    fn basin_bottom(&self) -> f64 {
        self.h * 0.95
    }
    /// Trazado del río en la meseta: de la lejanía (arriba) al borde del acantilado.
    fn river_pts(&self, i: usize) -> Vec<(f64, f64)> {
        let (x0, x1) = self.lane_x(i);
        let cx = (x0 + x1) / 2.0;
        let amp = (x1 - x0) * 0.22;
        let n = 36;
        (0..=n)
            .map(|k| {
                let u = k as f64 / n as f64;
                let y = self.h * 0.06 + (self.cliff_y() - self.h * 0.06) * u;
                let x = cx + amp * ((u * 3.1 + i as f64) * PI).sin() * (1.0 - u * 0.6) + (noise::fbm1(u * 4.0 + i as f64 * 3.0, 2, 61 + i as u32) - 0.5) * amp * 0.8;
                (x, y)
            })
            .collect()
    }

    pub fn step(&mut self, dt: f64) {
        let dt = dt.clamp(0.0, 0.1);
        self.t += dt;
        let k = |r: f64| 1.0 - (-dt * r).exp();
        for r in &mut self.rivers {
            r.flow += (flow01(r.rbps + r.wbps) - r.flow) * k(1.2);
            r.fall += (flow01(r.wbps) - r.fall) * k(1.2);
            r.level += (r.level_goal - r.level) * k(0.8);
        }
        // Salpicaduras al pie de cada cascada.
        let rivers: Vec<(f64, f64, f64)> = (0..self.rivers.len())
            .map(|i| {
                let p = self.river_pts(i);
                let (x, _) = p[p.len() - 1];
                (x, self.basin_top() + (self.basin_bottom() - self.basin_top()) * (1.0 - self.rivers[i].level), self.rivers[i].flow)
            })
            .collect();
        for (x, y, flow) in rivers {
            self.drop_acc += (2.0 + 60.0 * flow) * dt;
            while self.drop_acc >= 1.0 {
                self.drop_acc -= 1.0;
                if self.drops.len() > 500 {
                    break;
                }
                self.drops.push(Drop { x: x + self.rng.gauss() * 10.0, y, vx: self.rng.gauss() * 50.0, vy: -(30.0 + 90.0 * flow) * self.rng.next(), life: self.rng.range(0.4, 1.0) });
            }
        }
        for d in &mut self.drops {
            d.life -= dt;
            d.vy += 300.0 * dt;
            d.x += d.vx * dt;
            d.y += d.vy * dt;
        }
        self.drops.retain(|d| d.life > 0.0);
    }

    // --- dibujo

    fn water(pal: &Palette) -> Rgb {
        mix((0.16, 0.45, 0.62), mix(pal.blue, pal.cyan, 0.5), 0.35)
    }

    /// Meseta, acantilado y presas: estáticos por tamaño/paleta/número de discos.
    fn bake(&self, pal: &Palette) {
        let (w, h) = (self.w, self.h);
        let cy = self.cliff_y();
        let surf = ImageSurface::create(cairo::Format::ARgb32, w.ceil() as i32, h.ceil() as i32).expect("terrain");
        let cr = &Context::new(&surf).expect("cairo");
        let grass = mix((0.24, 0.42, 0.22), mix(pal.green, pal.dbg, 0.5), 0.35);
        let rock = mix((0.36, 0.32, 0.28), mix(pal.dfg, pal.dbg, 0.5), 0.35);
        // Meseta: verde con relieve de ruido y bruma hacia el horizonte.
        let _ = cr.set_source(lin(0.0, 0.0, 0.0, cy, &[(0.0, mix(grass, pal.bg, 0.55), 1.0), (0.35, grass, 1.0), (1.0, darken(grass, 0.15), 1.0)]));
        cr.rectangle(0.0, 0.0, w, cy);
        cr.fill_q();
        let relief = noise::grey_tile(256, 6, 4, 303, 1.4);
        let pat = cairo::SurfacePattern::create(&relief);
        pat.set_extend(cairo::Extend::Repeat);
        let mut m = cairo::Matrix::identity();
        m.scale(0.6, 0.6);
        pat.set_matrix(m);
        cr.save_q();
        cr.rectangle(0.0, 0.0, w, cy);
        cr.clip();
        cr.set_operator(cairo::Operator::SoftLight);
        let _ = cr.set_source(&pat);
        let _ = cr.paint_with_alpha(0.8);
        cr.set_operator(cairo::Operator::Over);
        // Bosque: manchas oscuras de árboles, más densas lejos del agua.
        for i in 0..260u64 {
            let x = noise::rnd(i as i32, 1, 5) * w;
            let y = noise::rnd(i as i32, 2, 5).powf(0.7) * cy * 0.95;
            let near_river = (0..self.lanes()).any(|l| {
                let p = self.river_pts(l);
                p.iter().any(|q| (q.0 - x).abs() < 22.0 && (q.1 - y).abs() < 22.0)
            });
            if near_river {
                continue;
            }
            let s = (2.5 + 4.0 * noise::rnd(i as i32, 3, 5)) * (0.4 + 0.6 * y / cy);
            rgba(cr, darken(grass, 0.55), 0.35);
            cr.arc(x + s * 0.5, y + s * 0.4, s, 0.0, 2.0 * PI);
            cr.fill_q();
            rgba(cr, darken(grass, 0.3), 0.9);
            cr.move_to(x, y - s * 1.6);
            cr.line_to(x + s * 0.9, y + s * 0.5);
            cr.line_to(x - s * 0.9, y + s * 0.5);
            cr.close_path();
            cr.fill_q();
            rgba(cr, lighten(grass, 0.25), 0.6);
            cr.move_to(x, y - s * 1.6);
            cr.line_to(x - s * 0.9, y + s * 0.5);
            cr.line_to(x - s * 0.2, y + s * 0.5);
            cr.close_path();
            cr.fill_q();
        }
        cr.restore_q();
        // Acantilado: pared de roca estratificada.
        let bt = self.basin_top();
        let _ = cr.set_source(lin(0.0, cy, 0.0, bt + 20.0, &[(0.0, lighten(rock, 0.1), 1.0), (1.0, darken(rock, 0.45), 1.0)]));
        cr.rectangle(0.0, cy, w, bt + 20.0 - cy);
        cr.fill_q();
        for i in 0..7u64 {
            let y0 = cy + (bt + 20.0 - cy) * (i as f64 + 0.5) / 7.0;
            rgba(cr, darken(rock, 0.6), 0.35);
            cr.set_line_width(1.5);
            cr.move_to(0.0, y0);
            let mut x = 0.0;
            while x <= w {
                cr.line_to(x, y0 + (noise::fbm1(x * 0.02 + i as f64 * 4.0, 2, 71) - 0.5) * 6.0);
                x += 8.0;
            }
            cr.stroke_q();
        }
        rgba(cr, lighten(grass, 0.2), 0.9);
        cr.rectangle(0.0, cy - 2.0, w, 3.0);
        cr.fill_q();
        // Cuencas: fondo oscuro y presa (muro) a la derecha de cada carril.
        let bb = self.basin_bottom();
        for i in 0..self.lanes() {
            let (x0, x1) = self.lane_x(i);
            let _ = cr.set_source(lin(0.0, bt, 0.0, bb, &[(0.0, darken(rock, 0.5), 1.0), (1.0, darken(rock, 0.75), 1.0)]));
            cr.rectangle(x0, bt, x1 - x0, bb - bt);
            cr.fill_q();
            let dam = mix(rock, pal.dfg, 0.25);
            let _ = cr.set_source(lin(x1 - 22.0, 0.0, x1, 0.0, &[(0.0, lighten(dam, 0.15), 1.0), (1.0, darken(dam, 0.3), 1.0)]));
            cr.rectangle(x1 - 22.0, bt - 10.0, 22.0, bb - bt + 10.0);
            cr.fill_q();
            rgba(cr, darken(dam, 0.5), 0.6);
            for k in 0..6 {
                let y = bt - 10.0 + (bb - bt + 10.0) * k as f64 / 6.0;
                cr.rectangle(x1 - 22.0, y, 22.0, 1.0);
            }
            cr.fill_q();
        }
        // Suelo por debajo de las cuencas.
        rgba(cr, darken(pal.dbg, 0.3), 1.0);
        cr.rectangle(0.0, bb, w, h - bb);
        cr.fill_q();
        *self.terrain.borrow_mut() = Some(surf);
        *self.water_tile.borrow_mut() = Some(water_tile());
    }

    pub fn draw(&self, cr: &Context, pal: &Palette, font: &str) {
        let (w, h) = (self.w, self.h);
        let key = (w as i32, h as i32, crate::sim::hash_str(&pal.raw), self.rivers.len());
        if *self.cache_key.borrow() != Some(key) {
            self.bake(pal);
            *self.cache_key.borrow_mut() = Some(key);
        }
        let tile_ref = self.water_tile.borrow();
        let Some(tile) = tile_ref.as_ref() else { return };
        let water = Self::water(pal);
        let foam = mix((0.92, 0.97, 1.0), pal.bfg, 0.2);
        let sc = (w / 1000.0).max(0.6);
        cr.select_font_face(font, cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        let fs = (11.0 * sc).clamp(10.0, 14.0);
        cr.set_font_size(fs);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Round);

        // Cielo y terreno.
        let _ = cr.set_source(lin(0.0, 0.0, 0.0, h * 0.1, &[(0.0, mix(pal.dbg, pal.blue, 0.2), 1.0), (1.0, mix(pal.bg, pal.blue, 0.25), 1.0)]));
        cr.rectangle(0.0, 0.0, w, h * 0.1);
        cr.fill_q();
        if let Some(t) = self.terrain.borrow().as_ref() {
            let _ = cr.set_source_surface(t, 0.0, 0.0);
            cr.paint_q();
        }
        let cy = self.cliff_y();
        let (bt, bb) = (self.basin_top(), self.basin_bottom());
        let water_pat = |scale: f64, dx: f64, dy: f64| {
            let p = cairo::SurfacePattern::create(tile);
            p.set_extend(cairo::Extend::Repeat);
            let mut m = cairo::Matrix::identity();
            m.scale(1.0 / scale, 1.0 / scale);
            m.translate(-dx, -dy);
            p.set_matrix(m);
            p
        };

        for (i, r) in self.rivers.iter().enumerate() {
            let (x0, x1) = self.lane_x(i);
            let pts = self.river_pts(i);
            let wd = (4.0 + 44.0 * r.flow) * sc;
            let path = |cr: &Context| {
                cr.move_to(pts[0].0, pts[0].1);
                for p in &pts[1..] {
                    cr.line_to(p.0, p.1);
                }
            };
            // Orillas y cauce.
            path(cr);
            rgba(cr, darken(Self::water(pal), 0.6), 0.6);
            cr.set_line_width(wd + 6.0 * sc);
            cr.stroke_q();
            path(cr);
            let _ = cr.set_source(lin(0.0, 0.0, 0.0, cy, &[(0.0, mix(water, pal.bg, 0.4), 1.0), (1.0, water, 1.0)]));
            cr.set_line_width(wd);
            cr.stroke_q();
            // Brillo de la corriente: la textura baja con el río, más rápido cuanto más caudal.
            path(cr);
            cr.set_operator(cairo::Operator::Screen);
            let _ = cr.set_source(water_pat(0.9 * sc, 0.0, self.t * (20.0 + 140.0 * r.flow)));
            cr.set_line_width(wd * 0.8);
            let _ = cr.stroke_preserve();
            cr.set_operator(cairo::Operator::Over);
            cr.new_path();
            // Espuma en las orillas cuando hay mucho caudal.
            if r.flow > 0.35 {
                path(cr);
                rgba(cr, foam, 0.25 * (r.flow - 0.35) / 0.65);
                cr.set_line_width(wd);
                cr.set_dash(&[6.0, 10.0], self.t * 60.0);
                cr.stroke_q();
                cr.set_dash(&[], 0.0);
            }

            // Afluentes: los programas que más leen/escriben, desde los lados del carril.
            let my_tribs: Vec<&Tributary> = self.tribs.iter().enumerate().filter(|(k, _)| k % self.lanes() == i).map(|(_, t)| t).collect();
            for (k, t) in my_tribs.iter().enumerate() {
                let u = 0.25 + 0.5 * (k as f64 + 0.5) / my_tribs.len().max(1) as f64;
                let j = ((pts.len() - 1) as f64 * u) as usize;
                let (jx, jy) = pts[j];
                let side = if k % 2 == 0 { -1.0 } else { 1.0 };
                let sx = if side < 0.0 { x0 + 10.0 } else { x1 - 32.0 };
                let tw = (1.5 + 10.0 * flow01(t.bps)) * sc;
                let col = if t.write { mix(water, foam, 0.35) } else { water };
                cr.move_to(sx, jy - 40.0 * sc);
                cr.curve_to(sx + (jx - sx) * 0.3, jy - 30.0 * sc, jx - (jx - sx) * 0.2, jy - 8.0, jx, jy);
                rgba(cr, darken(col, 0.5), 0.6);
                cr.set_line_width(tw + 3.0);
                let _ = cr.stroke_preserve();
                rgba(cr, col, 1.0);
                cr.set_line_width(tw);
                cr.stroke_q();
                let txt = format!("{} {} {}/s", if t.write { "↑" } else { "↓" }, t.name, fmt_bytes(t.bps));
                if let Ok(e) = cr.text_extents(&txt) {
                    let x = if side < 0.0 { sx } else { (sx - e.width()).max(x0 + 4.0) };
                    rgba(cr, pal.dbg, 0.6);
                    cr.rectangle(x - 4.0, jy - 44.0 * sc - fs - 1.0, e.width() + 8.0, fs + 6.0);
                    cr.fill_q();
                    rgba(cr, lighten(pal.bfg, 0.1), 1.0);
                    cr.move_to(x, jy - 44.0 * sc);
                    let _ = cr.show_text(&txt);
                }
            }

            // Embalse: agua hasta el nivel de ocupación.
            let lvl = bt + (bb - bt) * (1.0 - r.level);
            let (fx, _) = pts[pts.len() - 1];
            let _ = cr.set_source(lin(0.0, lvl, 0.0, bb, &[(0.0, lighten(water, 0.1), 1.0), (1.0, darken(water, 0.55), 1.0)]));
            cr.rectangle(x0, lvl, x1 - 22.0 - x0, bb - lvl);
            cr.fill_q();
            cr.save_q();
            cr.rectangle(x0, lvl, x1 - 22.0 - x0, bb - lvl);
            cr.clip();
            cr.set_operator(cairo::Operator::Screen);
            let _ = cr.set_source(water_pat(1.6 * sc, self.t * 8.0, self.t * 2.0));
            let _ = cr.paint_with_alpha(0.35);
            cr.set_operator(cairo::Operator::Over);
            // Reflejo de la cascada en el agua.
            let g = RadialGradient::new(fx, lvl, 0.0, fx, lvl, 60.0 * sc);
            g.add_color_stop_rgba(0.0, foam.0, foam.1, foam.2, 0.35 * r.flow);
            g.add_color_stop_rgba(1.0, foam.0, foam.1, foam.2, 0.0);
            let _ = cr.set_source(&g);
            cr.paint_q();
            cr.restore_q();
            // Superficie: línea ondulada.
            rgba(cr, lighten(water, 0.5), 0.7);
            cr.set_line_width(1.5);
            let mut x = x0;
            cr.move_to(x, lvl);
            while x <= x1 - 22.0 {
                cr.line_to(x, lvl + (x * 0.08 + self.t * 2.0).sin() * 1.5);
                x += 6.0;
            }
            cr.stroke_q();

            // Cascada: del borde del acantilado al agua, con la intensidad de la escritura.
            let fw = (3.0 + 40.0 * r.fall.max(r.flow * 0.4)) * sc;
            let _ = cr.set_source(lin(fx - fw / 2.0, 0.0, fx + fw / 2.0, 0.0, &[(0.0, water, 0.0), (0.2, foam, 0.75), (0.5, water, 0.95), (0.8, foam, 0.75), (1.0, water, 0.0)]));
            cr.rectangle(fx - fw / 2.0, cy - 2.0, fw, lvl - cy + 4.0);
            cr.fill_q();
            cr.save_q();
            cr.rectangle(fx - fw / 2.0, cy, fw, lvl - cy);
            cr.clip();
            cr.set_operator(cairo::Operator::Screen);
            let _ = cr.set_source(water_pat(0.6 * sc, 0.0, self.t * (120.0 + 300.0 * r.fall)));
            let _ = cr.paint_with_alpha(0.6);
            cr.set_operator(cairo::Operator::Over);
            cr.restore_q();
            // Bruma al pie.
            let g = RadialGradient::new(fx, lvl, 0.0, fx, lvl, (30.0 + 50.0 * r.flow) * sc);
            g.add_color_stop_rgba(0.0, foam.0, foam.1, foam.2, 0.55 * (0.3 + 0.7 * r.flow));
            g.add_color_stop_rgba(1.0, foam.0, foam.1, foam.2, 0.0);
            let _ = cr.set_source(&g);
            cr.arc(fx, lvl, (30.0 + 50.0 * r.flow) * sc, 0.0, 2.0 * PI);
            cr.fill_q();

            // Escala de la presa: 0–100 % de ocupación.
            let xs = x1 - 22.0;
            rgba(cr, pal.bfg, 0.8);
            cr.set_line_width(1.0);
            cr.set_font_size(fs * 0.8);
            for k in 0..=10 {
                let y = (bt + (bb - bt) * (1.0 - k as f64 / 10.0)).round() + 0.5;
                let major = k % 5 == 0;
                cr.move_to(xs - 1.0, y);
                cr.line_to(xs - if major { 10.0 } else { 6.0 }, y);
                cr.stroke_q();
                if major {
                    let t = format!("{}%", k * 10);
                    if let Ok(e) = cr.text_extents(&t) {
                        cr.move_to(xs - 13.0 - e.width(), y + fs * 0.3);
                        let _ = cr.show_text(&t);
                    }
                }
            }
            cr.set_font_size(fs);
            // Marcador del nivel y cifra.
            rgba(cr, foam, 0.95);
            cr.move_to(xs - 1.0, lvl);
            cr.line_to(xs - 9.0, lvl - 5.0);
            cr.line_to(xs - 9.0, lvl + 5.0);
            cr.close_path();
            cr.fill_q();
            let pct = format!("{:.0}%", r.level * 100.0);
            cr.set_font_size(fs + 3.0);
            if let Ok(e) = cr.text_extents(&pct) {
                let x = (xs - e.width() - 48.0 * sc).max(x0 + 4.0);
                rgba(cr, pal.dbg, 0.7);
                cr.rectangle(x - 5.0, lvl - fs - 6.0, e.width() + 10.0, fs + 12.0);
                cr.fill_q();
                rgba(cr, foam, 1.0);
                cr.move_to(x, lvl + fs * 0.4);
                let _ = cr.show_text(&pct);
            }
            cr.set_font_size(fs);
            // Ficha del disco arriba del carril.
            let lines = [
                format!("{}  ·  {} / {}", r.name, fmt_bytes(r.mounts.iter().map(|m| m.1).sum()), fmt_bytes(r.mounts.iter().map(|m| m.2).sum())),
                format!("↓ {}/s   ↑ {}/s", fmt_bytes(r.rbps), fmt_bytes(r.wbps)),
                r.mounts.iter().map(|m| m.0.as_str()).collect::<Vec<_>>().join("  "),
            ];
            for (k, l) in lines.iter().enumerate() {
                if let Ok(e) = cr.text_extents(l) {
                    let x = x0 + 10.0;
                    let y = 10.0 + (fs + 8.0) * k as f64;
                    rgba(cr, pal.dbg, 0.6);
                    cr.rectangle(x - 4.0, y, e.width().min(x1 - x0 - 20.0) + 8.0, fs + 8.0);
                    cr.fill_q();
                    cr.save_q();
                    cr.rectangle(x0, y, x1 - x0 - 14.0, fs + 8.0);
                    cr.clip();
                    rgba(cr, if k == 0 { lighten(pal.bfg, 0.1) } else { pal.fg }, 1.0);
                    cr.move_to(x, y + fs + 2.0);
                    let _ = cr.show_text(l);
                    cr.restore_q();
                }
            }
            let _ = trf;
        }
        // Salpicaduras.
        for d in &self.drops {
            rgba(cr, foam, d.life.min(1.0) * 0.8);
            cr.arc(d.x, d.y, 1.3 * sc, 0.0, 2.0 * PI);
            cr.fill_q();
        }
        if self.rivers.is_empty() {
            rgba(cr, pal.dfg, 1.0);
            cr.move_to(20.0, 40.0);
            let _ = cr.show_text("…");
        }
    }
}

pub fn widget(v: Rc<RefCell<Rivers>>, pal: Rc<RefCell<Palette>>, font: String) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    let (v2, p2) = (v.clone(), pal.clone());
    area.set_draw_func(move |_, cr, w, h| {
        let mut r = v2.borrow_mut();
        r.resize(w as f64, h as f64);
        r.draw(cr, &p2.borrow(), &font);
    });
    let last: Rc<RefCell<Option<i64>>> = Rc::new(RefCell::new(None));
    area.add_tick_callback(move |a, clock| {
        let now = clock.frame_time();
        let mut l = last.borrow_mut();
        let dt = l.map_or(0.0, |t| (now - t) as f64 / 1e6);
        if l.is_some() && dt < 1.0 / 31.0 {
            return glib::ControlFlow::Continue;
        }
        *l = Some(now);
        v.borrow_mut().step(dt);
        a.queue_draw();
        glib::ControlFlow::Continue
    });
    area
}
