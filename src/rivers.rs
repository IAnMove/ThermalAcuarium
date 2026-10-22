//! Pestaña Discos, skin Arcade: ríos. Cada disco es un cauce que baja por un valle de la meseta y
//! se despeña en cascada hasta su embalse. El cauce entero es el disco: el agua lo llena en la
//! proporción usada (desde la orilla izquierda) y el lecho seco que queda a la derecha es el espacio
//! libre. La actividad del disco (lectura + escritura, escala logarítmica) es la velocidad de la
//! corriente y la fuerza de la cascada; el embalse repite la ocupación con su escala en la presa, y
//! los programas que más leen o escriben son los afluentes. El paisaje es un mapa de alturas iluminado, con los valles excavados
//! por los ríos, bosque en las laderas y bruma hacia el horizonte.

use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::cairo::{self, Context, ImageSurface};
use gtk::glib;
use gtk::prelude::*;

use crate::draw::{Quiet, lin, rgba};
use crate::reef::noise;
use crate::sim::Rng;
use crate::stamps::{ScaledTiles, Stamps};
use crate::sys::{Disk, Group, fmt_bytes};
use crate::theme::{Palette, Rgb, darken, lighten, mix};

/// Afluente: un programa que lee o escribe. Ocupa un hueco fijo del paisaje; si deja de hacer
/// E/S se queda en su sitio con el cauce seco (caudal cero) en vez de desaparecer.
#[derive(Clone)]
pub struct Tributary {
    pub name: String,
    pub bps: f64,
    pub write: bool,
    /// Caudal suavizado (0–1), lo que se dibuja.
    pub act: f64,
    /// Segundos seguidos sin E/S.
    pub idle: f64,
}

/// Huecos de afluente en todo el paisaje.
const TRIB_SLOTS: usize = 6;
/// Un hueco seco solo se cede a un programa nuevo tras este tiempo sin E/S.
const TRIB_IDLE_FREE: f64 = 25.0;

#[derive(Clone)]
pub struct River {
    pub name: String,
    pub mounts: Vec<(String, f64, f64)>,
    rbps: f64,
    wbps: f64,
    flow: f64,
    fall: f64,
    level: f64,
    level_goal: f64,
}

/// Un afluente ya trazado.
struct TribPath {
    pts: Vec<(f64, f64)>,
    /// Semiancho del agua y del cauce en cada punto.
    hw_w: Vec<f64>,
    hw_b: Vec<f64>,
    act: f64,
    write: bool,
    name: String,
    bps: f64,
    side: f64,
    spring: (f64, f64),
    conf: (f64, f64),
    /// Hueco sin programa todavía: no se dibuja nada.
    empty: bool,
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
    pub tribs: Vec<Option<Tributary>>,
    drops: Vec<Drop>,
    drop_acc: f64,
    rng: Rng,
    cache_key: RefCell<Option<(i32, i32, u64, usize)>>,
    terrain: RefCell<Option<ImageSurface>>,
    /// Rizos del río (alfa horneado), los mismos más tenues para el embalse, cáusticas del embalse
    /// y chorros de la cascada.
    water_tile: RefCell<Option<ScaledTiles>>,
    water_tile2: RefCell<Option<ScaledTiles>>,
    caustic_tile: RefCell<Option<ScaledTiles>>,
    streak_tile: RefCell<Option<ScaledTiles>>,
    /// Afluentes horneados (todo menos la corriente que se mueve).
    trib_cache: RefCell<Option<((u64, i32, i32, i32, u64), ImageSurface)>>,
    stamp: u64,
    blobs: Stamps,
}

/// Caudal (bytes/s) → 0–1 en escala logarítmica (1 KB/s → 0, 1 GB/s → 1).
fn flow01(bps: f64) -> f64 {
    ((bps.max(1.0).log10() - 3.0) / 6.0).clamp(0.0, 1.0)
}

/// Distancia de un punto a una polilínea.
fn dist_poly(x: f64, y: f64, pts: &[(f64, f64)]) -> f64 {
    let mut best = f64::MAX;
    for s in pts.windows(2) {
        let (ax, ay, bx, by) = (s[0].0, s[0].1, s[1].0, s[1].1);
        let (dx, dy) = (bx - ax, by - ay);
        let l2 = (dx * dx + dy * dy).max(1e-6);
        let t = (((x - ax) * dx + (y - ay) * dy) / l2).clamp(0.0, 1.0);
        let (px, py) = (ax + dx * t, ay + dy * t);
        best = best.min(((x - px).powi(2) + (y - py).powi(2)).sqrt());
    }
    best
}

/// Hierba agostada: el color del cauce seco de un afluente sin caudal.
fn grass_dry(pal: &Palette) -> Rgb {
    mix((0.62, 0.60, 0.34), mix(pal.yellow, pal.dbg, 0.5), 0.3)
}

fn put(d: &mut [u8], i: usize, c: Rgb) {
    d[i] = (c.2.clamp(0.0, 1.0) * 255.0) as u8;
    d[i + 1] = (c.1.clamp(0.0, 1.0) * 255.0) as u8;
    d[i + 2] = (c.0.clamp(0.0, 1.0) * 255.0) as u8;
    d[i + 3] = 255;
}

impl Rivers {
    pub fn new() -> Rivers {
        Rivers {
            w: 1000.0,
            h: 600.0,
            t: 0.0,
            rivers: vec![],
            tribs: vec![None; TRIB_SLOTS],
            drops: vec![],
            drop_acc: 0.0,
            rng: Rng::new(0x5EA),
            cache_key: RefCell::new(None),
            terrain: RefCell::new(None),
            water_tile: RefCell::new(None),
            water_tile2: RefCell::new(None),
            caustic_tile: RefCell::new(None),
            streak_tile: RefCell::new(None),
            trib_cache: RefCell::new(None),
            stamp: 0,
            blobs: Stamps::new(2, 2600),
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
        let mut cands: Vec<(String, f64, bool)> = groups
            .iter()
            .filter_map(|g| {
                let w: f64 = g.procs.iter().map(|p| p.wbps).sum();
                let r: f64 = g.procs.iter().map(|p| p.rbps).sum();
                if w.max(r) < 2e4 {
                    return None;
                }
                Some((g.key.clone(), w.max(r), w >= r))
            })
            .collect();
        cands.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // Los que ya tienen hueco actualizan su caudal (o se quedan secos).
        for slot in self.tribs.iter_mut().flatten() {
            match cands.iter().find(|c| c.0 == slot.name) {
                Some(c) => {
                    slot.bps = c.1;
                    slot.write = c.2;
                    slot.idle = 0.0;
                }
                None => slot.bps = 0.0,
            }
        }
        // Los nuevos ocupan huecos vacíos; si no hay, el hueco que lleve más tiempo seco.
        for c in cands.iter().take(TRIB_SLOTS) {
            if self.tribs.iter().flatten().any(|s| s.name == c.0) {
                continue;
            }
            let fresh = Tributary { name: c.0.clone(), bps: c.1, write: c.2, act: 0.0, idle: 0.0 };
            if let Some(i) = self.tribs.iter().position(|s| s.is_none()) {
                self.tribs[i] = Some(fresh);
            } else if let Some(i) = (0..TRIB_SLOTS)
                .filter(|&i| self.tribs[i].as_ref().is_some_and(|s| s.idle > TRIB_IDLE_FREE))
                .max_by(|&a, &b| self.tribs[a].as_ref().unwrap().idle.partial_cmp(&self.tribs[b].as_ref().unwrap().idle).unwrap_or(std::cmp::Ordering::Equal))
            {
                self.tribs[i] = Some(fresh);
            }
        }
        self.stamp += 1;
    }

    pub fn resize(&mut self, w: f64, h: f64) {
        if (w - self.w).abs() > 0.5 || (h - self.h).abs() > 0.5 {
            self.w = w;
            self.h = h;
            self.drops.clear();
        }
    }

    // --- geometría

    fn lanes(&self) -> usize {
        self.rivers.len().max(1)
    }
    fn lane_x(&self, i: usize) -> (f64, f64) {
        let lw = self.w / self.lanes() as f64;
        (lw * i as f64, lw * (i + 1) as f64)
    }
    fn sky_y(&self) -> f64 {
        self.h * 0.14
    }
    fn cliff_y(&self) -> f64 {
        self.h * 0.60
    }
    fn basin_top(&self) -> f64 {
        self.h * 0.70
    }
    fn basin_bottom(&self) -> f64 {
        self.h * 0.96
    }
    /// Ancho del cauce (el disco entero) en un carril.
    fn bed_w(&self, i: usize) -> f64 {
        let (x0, x1) = self.lane_x(i);
        ((x1 - x0) * 0.34).clamp(40.0, 180.0)
    }
    fn dam_w(&self) -> f64 {
        (26.0 * (self.w / 1000.0)).max(18.0)
    }
    /// Semiancho del cauce a lo largo del río (perspectiva: un hilo en el horizonte, ancho cerca).
    fn half_at(&self, i: usize, u: f64) -> f64 {
        self.bed_w(i) / 2.0 * (0.04 + 0.96 * u.powf(1.15))
    }

    /// Factor de perspectiva a la altura `y` de la meseta: 0 en el horizonte, 1 en el acantilado.
    fn depth(&self, y: f64) -> f64 {
        ((y - self.sky_y()) / (self.cliff_y() - self.sky_y())).clamp(0.0, 1.0)
    }

    /// Trazado del río en la meseta, del horizonte al borde del acantilado.
    fn river_pts(&self, i: usize) -> Vec<(f64, f64)> {
        let (x0, x1) = self.lane_x(i);
        let cx = (x0 + x1) / 2.0;
        let amp = (x1 - x0) * 0.07;
        let n = 48;
        (0..=n)
            .map(|k| {
                let u = k as f64 / n as f64;
                // Nace en el horizonte y baja con la perspectiva (más apretado a lo lejos).
                let y0 = self.sky_y() + 1.0;
                let y = y0 + (self.cliff_y() - y0) * u.powf(1.25);
                let x = cx + amp * ((u * 1.6 + i as f64 * 0.7) * PI).sin() * (0.3 + 0.7 * u) + (noise::fbm1(u * 2.5 + i as f64 * 3.0, 2, 61 + i as u32) - 0.5) * amp * 0.6 * u;
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
        for tb in self.tribs.iter_mut().flatten() {
            tb.act += (flow01(tb.bps) - tb.act) * k(0.7);
            if tb.bps <= 0.0 {
                tb.idle += dt;
            }
        }
        let spots: Vec<(f64, f64, f64)> = (0..self.rivers.len())
            .map(|i| {
                let p = self.river_pts(i);
                let (x, _) = p[p.len() - 1];
                (x, self.basin_top() + (self.basin_bottom() - self.basin_top()) * (1.0 - self.rivers[i].level), self.rivers[i].flow)
            })
            .collect();
        for (x, y, flow) in spots {
            self.drop_acc += (3.0 + 80.0 * flow) * dt;
            while self.drop_acc >= 1.0 {
                self.drop_acc -= 1.0;
                if self.drops.len() > 600 {
                    break;
                }
                self.drops.push(Drop { x: x + self.rng.gauss() * 12.0, y, vx: self.rng.gauss() * 60.0, vy: -(40.0 + 120.0 * flow) * self.rng.next(), life: self.rng.range(0.4, 1.1) });
            }
        }
        for d in &mut self.drops {
            d.life -= dt;
            d.vy += 320.0 * dt;
            d.x += d.vx * dt;
            d.y += d.vy * dt;
        }
        self.drops.retain(|d| d.life > 0.0);
    }

    // --- dibujo

    fn water(pal: &Palette) -> Rgb {
        mix((0.14, 0.42, 0.60), mix(pal.blue, pal.cyan, 0.5), 0.3)
    }

    fn haze(pal: &Palette) -> Rgb {
        mix(lighten(mix(pal.blue, pal.bg, 0.5), 0.3), (0.80, 0.86, 0.90), 0.5)
    }

    /// Mapa de alturas de la meseta: colinas con los valles de los ríos excavados. El ruido se
    /// muestrea en un espacio en perspectiva (lejos las colinas se ven más pequeñas) con la misma
    /// escala en x y en y, para que el relieve no salga a rayas.
    fn height(&self, x: f64, y: f64, rivers: &[Vec<(f64, f64)>]) -> f64 {
        let l = (self.cliff_y() - self.sky_y()).max(1.0);
        let v = (y - self.sky_y()).clamp(0.0, l);
        let u = v / l;
        // Profundidad: factor de 2.2 en el horizonte a 1 en el borde; integrado en y para que la
        // coordenada sea monótona.
        let f = 1.0 + 1.2 * (1.0 - u);
        let wy = v + 1.2 * (v - v * v / (2.0 * l));
        let wx = (x - self.w / 2.0) * f;
        let cell = 150.0;
        let base = noise::fbm(wx / cell, wy / cell, 4, 404, 0);
        let fine = noise::fbm(wx / (cell * 0.35) + 7.0, wy / (cell * 0.35), 2, 405, 0);
        let mut h = 0.30 + 0.60 * base + 0.14 * (fine - 0.5);
        let vw = self.w * 0.085 * (0.25 + 0.75 * self.depth(y));
        for r in rivers {
            let d = dist_poly(x, y, r);
            let valley = (1.0 - (d / vw).min(1.0)).powi(2);
            h -= 0.55 * valley;
        }
        h
    }

    /// Lecho del río (orillas, grava y piedras): estático, se hornea con el terreno.
    fn draw_bed(&self, cr: &Context, i: usize, pal: &Palette, tile: &ImageSurface) {
        let pts = self.river_pts(i);
        let n = pts.len();
        let sc = (self.w / 1000.0).max(0.6);
        let hw = |k: usize| self.half_at(i, k as f64 / (n - 1) as f64);
        let left = |k: usize| pts[k].0 - hw(k);
        let right = |k: usize| pts[k].0 + hw(k);
        let poly = |cr: &Context, a: &dyn Fn(usize) -> f64, b: &dyn Fn(usize) -> f64| {
            cr.move_to(a(0), pts[0].1);
            for k in 1..n {
                cr.line_to(a(k), pts[k].1);
            }
            for k in (0..n).rev() {
                cr.line_to(b(k), pts[k].1);
            }
            cr.close_path();
        };
        let pat = cairo::SurfacePattern::create(tile);
        pat.set_extend(cairo::Extend::Repeat);
        let mut m = cairo::Matrix::identity();
        m.scale(1.0 / (0.6 * sc), 1.0 / (0.6 * sc));
        pat.set_matrix(m);
        poly(cr, &|k| left(k) - 5.0 * sc, &|k| right(k) + 5.0 * sc);
        rgba(cr, (0.20, 0.16, 0.11), 0.75);
        cr.fill_q();
        let gravel = mix((0.66, 0.60, 0.48), mix(pal.dfg, pal.bg, 0.4), 0.3);
        poly(cr, &left, &right);
        rgba(cr, gravel, 1.0);
        cr.fill_keep();
        cr.save_q();
        cr.clip();
        cr.set_operator(cairo::Operator::SoftLight);
        let _ = cr.set_source(&pat);
        cr.paint_q();
        cr.set_operator(cairo::Operator::Over);
        for k in 0..60u64 {
            let kk = ((noise::rnd(k as i32, 1, 300 + i as u32)) * (n - 1) as f64) as usize;
            let x = left(kk) + (right(kk) - left(kk)) * noise::rnd(k as i32, 2, 300 + i as u32);
            let y = pts[kk].1;
            let rr = (1.0 + 2.5 * noise::rnd(k as i32, 3, 300 + i as u32)) * sc * (0.3 + 0.7 * kk as f64 / n as f64);
            rgba(cr, darken(gravel, 0.45), 0.7);
            cr.arc(x, y, rr, 0.0, 2.0 * PI);
            cr.fill_q();
        }
        cr.restore_q();
    }

    /// Paisaje estático: cielo, montañas, meseta iluminada, acantilado, cuencas y presas.
    fn bake(&self, pal: &Palette) {
        let (w, h) = (self.w, self.h);
        let (sy, cy, bt, bb) = (self.sky_y(), self.cliff_y(), self.basin_top(), self.basin_bottom());
        let rivers: Vec<Vec<(f64, f64)>> = (0..self.lanes()).map(|i| self.river_pts(i)).collect();
        let grass = mix((0.33, 0.52, 0.24), mix(pal.green, pal.dbg, 0.4), 0.3);
        let dry = mix((0.62, 0.60, 0.34), mix(pal.yellow, pal.dbg, 0.5), 0.3);
        let rock = mix((0.42, 0.38, 0.34), mix(pal.dfg, pal.dbg, 0.5), 0.35);
        let forest = darken(mix(grass, (0.08, 0.25, 0.12), 0.6), 0.1);
        let wet = darken(mix(grass, (0.25, 0.22, 0.14), 0.5), 0.2);
        let haze = Self::haze(pal);

        // Meseta por píxel, a media resolución.
        let q = 2.0;
        let (pw, ph) = ((w / q).ceil() as i32, ((cy - sy) / q).ceil() as i32 + 2);
        let mut img = ImageSurface::create(cairo::Format::ARgb32, pw, ph).expect("terrain");
        let stride = img.stride() as usize;
        {
            let mut d = img.data().expect("terrain data");
            let (lx, ly) = (-0.6, -0.8);
            for py in 0..ph as usize {
                for px in 0..pw as usize {
                    let (x, y) = (px as f64 * q, sy + py as f64 * q);
                    let hc = self.height(x, y, &rivers);
                    let hx = self.height(x + 2.0 * q, y, &rivers);
                    let hy = self.height(x, y + 2.0 * q, &rivers);
                    let (dx, dy) = ((hx - hc) * 22.0, (hy - hc) * 22.0);
                    let slope = (dx * dx + dy * dy).sqrt();
                    let shade = (0.60 + 0.75 * (-dx * lx - dy * ly).clamp(-0.6, 0.8)).clamp(0.22, 1.30);
                    let dmin = rivers.iter().map(|r| dist_poly(x, y, r)).fold(f64::MAX, f64::min);
                    let fnoise = noise::fbm(x / w * 9.0, y / h * 12.0, 3, 808, 0);
                    let mut c = mix(grass, dry, ((hc - 0.45) / 0.5).clamp(0.0, 1.0));
                    let ww = w * 0.03 * (0.3 + 0.7 * self.depth(y));
                    if dmin < ww {
                        c = mix(wet, c, (dmin / ww).powf(0.7));
                    }
                    if fnoise > 0.56 && dmin > w * 0.035 && hc < 0.9 {
                        c = mix(c, forest, ((fnoise - 0.56) * 6.0).clamp(0.0, 1.0));
                    }
                    if slope > 0.9 {
                        c = mix(c, rock, ((slope - 0.9) * 1.5).clamp(0.0, 1.0));
                    }
                    let mut col = (c.0 * shade, c.1 * shade, c.2 * shade);
                    // Bruma hacia el horizonte.
                    let far = 1.0 - ((y - sy) / (cy - sy)).clamp(0.0, 1.0);
                    col = mix(col, haze, 0.42 * far.powf(1.8));
                    put(&mut d, py * stride + px * 4, col);
                }
            }
        }
        let surf = ImageSurface::create(cairo::Format::ARgb32, w.ceil() as i32, h.ceil() as i32).expect("scene");
        let cr = &Context::new(&surf).expect("cairo");
        // Cielo y montañas lejanas.
        let _ = cr.set_source(lin(0.0, 0.0, 0.0, sy, &[(0.0, mix(pal.dbg, pal.blue, 0.35), 1.0), (1.0, haze, 1.0)]));
        cr.rectangle(0.0, 0.0, w, sy + 2.0);
        cr.fill_q();
        for (i, amp, k) in [(0u32, 0.75, 0.2), (1, 0.55, 0.45), (2, 0.35, 0.7)] {
            cr.move_to(0.0, sy + 4.0);
            let mut x = 0.0;
            while x <= w + 5.0 {
                let n = noise::fbm1(x / w * (3.0 + i as f64 * 1.5) + i as f64 * 9.0, 4, 700 + i);
                cr.line_to(x, sy - sy * amp * n.powf(1.3));
                x += 5.0;
            }
            cr.line_to(w, sy + 4.0);
            cr.close_path();
            rgba(cr, mix(haze, mix((0.30, 0.38, 0.36), pal.dfg, 0.3), k), 1.0);
            cr.fill_q();
        }
        // Meseta.
        cr.save_q();
        cr.translate(0.0, sy);
        cr.scale(q, q);
        let _ = cr.set_source_surface(&img, 0.0, 0.0);
        cr.source().set_filter(cairo::Filter::Bilinear);
        cr.paint_q();
        cr.restore_q();
        // Grano de la pradera a resolución completa (hierba, matorral) y bosquecillos en las
        // manchas oscuras, más pequeños cuanto más lejos.
        {
            let gtile = noise::grey_tile(256, 40, 3, 516, 1.5);
            let gpat = cairo::SurfacePattern::create(&gtile);
            gpat.set_extend(cairo::Extend::Repeat);
            cr.save_q();
            cr.rectangle(0.0, sy, w, cy - sy);
            cr.clip();
            cr.set_operator(cairo::Operator::SoftLight);
            let _ = cr.set_source(&gpat);
            let _ = cr.paint_with_alpha(0.45);
            cr.set_operator(cairo::Operator::Over);
            cr.restore_q();
            let mut trees: Vec<(f64, f64, f64)> = vec![];
            for i in 0..900u64 {
                let x = noise::rnd(i as i32, 1, 611) * w;
                let u = noise::rnd(i as i32, 2, 611);
                let y = sy + (cy - sy) * (0.08 + 0.9 * u * u);
                let fnoise = noise::fbm(x / w * 9.0, y / h * 12.0, 3, 808, 0);
                let dmin = rivers.iter().map(|r| dist_poly(x, y, r)).fold(f64::MAX, f64::min);
                if fnoise > 0.57 && dmin > w * 0.045 && self.height(x, y, &rivers) < 0.9 {
                    let s = (1.6 + 5.5 * ((y - sy) / (cy - sy))) * (0.7 + 0.6 * noise::rnd(i as i32, 3, 611));
                    trees.push((x, y, s));
                }
            }
            trees.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            for (x, y, s) in trees {
                // Sombra, copa y luz: tres triángulos (abetos vistos de lejos).
                rgba(cr, darken(forest, 0.5), 0.6);
                cr.move_to(x + s * 0.3, y + 1.0);
                cr.line_to(x + s * 1.3, y + 1.0);
                cr.line_to(x + s * 0.5, y - s * 0.6);
                cr.close_path();
                cr.fill_q();
                rgba(cr, darken(forest, 0.25), 1.0);
                cr.move_to(x - s * 0.55, y);
                cr.line_to(x + s * 0.55, y);
                cr.line_to(x, y - s * 1.6);
                cr.close_path();
                cr.fill_q();
                rgba(cr, lighten(forest, 0.25), 0.9);
                cr.move_to(x - s * 0.5, y - s * 0.1);
                cr.line_to(x, y - s * 0.2);
                cr.line_to(x, y - s * 1.55);
                cr.close_path();
                cr.fill_q();
            }
        }
        let wtile = noise::grey_tile(256, 12, 3, 515, 1.6);
        for i in 0..self.lanes() {
            self.draw_bed(cr, i, pal, &wtile);
        }
        // Bruma del horizonte por encima de todo lo lejano (ríos y cauces incluidos).
        let _ = cr.set_source(lin(0.0, sy, 0.0, sy + (cy - sy) * 0.5, &[(0.0, haze, 0.5), (1.0, haze, 0.0)]));
        cr.rectangle(0.0, sy, w, (cy - sy) * 0.5);
        cr.fill_q();
        // Acantilado: pared de roca con estratos y sombra bajo el borde.
        let _ = cr.set_source(lin(0.0, cy, 0.0, bt + 24.0, &[(0.0, lighten(rock, 0.12), 1.0), (0.3, rock, 1.0), (1.0, darken(rock, 0.55), 1.0)]));
        cr.rectangle(0.0, cy, w, bt + 24.0 - cy);
        cr.fill_q();
        let tile = noise::grey_tile(256, 20, 3, 7, 1.2);
        let pat = cairo::SurfacePattern::create(&tile);
        pat.set_extend(cairo::Extend::Repeat);
        cr.save_q();
        cr.rectangle(0.0, cy, w, bt + 24.0 - cy);
        cr.clip();
        cr.set_operator(cairo::Operator::SoftLight);
        let _ = cr.set_source(&pat);
        let _ = cr.paint_with_alpha(0.8);
        cr.set_operator(cairo::Operator::Over);
        for i in 0..9u64 {
            let y0 = cy + (bt + 24.0 - cy) * (i as f64 + 0.5) / 9.0;
            rgba(cr, darken(rock, 0.65), 0.35);
            cr.set_line_width(1.0 + 1.5 * noise::rnd(i as i32, 2, 71));
            cr.move_to(0.0, y0);
            let mut x = 0.0;
            while x <= w {
                cr.line_to(x, y0 + (noise::fbm1(x * 0.02 + i as f64 * 4.0, 2, 71) - 0.5) * 7.0);
                x += 8.0;
            }
            cr.stroke_q();
        }
        cr.restore_q();
        let _ = cr.set_source(lin(0.0, cy, 0.0, cy + 10.0, &[(0.0, (0.0, 0.0, 0.0), 0.45), (1.0, (0.0, 0.0, 0.0), 0.0)]));
        cr.rectangle(0.0, cy, w, 10.0);
        cr.fill_q();
        rgba(cr, lighten(grass, 0.15), 0.9);
        cr.rectangle(0.0, cy - 2.0, w, 2.5);
        cr.fill_q();
        // Cuencas y presas.
        let dw = self.dam_w();
        for i in 0..self.lanes() {
            let (x0, x1) = self.lane_x(i);
            // Vaso del embalse: pared de roca con estratos, más oscura hacia el fondo.
            let _ = cr.set_source(lin(0.0, bt, 0.0, bb, &[(0.0, darken(rock, 0.2), 1.0), (1.0, darken(rock, 0.65), 1.0)]));
            cr.rectangle(x0, bt, x1 - x0, bb - bt);
            cr.fill_q();
            cr.save_q();
            cr.rectangle(x0, bt, x1 - x0, bb - bt);
            cr.clip();
            cr.set_operator(cairo::Operator::SoftLight);
            let _ = cr.set_source(&pat);
            let _ = cr.paint_with_alpha(0.7);
            cr.set_operator(cairo::Operator::Over);
            for k in 0..6u64 {
                let yy = bt + (bb - bt) * (k as f64 + 0.5) / 6.0;
                rgba(cr, darken(rock, 0.7), 0.3);
                cr.set_line_width(1.0 + noise::rnd(k as i32, 3, 72));
                cr.move_to(x0, yy);
                let mut x = x0;
                while x <= x1 {
                    cr.line_to(x, yy + (noise::fbm1(x * 0.02 + k as f64 * 3.0, 2, 72) - 0.5) * 6.0);
                    x += 8.0;
                }
                cr.stroke_q();
            }
            cr.restore_q();
            // Orilla izquierda de roca.
            rgba(cr, darken(rock, 0.3), 1.0);
            cr.move_to(x0, bt);
            let mut y = bt;
            while y <= bb {
                cr.line_to(x0 + 8.0 + (noise::fbm1(y * 0.03 + i as f64, 2, 90) - 0.5) * 14.0, y);
                y += 6.0;
            }
            cr.line_to(x0, bb);
            cr.close_path();
            cr.fill_q();
            // Presa: muro de hormigón con sombra y contrafuertes.
            let dam = mix((0.62, 0.60, 0.58), pal.dfg, 0.25);
            let _ = cr.set_source(lin(x1 - dw, 0.0, x1, 0.0, &[(0.0, darken(dam, 0.35), 1.0), (0.3, dam, 1.0), (1.0, darken(dam, 0.2), 1.0)]));
            cr.rectangle(x1 - dw, bt - 12.0, dw, bb - bt + 12.0);
            cr.fill_q();
            rgba(cr, lighten(dam, 0.25), 1.0);
            cr.rectangle(x1 - dw, bt - 12.0, dw, 3.0);
            cr.fill_q();
            rgba(cr, darken(dam, 0.55), 0.7);
            for k in 1..7 {
                let y = bt - 12.0 + (bb - bt + 12.0) * k as f64 / 7.0;
                cr.rectangle(x1 - dw, y, dw, 1.0);
            }
            cr.fill_q();
        }
        rgba(cr, darken(pal.dbg, 0.3), 1.0);
        cr.rectangle(0.0, bb, w, h - bb);
        cr.fill_q();
        *self.terrain.borrow_mut() = Some(surf);
        let ripple = noise::ripple_tile(256, 515, 0.55);
        *self.water_tile2.borrow_mut() = Some(ScaledTiles::new(noise::with_alpha(&ripple, 0.45)));
        *self.water_tile.borrow_mut() = Some(ScaledTiles::new(ripple));
        *self.caustic_tile.borrow_mut() = Some(ScaledTiles::new(noise::with_alpha(&noise::caustic_tile(256, 31), 0.16)));
        *self.streak_tile.borrow_mut() = Some(ScaledTiles::new(noise::streak_tile(256, 73)));
    }

    /// Geometría de los afluentes del río `i`: trazado (del manantial a la confluencia), semiancho
    /// del agua y del cauce en cada punto (más finos lejos, y el agua según el caudal), actividad,
    /// manantial y confluencia. Cada hueco tiene su sitio fijo: un afluente nunca cambia de lugar.
    fn trib_paths(&self, i: usize) -> Vec<TribPath> {
        let Some(r) = self.rivers.get(i) else { return vec![] };
        let (x0, x1) = self.lane_x(i);
        let dw = self.dam_w();
        let sc = (self.w / 1000.0).max(0.6);
        let pts = self.river_pts(i);
        let n = pts.len();
        let hw = |k: usize| self.half_at(i, k as f64 / (n - 1) as f64);
        let lanes = self.lanes();
        let per_lane = TRIB_SLOTS.div_ceil(lanes);
        (0..TRIB_SLOTS)
            .filter(|&slot| slot % lanes == i)
            .map(|slot| {
                let k = slot / lanes;
                let t = self.tribs[slot].as_ref();
                let side = if k % 2 == 0 { -1.0 } else { 1.0 };
                let u = 0.30 + 0.58 * (k as f64 + 0.5) / per_lane as f64;
                let j = ((n - 1) as f64 * u) as usize;
                let (jx, jy) = (pts[j].0 + side * hw(j) * r.level, pts[j].1);
                // Manantial: en el borde del carril, más arriba que la confluencia.
                let sx = if side < 0.0 { x0 + 18.0 + 30.0 * noise::rnd(k as i32, 4, 40 + i as u32) } else { x1 - dw - 18.0 - 30.0 * noise::rnd(k as i32, 4, 40 + i as u32) };
                let sy = (jy - (70.0 + 90.0 * noise::rnd(k as i32, 5, 40 + i as u32)) * sc).max(self.sky_y() + 24.0 * sc);
                let m = 28;
                let spts: Vec<(f64, f64)> = (0..=m)
                    .map(|q| {
                        let v = q as f64 / m as f64;
                        // Curva suave con un meandro, que se endereza al llegar al río.
                        let x = sx + (jx - sx) * (v * v * (3.0 - 2.0 * v)) + (noise::fbm1(v * 3.0 + k as f64 * 5.0, 2, 90 + i as u32) - 0.5) * 36.0 * sc * (1.0 - v) * v.sqrt();
                        let y = sy + (jy - sy) * v + (noise::fbm1(v * 2.0 + k as f64 * 7.0, 2, 95 + i as u32) - 0.5) * 20.0 * sc * (1.0 - v);
                        (x, y)
                    })
                    .collect();
                let act = t.map_or(0.0, |t| t.act);
                let (hw_w, hw_b): (Vec<f64>, Vec<f64>) = spts
                    .iter()
                    .map(|&(_, y)| {
                        let persp = 0.45 + 0.55 * self.depth(y);
                        let water = (0.8 + 4.5 * act) * sc * persp;
                        (water, (3.2 + 4.5 * act) * sc * persp)
                    })
                    .unzip();
                TribPath {
                    pts: spts,
                    hw_w,
                    hw_b,
                    act,
                    write: t.is_some_and(|t| t.write),
                    name: t.map_or(String::new(), |t| t.name.clone()),
                    bps: t.map_or(0.0, |t| t.bps),
                    side,
                    spring: (sx, sy),
                    conf: (jx, jy),
                    empty: t.is_none(),
                }
            })
            .collect()
    }

    /// Cinta alrededor de una polilínea con semiancho por punto (normales de cada tramo).
    fn ribbon_var(cr: &Context, pts: &[(f64, f64)], hw: &[f64]) {
        let n = pts.len();
        let normal = |k: usize| {
            let (a, b) = (pts[k.saturating_sub(1)], pts[(k + 1).min(n - 1)]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l = (dx * dx + dy * dy).sqrt().max(1e-6);
            (-dy / l, dx / l)
        };
        for k in 0..n {
            let (nx, ny) = normal(k);
            let (x, y) = (pts[k].0 + nx * hw[k], pts[k].1 + ny * hw[k]);
            if k == 0 {
                cr.move_to(x, y);
            } else {
                cr.line_to(x, y);
            }
        }
        for k in (0..n).rev() {
            let (nx, ny) = normal(k);
            cr.line_to(pts[k].0 - nx * hw[k], pts[k].1 - ny * hw[k]);
        }
        cr.close_path();
    }

    /// Polígono entre dos orillas (funciones del índice de punto) a lo largo del trazado `pts`.
    fn ribbon(cr: &Context, pts: &[(f64, f64)], a: &dyn Fn(usize) -> f64, b: &dyn Fn(usize) -> f64) {
        let n = pts.len();
        cr.move_to(a(0), pts[0].1);
        for k in 1..n {
            cr.line_to(a(k), pts[k].1);
        }
        for k in (0..n).rev() {
            cr.line_to(b(k), pts[k].1);
        }
        cr.close_path();
    }

    /// Capa de los afluentes y de la parte fija del agua del río (orilla mojada, color del agua,
    /// canal profundo, sombra de la orilla y brillo de los bordes): depende solo del nivel, así que
    /// se hornea con los afluentes y por fotograma solo quedan los rizos y la espuma.
    fn bake_tribs(&self, pal: &Palette, font: &str, fs: f64, sc: f64) -> ImageSurface {
        let (w, cy) = (self.w, self.cliff_y());
        let surf = ImageSurface::create(cairo::Format::ARgb32, w.ceil() as i32, (cy + 4.0).ceil() as i32).expect("afluentes");
        let cr = &Context::new(&surf).expect("cairo");
        cr.select_font_face(font, cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        cr.set_font_size(fs);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Round);
        let water = Self::water(pal);
        let foam = mix((0.93, 0.97, 1.0), pal.bfg, 0.2);
        let dw = self.dam_w();
        // Etiquetas: se pintan al final, por encima de la bruma.
        let mut labels: Vec<(String, f64, f64, f64, bool)> = vec![];
        for i in 0..self.lanes() {
            let (x0, x1) = self.lane_x(i);
            for tb in self.trib_paths(i) {
                if tb.empty {
                    continue;
                }
                let col = if tb.write { mix(water, foam, 0.25) } else { water };
                let gravel = mix((0.66, 0.60, 0.48), mix(pal.dfg, pal.bg, 0.4), 0.3);
                let n = tb.pts.len();
                // Cauce: tierra húmeda alrededor, grava, piedras en las orillas y sombra del talud.
                let outer: Vec<f64> = tb.hw_b.iter().map(|h| h + 2.5 * sc).collect();
                Self::ribbon_var(cr, &tb.pts, &outer);
                rgba(cr, (0.20, 0.16, 0.11), 0.6);
                cr.fill_q();
                Self::ribbon_var(cr, &tb.pts, &tb.hw_b);
                rgba(cr, mix(gravel, grass_dry(pal), 0.2 * (1.0 - tb.act)), 1.0);
                cr.fill_q();
                for q in (1..n).step_by(2) {
                    let sd = 300 + i as u32 * 7 + q as u32;
                    let (x, y) = tb.pts[q];
                    let side = if noise::rnd(q as i32, 1, sd) > 0.5 { 1.0 } else { -1.0 };
                    let rr = (0.7 + 1.3 * noise::rnd(q as i32, 2, sd)) * sc * (0.5 + 0.5 * self.depth(y));
                    rgba(cr, darken(gravel, 0.45), 0.7);
                    cr.arc(x + side * (tb.hw_b[q] - rr), y, rr, 0.0, 2.0 * PI);
                    cr.fill_q();
                }
                // Agua (si hay caudal): borde oscuro, cuerpo con el reflejo del cielo y brillo de orillas.
                if tb.act > 0.02 {
                    let edge: Vec<f64> = tb.hw_w.iter().map(|h| h + 1.0).collect();
                    Self::ribbon_var(cr, &tb.pts, &edge);
                    rgba(cr, darken(col, 0.5), 1.0);
                    cr.fill_q();
                    Self::ribbon_var(cr, &tb.pts, &tb.hw_w);
                    let _ = cr.set_source(lin(0.0, tb.spring.1, 0.0, tb.conf.1, &[(0.0, lighten(col, 0.35), 1.0), (1.0, col, 1.0)]));
                    cr.fill_q();
                    let inner: Vec<f64> = tb.hw_w.iter().map(|h| (h * 0.45).max(0.4)).collect();
                    Self::ribbon_var(cr, &tb.pts, &inner);
                    rgba(cr, darken(mix(col, (0.05, 0.25, 0.30), 0.5), 0.25), 0.35);
                    cr.fill_q();
                    // Manantial: poza clara de la que brota el agua.
                    rgba(cr, lighten(col, 0.3), 0.8);
                    let sr = (3.0 + 4.0 * tb.act) * sc;
                    self.blobs.paint(cr, 0, tb.spring.0, tb.spring.1, sr, sr * 0.6);
                    // Espuma en la confluencia.
                    rgba(cr, foam, 0.3 + 0.35 * tb.act);
                    let fr = tb.hw_w[n - 1] * 1.6 + 4.0 * sc;
                    self.blobs.paint(cr, 1, tb.conf.0, tb.conf.1, fr, fr * 0.7);
                }
                // Etiqueta junto al manantial (apagada si el cauce está seco).
                let txt = if tb.act > 0.02 && tb.bps > 0.0 {
                    format!("{} {} {}/s", if tb.write { "↑" } else { "↓" }, tb.name, fmt_bytes(tb.bps))
                } else {
                    format!("{} · 0 B/s", tb.name)
                };
                if let Ok(e) = cr.text_extents(&txt) {
                    let (sx, sy) = tb.spring;
                    let x = if tb.side < 0.0 { (sx - 6.0).max(x0 + 4.0) } else { (sx + 6.0 - e.width()).min(x1 - dw - e.width() - 4.0) };
                    labels.push((txt, x, sy - 8.0, e.width(), tb.act > 0.02));
                }
            }
            // Agua del río: lo que no se mueve.
            let Some(r) = self.rivers.get(i) else { continue };
            if r.level <= 0.01 {
                continue;
            }
            let pts = self.river_pts(i);
            let n = pts.len();
            let hw = |k: usize| self.half_at(i, k as f64 / (n - 1) as f64);
            let wleft = |k: usize| pts[k].0 - hw(k) * r.level;
            let wright = |k: usize| pts[k].0 + hw(k) * r.level;
            // Orilla mojada y borde oscuro.
            Self::ribbon(cr, &pts, &|k| wleft(k) - 2.0 * sc, &|k| wright(k) + 2.0 * sc);
            rgba(cr, (0.16, 0.13, 0.09), 0.55);
            cr.fill_q();
            Self::ribbon(cr, &pts, &wleft, &wright);
            rgba(cr, darken(water, 0.5), 1.0);
            cr.fill_q();
            // Agua: refleja el cielo a lo lejos y gana color al acercarse.
            Self::ribbon(cr, &pts, &|k| wleft(k) + 1.5, &|k| wright(k) - 1.5);
            let _ = cr.set_source(lin(0.0, self.sky_y(), 0.0, cy, &[(0.0, lighten(water, 0.4), 1.0), (0.5, lighten(water, 0.08), 1.0), (1.0, water, 1.0)]));
            cr.fill_keep();
            cr.save_q();
            cr.clip();
            // Canal profundo en el centro: dos bandas encajadas, más oscuras y verdosas.
            let deep = darken(mix(water, (0.05, 0.25, 0.30), 0.5), 0.3);
            for (f0, f1) in [(0.14, 0.86), (0.3, 0.7)] {
                Self::ribbon(cr, &pts, &|k| wleft(k) + (wright(k) - wleft(k)) * f0, &|k| wleft(k) + (wright(k) - wleft(k)) * f1);
                rgba(cr, deep, 0.3);
                cr.fill_q();
            }
            // Sombra que la orilla izquierda proyecta sobre el agua.
            Self::ribbon(cr, &pts, &wleft, &|k| wleft(k) + 5.0 * sc);
            rgba(cr, (0.0, 0.05, 0.1), 0.35);
            cr.fill_q();
            cr.restore_q();
            // Brillo de las dos orillas del agua (se apaga hacia el horizonte).
            for edge in [&wleft as &dyn Fn(usize) -> f64, &wright] {
                cr.move_to(edge(0), pts[0].1);
                for k in 1..n {
                    cr.line_to(edge(k), pts[k].1);
                }
                let _ = cr.set_source(lin(0.0, self.sky_y(), 0.0, cy, &[(0.0, foam, 0.0), (0.4, foam, 0.45), (1.0, foam, 0.45)]));
                cr.set_line_width(1.2);
                cr.stroke_q();
            }
        }
        // Bruma del horizonte sobre lo ya pintado (solo donde hay capa: `Atop`).
        let haze = Self::haze(pal);
        cr.set_operator(cairo::Operator::Atop);
        let _ = cr.set_source(lin(0.0, self.sky_y(), 0.0, self.sky_y() + (cy - self.sky_y()) * 0.5, &[(0.0, haze, 0.6), (1.0, haze, 0.0)]));
        cr.rectangle(0.0, self.sky_y(), w, (cy - self.sky_y()) * 0.5);
        cr.fill_q();
        cr.set_operator(cairo::Operator::Over);
        for (txt, x, y, tw, live) in labels {
            rgba(cr, pal.dbg, if live { 0.65 } else { 0.55 });
            cr.rectangle(x - 4.0, y - fs - 1.0, tw + 8.0, fs + 6.0);
            cr.fill_q();
            rgba(cr, if live { lighten(pal.bfg, 0.1) } else { mix(pal.dfg, pal.bfg, 0.45) }, 1.0);
            cr.move_to(x, y);
            let _ = cr.show_text(&txt);
        }
        surf
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
        let tile2_ref = self.water_tile2.borrow();
        let Some(tile2) = tile2_ref.as_ref() else { return };
        let caus_ref = self.caustic_tile.borrow();
        let Some(caustic) = caus_ref.as_ref() else { return };
        let streak_ref = self.streak_tile.borrow();
        let Some(streak) = streak_ref.as_ref() else { return };
        let mut laps = crate::draw::Laps::new("rivers");
        let water = Self::water(pal);
        let foam = mix((0.93, 0.97, 1.0), pal.bfg, 0.2);
        let sc = (w / 1000.0).max(0.6);
        let (cy, bt, bb, dw) = (self.cliff_y(), self.basin_top(), self.basin_bottom(), self.dam_w());
        cr.select_font_face(font, cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        let fs = (11.0 * sc).clamp(10.0, 14.0);
        cr.set_font_size(fs);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Round);
        if let Some(t) = self.terrain.borrow().as_ref() {
            let _ = cr.set_source_surface(t, 0.0, 0.0);
            cr.paint_q();
        }
        {
            // Niveles cuantizados, en orden (una suma dejaría pasar dos cambios que se compensan).
            let lv: i32 = self.rivers.iter().fold(0i32, |h, r| h.wrapping_mul(31).wrapping_add((r.level * 200.0) as i32));
            let lv = self.tribs.iter().fold(lv, |h, t| h.wrapping_mul(17).wrapping_add(t.as_ref().map_or(-1, |t| (t.act * 12.0).round() as i32)));
            let key = (self.stamp, lv, w as i32, h as i32, crate::sim::hash_str(&pal.raw));
            if self.trib_cache.borrow().as_ref().map(|c| c.0) != Some(key) {
                let t0 = std::time::Instant::now();
                *self.trib_cache.borrow_mut() = Some((key, self.bake_tribs(pal, font, fs, sc)));
                if laps.on() {
                    eprintln!("rivers: capa de afluentes horneada en {:.2} ms", t0.elapsed().as_secs_f64() * 1000.0);
                }
            }
            if let Some((_, layer)) = self.trib_cache.borrow().as_ref() {
                let _ = cr.set_source_surface(layer, 0.0, 0.0);
                cr.paint_q();
            }
        }
        laps.lap("fondo");
        let water_pat = |scale: f64, dx: f64, dy: f64| tile.pattern(scale, dx, dy);

        for (i, r) in self.rivers.iter().enumerate() {
            let (x0, x1) = self.lane_x(i);
            let pts = self.river_pts(i);
            let n = pts.len();
            let bed = self.bed_w(i);
            let ww = bed * r.level; // ancho del agua al borde del acantilado = parte usada
            let hw = |k: usize| self.half_at(i, k as f64 / (n - 1) as f64);
            let left = |k: usize| pts[k].0 - hw(k);
            let right = |k: usize| pts[k].0 + hw(k);
            // El agua va centrada en el cauce: orillas secas a ambos lados, como un río de poco caudal.
            let wleft = |k: usize| pts[k].0 - hw(k) * r.level;
            let wright = |k: usize| pts[k].0 + hw(k) * r.level;
            let poly = |cr: &Context, a: &dyn Fn(usize) -> f64, b: &dyn Fn(usize) -> f64| {
                cr.move_to(a(0), pts[0].1);
                for k in 1..n {
                    cr.line_to(a(k), pts[k].1);
                }
                for k in (0..n).rev() {
                    cr.line_to(b(k), pts[k].1);
                }
                cr.close_path();
            };
            let shift = -(bed - ww) / 2.0;
            // Agua: desde la orilla izquierda, tan ancha como el porcentaje usado.
            if r.level > 0.01 {
                // Lo fijo del agua (color, canal, orillas) viene horneado con los afluentes; aquí,
                // dentro del agua, solo lo que se mueve: rizos, estelas, rápidos y destellos.
                poly(cr, &|k| wleft(k) + 1.5, &|k| wright(k) - 1.5);
                // Rizos en movimiento (crestas claras y senos oscuros, alfa horneado): un relleno.
                let _ = cr.set_source(&water_pat(0.7 * sc, 0.0, self.t * (25.0 + 160.0 * r.flow)));
                cr.fill_keep();
                cr.save_q();
                cr.clip();
                // Estelas de corriente: su velocidad es la actividad del disco.
                for s_ in 0..3 {
                    let f = 0.2 + 0.6 * s_ as f64 / 2.0;
                    cr.move_to(wleft(0) + (wright(0) - wleft(0)) * f, pts[0].1);
                    for k in 1..n {
                        cr.line_to(wleft(k) + (wright(k) - wleft(k)) * f, pts[k].1);
                    }
                    rgba(cr, foam, 0.12 + 0.3 * r.flow);
                    cr.set_line_width((1.0 + 0.4 * s_ as f64) * sc.max(0.8));
                    cr.set_dash(&[7.0 + 5.0 * s_ as f64, 24.0], -self.t * (40.0 + 240.0 * r.flow) + s_ as f64 * 9.0);
                    cr.stroke_q();
                }
                cr.set_dash(&[], 0.0);
                // Rápidos: espuma donde el agua se acelera hacia el borde del acantilado.
                let (xa, xb) = (wleft(n - 1), wright(n - 1));
                for q in 0..4 {
                    let fx = 0.12 + 0.76 * (q as f64 + 0.5) / 4.0;
                    let x = xa + (xb - xa) * fx;
                    let rr = ((xb - xa) * 0.12).clamp(3.0, 12.0) * (0.8 + 0.3 * (self.t * 3.0 + q as f64 * 1.9).sin());
                    rgba(cr, foam, 0.25 + 0.45 * r.flow);
                    self.blobs.paint(cr, q % 2, x, pts[n - 1].1 - rr * 0.6 - 10.0 * sc * noise::rnd(q as i32, 1, 77 + i as u32), rr, rr * 0.5);
                }
                // Lejos, los rizos se funden con la bruma.
                let haze = Self::haze(pal);
                let _ = cr.set_source(lin(0.0, self.sky_y(), 0.0, self.sky_y() + (cy - self.sky_y()) * 0.5, &[(0.0, haze, 0.6), (1.0, haze, 0.0)]));
                cr.rectangle(0.0, self.sky_y(), w, (cy - self.sky_y()) * 0.5);
                cr.fill_q();
                cr.restore_q();
                // Destellos del sol sobre el agua, que bajan con la corriente.
                let sd = 500 + i as u32;
                for q in 0..16 {
                    let drift = (noise::rnd(q, 1, sd) + self.t * 0.03 * (1.0 + 4.0 * r.flow)).rem_euclid(1.0);
                    let kk = ((n - 1) as f64 * drift) as usize;
                    let x = wleft(kk) + (wright(kk) - wleft(kk)) * (0.15 + 0.7 * noise::rnd(q, 2, sd));
                    let a = (self.t * 2.5 + q as f64 * 1.7).sin().max(0.0).powi(3) * (0.4 + 0.5 * drift);
                    rgba(cr, foam, a);
                    cr.rectangle(x.round(), pts[kk].1.round(), 2.0 + 2.0 * drift, 1.0);
                    cr.fill_q();
                }
            }
            laps.lap("agua");
            // Regla a lo ancho del cauce, al borde del acantilado: 0 % en la orilla izquierda, 100 % en la derecha.
            let ey = pts[n - 1].1;
            let (gx0, gy) = (left(n - 1), ey - 18.0 * sc);
            let gw = right(n - 1) - left(n - 1);
            rgba(cr, pal.dbg, 0.55);
            cr.rectangle(gx0 - 4.0, gy - 6.0, gw + 8.0, 12.0);
            cr.fill_q();
            rgba(cr, pal.bfg, 0.9);
            cr.set_line_width(1.0);
            cr.move_to(gx0, gy + 0.5);
            cr.line_to(gx0 + gw, gy + 0.5);
            cr.stroke_q();
            cr.set_font_size(fs * 0.75);
            for k in 0..=4 {
                let x = (gx0 + gw * k as f64 / 4.0).round() + 0.5;
                cr.move_to(x, gy - 4.0);
                cr.line_to(x, gy + 4.0);
                cr.stroke_q();
                if k % 2 == 0 {
                    let t = format!("{}%", k * 25);
                    if let Ok(e) = cr.text_extents(&t) {
                        cr.move_to(x - e.width() / 2.0, gy - 7.0);
                        let _ = cr.show_text(&t);
                    }
                }
            }
            cr.set_font_size(fs);
            // Marcadores en las dos orillas del agua: la franja entre ellos es la parte usada.
            rgba(cr, foam, 1.0);
            for xm in [gx0 + gw * (0.5 - r.level / 2.0), gx0 + gw * (0.5 + r.level / 2.0)] {
                cr.move_to(xm, gy - 5.0);
                cr.line_to(xm - 4.0, gy - 11.0);
                cr.line_to(xm + 4.0, gy - 11.0);
                cr.close_path();
                cr.fill_q();
            }
            rgba(cr, foam, 0.9);
            cr.rectangle(gx0 + gw * (0.5 - r.level / 2.0), gy - 2.0, gw * r.level, 4.0);
            cr.fill_q();
            // Etiquetas: usado sobre el agua, libre sobre la grava.
            let mk = n * 3 / 4;
            let mid = pts[mk];
            for (k, (txt, x, col)) in [
                (format!("{} {}", crate::i18n::tr("disk_used"), fmt_bytes(r.mounts.iter().map(|m| m.1).sum())), pts[mk].0, lighten(pal.bfg, 0.1)),
                (format!("{} {}", crate::i18n::tr("disk_free"), fmt_bytes(r.mounts.iter().map(|m| m.2 - m.1).sum::<f64>().max(0.0))), (wright(mk) + right(mk)) / 2.0, pal.fg),
            ]
            .into_iter()
            .enumerate()
            {
                if let Ok(e) = cr.text_extents(&txt) {
                    let yy = mid.1 - 30.0 * sc + k as f64 * (fs + 8.0);
                    let xx = (x - e.width() / 2.0).clamp(x0 + 4.0, x1 - dw - e.width() - 4.0);
                    rgba(cr, pal.dbg, 0.65);
                    cr.rectangle(xx - 4.0, yy - fs - 1.0, e.width() + 8.0, fs + 6.0);
                    cr.fill_q();
                    rgba(cr, col, 1.0);
                    cr.move_to(xx, yy);
                    let _ = cr.show_text(&txt);
                }
            }

            laps.lap("regla");
            // Afluentes: lo fijo (lecho, agua, espuma, etiqueta) viene horneado en una capa; aquí
            // solo se dibuja la corriente que corre.
            for tb in self.trib_paths(i).iter().filter(|tb| tb.act > 0.02) {
                // Rizos que corren por el afluente (un relleno) y una estela de espuma.
                Self::ribbon_var(cr, &tb.pts, &tb.hw_w);
                let _ = cr.set_source(&tile2.pattern(0.4 * sc, tb.spring.0 * 0.5, self.t * (20.0 + 120.0 * tb.act)));
                cr.fill_q();
                cr.move_to(tb.pts[0].0, tb.pts[0].1);
                for p in &tb.pts[1..] {
                    cr.line_to(p.0, p.1);
                }
                rgba(cr, foam, 0.2 + 0.35 * tb.act);
                cr.set_line_width((tb.hw_w[tb.pts.len() / 2] * 0.5).max(0.7));
                cr.set_dash(&[5.0, 13.0], -self.t * (30.0 + 160.0 * tb.act));
                cr.stroke_q();
                cr.set_dash(&[], 0.0);
            }
            laps.lap("afluentes");
            // Embalse: marca de humedad en la pared justo sobre el nivel, y el agua.
            let lvl = bt + (bb - bt) * (1.0 - r.level);
            let fx = pts[n - 1].0;
            let _ = shift;
            let xr = x1 - dw;
            let _ = cr.set_source(lin(0.0, lvl - 16.0 * sc, 0.0, lvl, &[(0.0, (0.0, 0.0, 0.0), 0.0), (1.0, (0.0, 0.0, 0.0), 0.45)]));
            cr.rectangle(x0, (lvl - 16.0 * sc).max(bt), xr - x0, (16.0 * sc).min(lvl - bt).max(0.0));
            cr.fill_q();
            let _ = cr.set_source(lin(0.0, lvl, 0.0, bb, &[(0.0, lighten(water, 0.22), 1.0), (0.35, water, 1.0), (1.0, darken(mix(water, (0.02, 0.12, 0.2), 0.5), 0.5), 1.0)]));
            cr.rectangle(x0, lvl, xr - x0, bb - lvl);
            cr.fill_q();
            cr.save_q();
            cr.rectangle(x0, lvl, xr - x0, bb - lvl);
            cr.clip();
            // Reflejos: la pared oscura justo bajo la orilla, la presa a la derecha y la cascada
            // como una columna clara que se disuelve hacia el fondo.
            let _ = cr.set_source(lin(0.0, lvl, 0.0, lvl + (bb - lvl) * 0.45, &[(0.0, darken(water, 0.4), 0.55), (1.0, darken(water, 0.3), 0.0)]));
            cr.paint_q();
            let _ = cr.set_source(lin(xr - dw * 2.2, 0.0, xr, 0.0, &[(0.0, (0.0, 0.0, 0.0), 0.0), (1.0, (0.0, 0.0, 0.0), 0.3)]));
            cr.rectangle(xr - dw * 2.2, lvl, dw * 2.2, bb - lvl);
            cr.fill_q();
            let fw = (ww * 0.9).max(2.0);
            let _ = cr.set_source(lin(0.0, lvl, 0.0, lvl + (bb - lvl) * 0.7, &[(0.0, foam, 0.28 + 0.2 * r.flow), (1.0, foam, 0.0)]));
            cr.rectangle(fx - fw * 0.6, lvl, fw * 1.2, (bb - lvl) * 0.7);
            cr.fill_q();
            // Red de luz bajo la superficie y rizos lentos, con el alfa ya horneado.
            let _ = cr.set_source(&caustic.pattern(1.6 * sc, self.t * 6.0, self.t * 2.5));
            cr.paint_q();
            let _ = cr.set_source(&tile2.pattern(1.8 * sc, self.t * 12.0, self.t * 4.0));
            cr.paint_q();
            for k in 0..5 {
                let yy = lvl + (bb - lvl) * (0.1 + 0.18 * k as f64);
                rgba(cr, lighten(water, 0.55), 0.08 + 0.1 * (self.t * 1.5 + k as f64).sin().abs());
                cr.set_line_width(1.0);
                let mut x = x0;
                cr.move_to(x, yy);
                while x <= xr {
                    cr.line_to(x, yy + (x * 0.05 + self.t * 2.0 + k as f64).sin() * 1.2);
                    x += 8.0;
                }
                cr.stroke_q();
            }
            rgba(cr, foam, 0.45 * r.flow + 0.1);
            let fr = (40.0 + 60.0 * r.flow) * sc;
            self.blobs.paint(cr, 0, fx, lvl, fr, fr * 0.55);
            cr.restore_q();
            rgba(cr, lighten(water, 0.6), 0.8);
            cr.set_line_width(1.5);
            let mut x = x0;
            cr.move_to(x, lvl);
            while x <= xr {
                cr.line_to(x, lvl + (x * 0.08 + self.t * 2.0).sin() * 1.5);
                x += 6.0;
            }
            cr.stroke_q();

            laps.lap("embalse");
            // Cascada: chorros verticales con la textura cayendo, ensanchándose, y bruma al pie.
            cr.save_q();
            cr.move_to(fx - fw / 2.0, cy - 2.0);
            cr.line_to(fx + fw / 2.0, cy - 2.0);
            cr.line_to(fx + fw * 0.8, lvl + 4.0);
            cr.line_to(fx - fw * 0.8, lvl + 4.0);
            cr.close_path();
            // Cuerpo de agua: azul translúcido con los bordes deshilachados; encima, los chorros de
            // espuma que caen (baldosa de hilos verticales, alfa horneado) a la velocidad de escritura.
            let _ = cr.set_source(lin(fx - fw * 0.8, 0.0, fx + fw * 0.8, 0.0, &[(0.0, water, 0.0), (0.2, lighten(water, 0.3), 0.75), (0.5, lighten(water, 0.5), 0.9), (0.8, lighten(water, 0.3), 0.75), (1.0, water, 0.0)]));
            cr.fill_keep();
            cr.clip();
            let _ = cr.set_source(&streak.pattern(0.55 * sc, fx * 0.3, self.t * (180.0 + 420.0 * r.fall)));
            cr.paint_q();
            // Más blanca y abierta hacia abajo, donde el agua se deshace en espuma.
            let _ = cr.set_source(lin(0.0, cy, 0.0, lvl, &[(0.0, foam, 0.0), (0.6, foam, 0.15), (1.0, foam, 0.55)]));
            cr.paint_q();
            // Labio: el agua se dobla sobre el borde, con un brillo arriba y sombra justo debajo.
            let _ = cr.set_source(lin(0.0, cy - 2.0, 0.0, cy + 14.0 * sc, &[(0.0, foam, 0.85), (0.25, lighten(water, 0.5), 0.5), (1.0, darken(water, 0.5), 0.0)]));
            cr.paint_q();
            cr.restore_q();
            // Bruma al pie de la cascada: dos manchas blandas que respiran.
            let fr = (34.0 + 70.0 * r.flow) * sc;
            for (k, ph) in [(0usize, 0.0), (1usize, 2.1)] {
                let br = fr * (0.9 + 0.12 * (self.t * 1.3 + ph).sin());
                rgba(cr, foam, 0.42 * (0.3 + 0.7 * r.flow));
                self.blobs.paint(cr, k, fx + (k as f64 - 0.5) * fr * 0.3, lvl - fr * 0.15, br, br * 0.7);
            }
            laps.lap("cascada");

            // Escala de la presa (0–100 % de ocupación) y marcador.
            let xs = xr;
            rgba(cr, pal.bfg, 0.85);
            cr.set_line_width(1.0);
            cr.set_font_size(fs * 0.8);
            for k in 0..=10 {
                let y = (bt + (bb - bt) * (1.0 - k as f64 / 10.0)).round() + 0.5;
                let major = k % 5 == 0;
                cr.move_to(xs + 3.0, y);
                cr.line_to(xs + if major { dw * 0.55 } else { dw * 0.3 }, y);
                cr.stroke_q();
                if major {
                    let t = format!("{}", k * 10);
                    cr.move_to(xs + dw * 0.6, y + fs * 0.3);
                    let _ = cr.show_text(&t);
                }
            }
            cr.set_font_size(fs);
            rgba(cr, foam, 0.95);
            cr.move_to(xs - 1.0, lvl);
            cr.line_to(xs - 9.0, lvl - 5.0);
            cr.line_to(xs - 9.0, lvl + 5.0);
            cr.close_path();
            cr.fill_q();
            let pct = format!("{:.0}%", r.level * 100.0);
            cr.set_font_size(fs + 3.0);
            if let Ok(e) = cr.text_extents(&pct) {
                let x = (xs - e.width() - 16.0).max(x0 + 4.0);
                rgba(cr, pal.dbg, 0.7);
                cr.rectangle(x - 5.0, lvl - fs - 6.0, e.width() + 10.0, fs + 12.0);
                cr.fill_q();
                rgba(cr, foam, 1.0);
                cr.move_to(x, lvl + fs * 0.4);
                let _ = cr.show_text(&pct);
            }
            cr.set_font_size(fs);
            // Ficha del disco.
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
        }
        laps.lap("textos");
        // Salpicaduras: en tres tandas de opacidad, un solo relleno por tanda.
        for (lo, hi, a) in [(0.0, 0.35, 0.3), (0.35, 0.7, 0.6), (0.7, 9.0, 0.85)] {
            rgba(cr, foam, a);
            for d in self.drops.iter().filter(|d| d.life >= lo && d.life < hi) {
                cr.new_sub_path();
                cr.arc(d.x, d.y, 1.4 * sc, 0.0, 2.0 * PI);
            }
            cr.fill_q();
        }
        laps.lap("gotas");
        laps.done(60);
        let _ = h;
    }
}

pub fn widget(v: Rc<RefCell<Rivers>>, pal: Rc<RefCell<Palette>>, font: String) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    let (v2, p2) = (v.clone(), pal.clone());
    let prof = std::env::var_os("TA_PROF").is_some();
    let acc: Rc<RefCell<(f64, u32)>> = Rc::new(RefCell::new((0.0, 0)));
    area.set_draw_func(move |_, cr, w, h| {
        let t0 = std::time::Instant::now();
        let mut r = v2.borrow_mut();
        r.resize(w as f64, h as f64);
        r.draw(cr, &p2.borrow(), &font);
        if prof {
            let mut a = acc.borrow_mut();
            a.0 += t0.elapsed().as_secs_f64() * 1000.0;
            a.1 += 1;
            if a.1 == 60 {
                eprintln!("rivers: {:.1} ms/draw", a.0 / 60.0);
                *a = (0.0, 0);
            }
        }
    });
    let last: Rc<RefCell<Option<i64>>> = Rc::new(RefCell::new(None));
    area.add_tick_callback(move |a, clock| {
        let now = clock.frame_time();
        let mut l = last.borrow_mut();
        if !a.is_mapped() {
            // Pestaña oculta: ni simulación ni repintado.
            *l = None;
            return glib::ControlFlow::Continue;
        }
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
