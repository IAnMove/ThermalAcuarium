//! Pestaña Memoria, skin Arcade: un volcán en sección, como en un libro de texto. La mitad
//! izquierda es el exterior (roca, nieve, coladas); la derecha está cortada y deja ver la cámara de
//! magma: su nivel es exactamente la RAM en uso, con una escala de 0 a 100 % al lado. La caché es la
//! capa de magma frío sobre el fundido, la memoria libre es la nieve de la cumbre, los programas que
//! más RAM usan son las coladas, el swap agrieta la llanura, y cuando el magma llega al borde del
//! cráter rebosa por la ladera y el volcán entra en erupción.

use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::cairo::{self, Context, ImageSurface, LinearGradient};
use gtk::glib;
use gtk::prelude::*;

use crate::draw::{Quiet, lin, rgba};
use crate::i18n::{tr, trf};
use crate::reef::noise;
use crate::sim::Rng;
use crate::stamps::{ScaledTiles, Stamps};
use crate::sys::{Group, fmt_bytes};
use crate::theme::{Palette, Rgb, darken, lighten, mix};

struct Bomb {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    r: f64,
    life: f64,
}

struct Puff {
    kind: usize,
    rot: f64,
    x: f64,
    y: f64,
    r: f64,
    vx: f64,
    vy: f64,
    age: f64,
    max: f64,
}

struct Ember {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    life: f64,
}

/// Relámpago volcánico: la ceniza de la erupción se carga y descarga dentro de la columna.
struct Bolt {
    pts: Vec<(f64, f64)>,
    life: f64,
}

/// Colada: un programa grande bajando por la ladera.
#[derive(Clone)]
pub struct Flow {
    pub name: String,
    pub mem: f64,
    pub share: f64,
}

pub struct Volcano {
    pub w: f64,
    pub h: f64,
    t: f64,
    usage: f64,
    goal: f64,
    cache: f64,
    cache_goal: f64,
    swap: f64,
    swap_goal: f64,
    pub total: f64,
    pub used: f64,
    pub cached: f64,
    pub swap_used: f64,
    pub swap_total: f64,
    pub flows: Vec<Flow>,
    /// Programas con más memoria en swap: una fisura cada uno.
    pub swappers: Vec<(String, f64)>,
    bombs: Vec<Bomb>,
    puffs: Vec<Puff>,
    embers: Vec<Ember>,
    bolts: Vec<Bolt>,
    bolt_acc: f64,
    bomb_acc: f64,
    puff_acc: f64,
    ember_acc: f64,
    rng: Rng,
    shake: (f64, f64),
    /// Capas estáticas cacheadas por (tamaño, paleta): montaña exterior y corte interior.
    cache_key: RefCell<Option<(i32, i32, u64)>>,
    mountain: RefCell<Option<ImageSurface>>,
    section: RefCell<Option<ImageSurface>>,
    lava_tile: RefCell<Option<ScaledTiles>>,
    /// Fundido de la cámara y del lago.
    molten_tile: RefCell<Option<ScaledTiles>>,
    /// Costra con alfa: pieles frías sobre el lago del cráter.
    crust_soft: RefCell<Option<ScaledTiles>>,
    blobs: Stamps,
    /// Cielo con estrellas, horneado por nivel de uso (cambia despacio).
    sky_key: RefCell<Option<i32>>,
    sky: RefCell<Option<ImageSurface>>,
    /// Sube con cada `set_groups`: invalida las capas horneadas de coladas y fisuras.
    stamp: u64,
    /// Coladas y fisuras: todo lo que no es la textura de lava (halo, borde, brillo, etiquetas) se
    /// hornea una vez; cada fotograma solo se pinta la lava en movimiento a través de una máscara.
    flow_cache: RefCell<Option<(FlowKey, Baked)>>,
    swap_cache: RefCell<Option<(FlowKey, Baked)>>,
}

type FlowKey = (u64, i32, i32, i32, u64);

/// Capas horneadas de un grupo de trazos de lava, recortadas a su caja (`x0`, `y0`). La lava en
/// movimiento se traza cada fotograma por encima de `under` (son trazos finos: baratos).
struct Baked {
    under: ImageSurface,
    /// Trazos de lava: puntos y anchura (y anchura del hilo brillante del centro, 0 si no hay).
    strokes: Vec<(Vec<(f64, f64)>, f64, f64)>,
    /// Etiquetas: texto, x, y (línea base).
    labels: Vec<(String, f64, f64)>,
    x0: f64,
    y0: f64,
}

impl Baked {
    fn new(x0: f64, y0: f64, w: f64, h: f64) -> Baked {
        let under = ImageSurface::create(cairo::Format::ARgb32, w.ceil().max(1.0) as i32, h.ceil().max(1.0) as i32).expect("capa");
        Baked { under, strokes: vec![], labels: vec![], x0, y0 }
    }

    /// Contexto de la capa, ya trasladado a coordenadas de la escena.
    fn cx(&self) -> Context {
        let c = Context::new(&self.under).expect("cairo");
        c.translate(-self.x0, -self.y0);
        c.set_line_cap(cairo::LineCap::Round);
        c.set_line_join(cairo::LineJoin::Round);
        c
    }

    /// Pinta: capa horneada, trazos de lava con su hilo brillante, etiquetas.
    fn paint(&self, cr: &Context, lava: &cairo::SurfacePattern, hot: Rgb, pal: &Palette, fs: f64, t: f64) {
        let path = |cr: &Context, pts: &[(f64, f64)]| {
            cr.move_to(pts[0].0, pts[0].1);
            for p in &pts[1..] {
                cr.line_to(p.0, p.1);
            }
        };
        for (pts, wd, _) in &self.strokes {
            path(cr, pts);
            let _ = cr.set_source(lava);
            cr.set_line_width(*wd);
            cr.stroke_q();
        }
        for (pts, wd, hw) in self.strokes.iter().filter(|s| s.2 > 0.0) {
            path(cr, pts);
            rgba(cr, hot, 0.5);
            cr.set_line_width(*hw);
            cr.stroke_q();
            // Lenguas que bajan por la colada: un trazo a rayas cuyo desfase corre con el tiempo.
            path(cr, pts);
            rgba(cr, hot, 0.4);
            cr.set_line_width(hw * 1.6);
            cr.set_dash(&[wd * 1.6, wd * 2.6], -t * 26.0);
            cr.stroke_q();
            cr.set_dash(&[], 0.0);
        }
        for (txt, x, y) in &self.labels {
            if let Ok(e) = cr.text_extents(txt) {
                rgba(cr, pal.dbg, 0.65);
                cr.rectangle(x - 4.0, y - fs - 1.0, e.width() + 8.0, fs + 6.0);
                cr.fill_q();
                rgba(cr, lighten(pal.bfg, 0.1), 1.0);
                cr.move_to(*x, *y);
                let _ = cr.show_text(txt);
            }
        }
    }
}

const MAX_BOMBS: usize = 260;

/// Rampa de color de la lava: costra fría → rojo profundo → naranja → amarillo blanco.
fn lava_ramp(pal: &Palette, v: f64) -> Rgb {
    let crust = mix((0.13, 0.05, 0.03), pal.red, 0.12);
    let deep = mix((0.55, 0.08, 0.02), pal.red, 0.25);
    let hot = mix((1.0, 0.42, 0.06), pal.orange, 0.25);
    let white = mix((1.0, 0.95, 0.65), pal.yellow, 0.2);
    let v = v.clamp(0.0, 1.0);
    if v < 0.3 {
        mix(crust, deep, v / 0.3)
    } else if v < 0.68 {
        mix(deep, hot, (v - 0.3) / 0.38)
    } else {
        mix(hot, white, (v - 0.68) / 0.32)
    }
}

/// Baldosa de costra: placas oscuras de roca con las grietas por donde asoma el fundido, cada
/// grieta con un núcleo blanco y un resplandor rojizo alrededor (horneado: cero coste por fotograma).
fn lava_tile(pal: &Palette) -> ImageSurface {
    let n = 256;
    let mut surf = ImageSurface::create(cairo::Format::ARgb32, n, n).expect("lava");
    let stride = surf.stride() as usize;
    {
        let mut d = surf.data().expect("lava data");
        for y in 0..n as usize {
            for x in 0..n as usize {
                let (fx, fy) = (x as f64 / n as f64, y as f64 / n as f64);
                // Deformación del dominio: las grietas serpentean en vez de seguir la rejilla.
                let wx = noise::fbm(fx * 4.0, fy * 4.0, 2, 501, 4) - 0.5;
                let wy = noise::fbm(fx * 4.0 + 5.0, fy * 4.0 + 5.0, 2, 502, 4) - 0.5;
                let a = noise::fbm(fx * 6.0 + 0.7 * wx, fy * 6.0 + 0.7 * wy, 4, 909, 6);
                let b = noise::fbm(fx * 12.0 + 3.0 + 0.4 * wy, fy * 12.0 + 0.4 * wx, 3, 313, 12);
                let core = (1.0 - ((a - 0.5).abs() * 13.0).min(1.0)).powf(1.3);
                let bloom = (1.0 - ((a - 0.5).abs() * 4.2).min(1.0)).powi(2) * 0.42;
                let fine = (1.0 - ((b - 0.5).abs() * 15.0).min(1.0)).powf(1.6) * 0.5;
                let rock = noise::fbm(fx * 16.0, fy * 16.0, 3, 515, 16);
                let v = (core + bloom + fine).clamp(0.0, 1.0);
                // La costra no es plana: tiene su propio grano de roca.
                let c = if v < 0.08 { mix(lava_ramp(pal, v), (0.26, 0.2, 0.17), 0.35 * rock) } else { lava_ramp(pal, v) };
                let i = y * stride + x * 4;
                d[i] = (c.2 * 255.0) as u8;
                d[i + 1] = (c.1 * 255.0) as u8;
                d[i + 2] = (c.0 * 255.0) as u8;
                d[i + 3] = 255;
            }
        }
    }
    surf
}

/// Baldosa de magma fundido: casi todo incandescente, con remolinos y finas pieles oscuras que
/// flotan (para la cámara y el lago del cráter).
fn molten_tile(pal: &Palette) -> ImageSurface {
    let n = 256;
    let mut surf = ImageSurface::create(cairo::Format::ARgb32, n, n).expect("molten");
    let stride = surf.stride() as usize;
    {
        let mut d = surf.data().expect("molten data");
        for y in 0..n as usize {
            for x in 0..n as usize {
                let (fx, fy) = (x as f64 / n as f64, y as f64 / n as f64);
                let wx = noise::fbm(fx * 3.0, fy * 3.0, 2, 611, 3) - 0.5;
                let wy = noise::fbm(fx * 3.0 + 4.0, fy * 3.0 + 4.0, 2, 612, 3) - 0.5;
                let swirl = noise::fbm(fx * 5.0 + 0.9 * wx, fy * 5.0 + 0.9 * wy, 4, 707, 5);
                let skin = noise::fbm(fx * 9.0 + 0.5 * wy, fy * 9.0 + 0.5 * wx, 3, 777, 9);
                let veins = (1.0 - ((skin - 0.5).abs() * 9.0).min(1.0)).powf(1.5);
                let sw = ((swirl - 0.5) * 1.9 + 0.5).clamp(0.0, 1.0);
                let mut v = 0.22 + 0.5 * sw + 0.38 * veins;
                // Pieles: islas finas de costra que se enfrían sobre el fundido.
                let cool = ((skin - 0.60) * 6.0).clamp(0.0, 1.0);
                v *= 1.0 - 0.85 * cool;
                let c = lava_ramp(pal, v);
                let i = y * stride + x * 4;
                d[i] = (c.2 * 255.0) as u8;
                d[i + 1] = (c.1 * 255.0) as u8;
                d[i + 2] = (c.0 * 255.0) as u8;
                d[i + 3] = 255;
            }
        }
    }
    surf
}

impl Volcano {
    pub fn new() -> Volcano {
        Volcano {
            w: 1000.0,
            h: 600.0,
            t: 0.0,
            usage: 0.3,
            goal: 0.3,
            cache: 0.1,
            cache_goal: 0.1,
            swap: 0.0,
            swap_goal: 0.0,
            total: 1.0,
            used: 0.0,
            cached: 0.0,
            swap_used: 0.0,
            swap_total: 0.0,
            flows: vec![],
            swappers: vec![],
            bombs: vec![],
            puffs: vec![],
            embers: vec![],
            bolts: vec![],
            bolt_acc: 0.0,
            bomb_acc: 0.0,
            puff_acc: 0.0,
            ember_acc: 0.0,
            rng: Rng::new(0xB00B5),
            shake: (0.0, 0.0),
            cache_key: RefCell::new(None),
            mountain: RefCell::new(None),
            section: RefCell::new(None),
            lava_tile: RefCell::new(None),
            molten_tile: RefCell::new(None),
            crust_soft: RefCell::new(None),
            blobs: Stamps::new(3, 1200),
            sky_key: RefCell::new(None),
            sky: RefCell::new(None),
            stamp: 0,
            flow_cache: RefCell::new(None),
            swap_cache: RefCell::new(None),
        }
    }

    pub fn set_memory(&mut self, used: f64, total: f64, cached: f64, swap_used: f64, swap_total: f64) {
        self.total = total.max(1.0);
        self.used = used;
        self.cached = cached;
        self.swap_used = swap_used;
        self.swap_total = swap_total;
        self.goal = (used / self.total).clamp(0.0, 1.0);
        self.cache_goal = (cached / self.total).clamp(0.0, 1.0 - self.goal);
        self.swap_goal = if swap_total > 0.0 { (swap_used / swap_total).clamp(0.0, 1.0) } else { 0.0 };
    }

    pub fn set_groups(&mut self, groups: &[Group]) {
        let mut v: Vec<Flow> = groups
            .iter()
            .map(|g| {
                let mem: f64 = g.procs.iter().map(|p| p.mem).sum();
                Flow { name: g.key.clone(), mem, share: mem / self.total }
            })
            .collect();
        v.sort_by(|a, b| b.mem.partial_cmp(&a.mem).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(5);
        self.flows = v;
        let mut sw: Vec<(String, f64)> = groups.iter().map(|g| (g.key.clone(), g.procs.iter().map(|p| p.swap).sum::<f64>())).filter(|x| x.1 > 1e6).collect();
        sw.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sw.truncate(6);
        self.swappers = sw;
        self.stamp += 1;
    }

    // --- geometría

    fn peak(&self) -> (f64, f64) {
        (self.w * 0.5, self.h * 0.20)
    }
    fn horizon(&self) -> f64 {
        self.h * 0.82
    }
    fn crater(&self) -> f64 {
        self.w * 0.055 + 12.0
    }
    /// Semiancho de la cámara a la altura `y` (conducto estrecho arriba, cámara abombada abajo).
    fn chamber_half(&self, y: f64) -> f64 {
        let (_, py) = self.peak();
        let hz = self.horizon();
        let u = ((y - py) / (hz - py)).clamp(0.0, 1.0);
        let c = self.crater() * 0.55;
        c + (self.w * 0.19 - c) * (u.powf(1.6) * (1.0 - 0.25 * (u * PI).sin()))
    }

    /// Altura de la ladera en `x`.
    pub fn slope_y(&self, x: f64) -> f64 {
        let (px, py) = self.peak();
        let c = self.crater();
        let hz = self.horizon();
        let d = (x - px).abs();
        if d <= c {
            return py + (d / c).powi(2) * self.h * 0.012;
        }
        let half = self.w * 0.46;
        let u = ((d - c) / half).clamp(0.0, 1.3);
        let base = py + (hz - py) * (1.0 - (1.0 - u.min(1.0)).powf(1.75));
        let rough = (noise::fbm1(x * 0.02, 3, 77) - 0.5) * self.h * 0.03 * u.min(1.0);
        (base + rough).min(hz + self.h * 0.01)
    }

    /// Altura del magma para una fracción de uso (0 = base de la cámara, 1 = borde del cráter).
    fn level_y(&self, frac: f64) -> f64 {
        let (_, py) = self.peak();
        let hz = self.horizon();
        hz - (hz - py) * frac
    }

    pub fn resize(&mut self, w: f64, h: f64) {
        if (w - self.w).abs() > 0.5 || (h - self.h).abs() > 0.5 {
            self.w = w;
            self.h = h;
            self.bombs.clear();
            self.puffs.clear();
            self.embers.clear();
            self.bolts.clear();
        }
    }

    pub fn step(&mut self, dt: f64) {
        let dt = dt.clamp(0.0, 0.1);
        self.t += dt;
        let k = |r: f64| 1.0 - (-dt * r).exp();
        self.usage += (self.goal - self.usage) * k(0.9);
        self.cache += (self.cache_goal - self.cache) * k(0.9);
        self.swap += (self.swap_goal - self.swap) * k(0.9);
        let u = self.usage;
        let (px, py) = self.peak();
        let act = ((u - 0.45) / 0.55).clamp(0.0, 1.0);
        let erupt = ((u - 0.90) / 0.10).clamp(0.0, 1.0);
        let sc = (self.w / 1000.0).max(0.6);

        // Bombas de lava.
        self.bomb_acc += (2.0 + 220.0 * act.powf(1.6) + 800.0 * erupt) * dt;
        while self.bomb_acc >= 1.0 {
            self.bomb_acc -= 1.0;
            if self.bombs.len() >= MAX_BOMBS {
                break;
            }
            let sp = self.h * (0.45 + 0.7 * act + 0.7 * erupt) * self.rng.range(0.55, 1.0);
            let ang = -PI / 2.0 + self.rng.gauss() * (0.10 + 0.2 * erupt);
            self.bombs.push(Bomb {
                x: px + self.rng.gauss() * self.crater() * 0.3,
                y: py,
                vx: ang.cos() * sp,
                vy: ang.sin() * sp,
                r: (1.6 + 3.5 * self.rng.next() + 4.0 * erupt * self.rng.next()) * sc,
                life: 1.0,
            });
        }
        let g = self.h * 0.9;
        let mut bombs = std::mem::take(&mut self.bombs);
        for b in &mut bombs {
            b.vy += g * dt;
            b.x += b.vx * dt;
            b.y += b.vy * dt;
            b.life -= dt * 0.3;
            if b.y > self.slope_y(b.x) && b.vy > 0.0 {
                b.life -= dt * 4.0;
                b.vx *= 0.5;
                b.vy = 0.0;
                b.y = self.slope_y(b.x);
            }
        }
        let hh = self.h;
        bombs.retain(|b| b.life > 0.0 && b.y < hh + 20.0);
        self.bombs = bombs;

        // Humo y ceniza.
        self.puff_acc += (1.0 + 6.0 * u + 30.0 * erupt) * dt;
        while self.puff_acc >= 1.0 {
            self.puff_acc -= 1.0;
            if self.puffs.len() > 110 {
                break;
            }
            let max = self.rng.range(4.0, 8.0);
            self.puffs.push(Puff {
                kind: (self.rng.next() * 3.0) as usize % 3,
                rot: self.rng.range(0.0, 2.0 * PI),
                x: px + self.rng.gauss() * self.crater() * 0.4,
                y: py - 4.0,
                r: self.crater() * (0.55 + 0.5 * self.rng.next()),
                vx: self.rng.gauss() * 6.0 + 12.0,
                vy: -(16.0 + 30.0 * u + 70.0 * erupt) * self.rng.range(0.7, 1.3),
                age: 0.0,
                max,
            });
        }
        for p in &mut self.puffs {
            p.age += dt;
            p.x += (p.vx + (self.t * 0.7 + p.y * 0.01).sin() * 8.0) * dt;
            p.y += p.vy * dt;
            p.vy *= 1.0 - 0.25 * dt;
            p.r += (14.0 + 20.0 * u) * dt;
        }
        self.puffs.retain(|p| p.age < p.max);

        // Chispas: del cráter y de las coladas.
        self.ember_acc += (4.0 + 60.0 * act + 150.0 * erupt) * dt;
        while self.ember_acc >= 1.0 {
            self.ember_acc -= 1.0;
            if self.embers.len() > 220 {
                break;
            }
            let from_flow = self.rng.chance(0.5) && !self.flows.is_empty();
            let (x, y) = if from_flow {
                let x = px - self.crater() - self.rng.next() * self.w * 0.3 * act.max(0.3);
                (x, self.slope_y(x))
            } else {
                (px + self.rng.gauss() * self.crater() * 0.5, py)
            };
            self.embers.push(Ember { x, y, vx: self.rng.gauss() * 20.0, vy: -(20.0 + 60.0 * self.rng.next()), life: self.rng.range(0.6, 1.6) });
        }
        for e in &mut self.embers {
            e.life -= dt;
            e.x += (e.vx + (self.t * 3.0 + e.y * 0.05).sin() * 15.0) * dt;
            e.y += e.vy * dt;
            e.vy -= 10.0 * dt;
        }
        self.embers.retain(|e| e.life > 0.0);

        // Relámpagos en la columna de ceniza, solo en erupción.
        self.bolt_acc += 1.6 * erupt * dt;
        if self.bolt_acc >= 1.0 && self.bolts.len() < 3 {
            self.bolt_acc = 0.0;
            let (x0, y0) = (px + self.rng.gauss() * self.crater() * 0.6, py - self.h * (0.12 + 0.25 * self.rng.next()));
            let len = self.h * (0.08 + 0.14 * self.rng.next());
            let dir = self.rng.gauss() * 0.8;
            let n = 9;
            let pts = (0..=n)
                .map(|k| {
                    let u = k as f64 / n as f64;
                    (x0 + dir * len * u + self.rng.gauss() * len * 0.12 * (u * PI).sin(), y0 + len * u)
                })
                .collect();
            self.bolts.push(Bolt { pts, life: 0.28 });
        }
        for b in &mut self.bolts {
            b.life -= dt;
        }
        self.bolts.retain(|b| b.life > 0.0);
        self.shake = if erupt > 0.0 {
            let a = 3.5 * erupt * sc;
            ((self.t * 37.0).sin() * a, (self.t * 29.0).cos() * a * 0.6)
        } else {
            (0.0, 0.0)
        };
    }

    // --- dibujo

    fn lava(pal: &Palette, k: f64) -> Rgb {
        let (y, o, r) = (mix((1.0, 0.93, 0.55), pal.yellow, 0.2), mix((1.0, 0.45, 0.08), pal.orange, 0.25), mix((0.45, 0.06, 0.02), pal.red, 0.3));
        if k < 0.5 { mix(y, o, k * 2.0) } else { mix(o, r, (k - 0.5) * 2.0) }
    }

    fn rock(pal: &Palette) -> Rgb {
        mix((0.30, 0.27, 0.26), mix(pal.dfg, pal.dbg, 0.5), 0.35)
    }

    /// Ruta de la silueta exterior de la montaña entre `x0` y `x1` (cerrada por la base).
    fn mountain_path(&self, cr: &Context, x0: f64, x1: f64) {
        let hz = self.horizon();
        cr.move_to(x0, hz + 2.0);
        let mut x = x0;
        while x <= x1 {
            cr.line_to(x, self.slope_y(x));
            x += 4.0;
        }
        cr.line_to(x1, self.slope_y(x1));
        cr.line_to(x1, hz + 2.0);
        cr.close_path();
    }

    /// Ruta de la cámara de magma (interior del corte).
    fn chamber_path(&self, cr: &Context) {
        let (px, py) = self.peak();
        let hz = self.horizon();
        let n = 40;
        cr.move_to(px, py + 2.0);
        for i in 0..=n {
            let y = py + (hz - py) * i as f64 / n as f64;
            cr.line_to(px + self.chamber_half(y), y);
        }
        cr.line_to(px, hz);
        cr.close_path();
    }

    /// Exterior (mitad izquierda) y corte (mitad derecha): se pintan una vez por tamaño y paleta.
    fn bake(&self, pal: &Palette) {
        let (w, h) = (self.w, self.h);
        let (px, py) = self.peak();
        let hz = self.horizon();
        let rock = Self::rock(pal);
        let tile = noise::grey_tile(256, 24, 3, 7, 0.9);
        let pat = cairo::SurfacePattern::create(&tile);
        pat.set_extend(cairo::Extend::Repeat);

        // Montaña exterior.
        let m = ImageSurface::create(cairo::Format::ARgb32, w.ceil() as i32, h.ceil() as i32).expect("mountain");
        {
            let cr = &Context::new(&m).expect("cairo");
            self.mountain_path(cr, 0.0, px);
            let _ = cr.set_source(lin(0.0, py, 0.0, hz, &[(0.0, lighten(rock, 0.22), 1.0), (0.5, rock, 1.0), (1.0, darken(rock, 0.5), 1.0)]));
            cr.fill_keep();
            cr.save_q();
            cr.clip();
            let _ = cr.set_source(lin(px - w * 0.4, 0.0, px, 0.0, &[(0.0, (1.0, 1.0, 1.0), 0.10), (1.0, (0.0, 0.0, 0.0), 0.25)]));
            cr.paint_q();
            let _ = cr.set_source(&pat);
            cr.set_operator(cairo::Operator::SoftLight);
            let _ = cr.paint_with_alpha(0.6);
            cr.set_operator(cairo::Operator::Over);
            // Relieve de la ladera: barrancos de erosión que bajan del cráter, cada uno con su
            // cara en sombra y su arista iluminada, y coladas antiguas (lomas oscuras) entre ellos.
            cr.set_line_cap(cairo::LineCap::Round);
            for i in 0..22u64 {
                let r1 = noise::rnd(i as i32, 11, 2);
                let x0 = px - self.crater() * (0.7 + 0.6 * r1);
                let x1 = (px - w * 0.08 - r1 * w * 0.40).max(2.0);
                let wd = 1.5 + 3.5 * noise::rnd(i as i32, 12, 2);
                let pts: Vec<(f64, f64)> = (0..=14)
                    .map(|k| {
                        let u = k as f64 / 14.0;
                        let x = x0 + (x1 - x0) * u;
                        (x + (noise::fbm1(u * 4.0 + i as f64 * 3.0, 2, 4) - 0.5) * 14.0 * u, self.slope_y(x) + 1.0 + u * 2.0)
                    })
                    .collect();
                for (dx, c, a, wm) in [(1.5, darken(rock, 0.75), 0.45, 1.0), (-1.0, lighten(rock, 0.35), 0.30, 0.6)] {
                    rgba(cr, c, a);
                    cr.set_line_width(wd * wm);
                    cr.move_to(pts[0].0 + dx, pts[0].1);
                    for p in &pts[1..] {
                        cr.line_to(p.0 + dx, p.1);
                    }
                    cr.stroke_q();
                }
            }
            for i in 0..7u64 {
                let r1 = noise::rnd(i as i32, 21, 2);
                let x0 = px - self.crater() * (1.2 + 0.8 * r1);
                let x1 = (px - w * 0.14 - r1 * w * 0.30).max(2.0);
                rgba(cr, darken(mix(rock, (0.2, 0.12, 0.1), 0.5), 0.3), 0.22);
                cr.set_line_width(6.0 + 10.0 * noise::rnd(i as i32, 22, 2));
                cr.move_to(x0, self.slope_y(x0) + 3.0);
                for k in 1..=10 {
                    let x = x0 + (x1 - x0) * k as f64 / 10.0;
                    cr.line_to(x + (noise::rnd(k, i as i32, 4) - 0.5) * 10.0, self.slope_y(x) + 3.0);
                }
                cr.stroke_q();
            }
            // Ceniza oscura en torno al cráter.
            let _ = cr.set_source(lin(0.0, py, 0.0, py + (hz - py) * 0.35, &[(0.0, (0.05, 0.03, 0.03), 0.45), (1.0, (0.05, 0.03, 0.03), 0.0)]));
            cr.rectangle(0.0, py - 4.0, px, (hz - py) * 0.35 + 4.0);
            cr.fill_q();
            cr.restore_q();
        }

        // Corte: cara de roca estratificada con la cavidad de la cámara.
        let s = ImageSurface::create(cairo::Format::ARgb32, w.ceil() as i32, h.ceil() as i32).expect("section");
        {
            let cr = &Context::new(&s).expect("cairo");
            self.mountain_path(cr, px, w);
            let cut = mix(rock, (0.42, 0.30, 0.22), 0.5);
            let _ = cr.set_source(lin(0.0, py, 0.0, hz, &[(0.0, lighten(cut, 0.15), 1.0), (1.0, darken(cut, 0.45), 1.0)]));
            cr.fill_keep();
            cr.save_q();
            cr.clip();
            for i in 0..22u64 {
                let y0 = py + (hz - py) * (i as f64 + 0.5) / 22.0;
                let warm = i % 3 == 0;
                rgba(cr, if warm { mix(cut, (0.55, 0.35, 0.2), 0.5) } else { darken(cut, 0.35) }, if warm { 0.35 } else { 0.25 });
                cr.set_line_width((hz - py) / 22.0 * if warm { 0.55 } else { 0.35 });
                cr.move_to(px, y0);
                let mut x = px;
                while x <= w + 10.0 {
                    cr.line_to(x, y0 + (noise::fbm1(x * 0.012 + i as f64 * 5.0, 2, 17) - 0.5) * 14.0 + (x - px) * 0.04);
                    x += 10.0;
                }
                cr.stroke_q();
            }
            let _ = cr.set_source(&pat);
            cr.set_operator(cairo::Operator::SoftLight);
            let _ = cr.paint_with_alpha(0.5);
            cr.set_operator(cairo::Operator::Over);
            cr.restore_q();
            // Cavidad: queda transparente, con el borde oscurecido.
            cr.set_operator(cairo::Operator::Clear);
            self.chamber_path(cr);
            cr.fill_q();
            cr.set_operator(cairo::Operator::Over);
            self.chamber_path(cr);
            rgba(cr, (0.0, 0.0, 0.0), 0.55);
            cr.set_line_width(6.0);
            cr.stroke_q();
            rgba(cr, (0.0, 0.0, 0.0), 0.5);
            cr.set_line_width(2.0);
            cr.move_to(px, py);
            cr.line_to(px, hz + 2.0);
            cr.stroke_q();
        }
        // Una sola capa estática: el corte encima de la montaña.
        {
            let cr = &Context::new(&m).expect("cairo");
            let _ = cr.set_source_surface(&s, 0.0, 0.0);
            cr.paint_q();
        }
        *self.mountain.borrow_mut() = Some(m);
        *self.section.borrow_mut() = None;
        *self.sky_key.borrow_mut() = None;
        let crust = lava_tile(pal);
        let molten = molten_tile(pal);
        *self.crust_soft.borrow_mut() = Some(ScaledTiles::new(noise::with_alpha(&crust, 0.6)));
        *self.molten_tile.borrow_mut() = Some(ScaledTiles::new(molten));
        *self.lava_tile.borrow_mut() = Some(ScaledTiles::new(crust));
    }

    /// Pinta una mancha blanda con el color actual, escalada a (rx, ry), con filtro rápido.
    /// Geometría de la colada `i`: puntos de la ladera y anchura.
    fn flow_geometry(&self, i: usize, f: &Flow, act: f64, sc: f64) -> (Vec<(f64, f64)>, f64) {
        let (px, _) = self.peak();
        let c = self.crater();
        let w = self.w;
        let len = (0.18 + 3.0 * f.share).clamp(0.18, 1.0) * (0.4 + 0.6 * act);
        let wd = (3.0 + 30.0 * f.share.sqrt()) * sc;
        let x0 = px - c * (0.6 + 0.9 * i as f64);
        let x1 = px - (c + (w * 0.46 - c) * len);
        let n = 28;
        let pts = (0..=n)
            .map(|k| {
                let x = x0 + (x1 - x0) * k as f64 / n as f64;
                let wig = (noise::fbm1(x * 0.02 + i as f64 * 9.0, 2, 33 + i as u32) - 0.5) * 16.0 * (k as f64 / n as f64);
                (x, self.slope_y(x + wig) + 1.0)
            })
            .collect();
        (pts, wd)
    }

    fn bake_flows(&self, pal: &Palette, font: &str, act: f64, sc: f64, fs: f64) -> Baked {
        let (px, _) = self.peak();
        let hz = self.horizon();
        let c = self.crater();
        let mut b = Baked::new(0.0, 0.0, px + c + 40.0, hz + 20.0);
        let under = b.cx();
        under.select_font_face(font, cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        under.set_font_size(fs);
        for (i, f) in self.flows.iter().enumerate() {
            let (pts, wd) = self.flow_geometry(i, f, act, sc);
            let n = pts.len() - 1;
            let path = |cr: &Context| {
                cr.move_to(pts[0].0, pts[0].1);
                for p in &pts[1..] {
                    cr.line_to(p.0, p.1);
                }
            };
            // Halo de calor sobre la roca y borde oscuro de la costra.
            path(&under);
            rgba(&under, Self::lava(pal, 0.4), 0.22);
            under.set_line_width(wd * 2.4);
            under.stroke_q();
            path(&under);
            rgba(&under, (0.1, 0.02, 0.01), 0.55);
            under.set_line_width(wd + 3.0);
            under.stroke_q();
            let txt = format!("{} · {}", f.name, fmt_bytes(f.mem));
            if let Ok(e) = under.text_extents(&txt) {
                // Las etiquetas se apilan ladera arriba, una fila por colada.
                let (lx, ly2) = (pts[n].0 - 8.0, pts[n].1 - 6.0 - i as f64 * (fs + 6.0));
                b.labels.push((txt, (lx - e.width()).max(4.0), ly2));
            }
            b.strokes.push((pts, wd, wd * 0.3));
        }
        drop(under);
        b
    }

    fn bake_swap(&self, pal: &Palette, font: &str, sc: f64, fs: f64) -> Baked {
        let (w, h) = (self.w, self.h);
        let hz = self.horizon();
        let mut b = Baked::new(0.0, hz, w, h - hz);
        let under = b.cx();
        under.select_font_face(font, cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        under.set_font_size(fs);
        let ncr = self.swappers.len();
        let top = self.swappers[0].1.max(1.0);
        for i in 0..ncr {
            let (name, bytes) = &self.swappers[i];
            let sd = i as u32 * 31 + 7;
            // Repartidas en dos filas para que las etiquetas no se pisen.
            let x0 = w * (0.06 + 0.9 * ((i as f64 + 0.5) / ncr as f64)) - w * 0.06;
            let y0 = hz + 16.0 + (i % 2) as f64 * (h - hz - 36.0) * 0.5 + noise::rnd(i as i32, 2, sd) * 10.0;
            let len = w * (0.05 + 0.16 * (bytes / top).sqrt());
            let pts: Vec<(f64, f64)> = (0..=8).map(|k| (x0 + len * k as f64 / 8.0, y0 + (noise::rnd(k, i as i32, sd) - 0.5) * 12.0)).collect();
            let path = |cr: &Context| {
                cr.move_to(pts[0].0, pts[0].1);
                for p in &pts[1..] {
                    cr.line_to(p.0, p.1);
                }
            };
            path(&under);
            rgba(&under, Self::lava(pal, 0.3), 0.18 + 0.2 * self.swap);
            under.set_line_width(14.0 * sc);
            under.stroke_q();
            path(&under);
            rgba(&under, (0.05, 0.03, 0.03), 0.9);
            under.set_line_width(5.0 * sc);
            under.stroke_q();
            // Grietas secundarias.
            rgba(&under, (0.05, 0.03, 0.03), 0.8);
            under.set_line_width(1.5 * sc);
            for k in [2usize, 5] {
                let (bx, by) = pts[k];
                under.move_to(bx, by);
                under.line_to(bx + (noise::rnd(k as i32, 9, sd) - 0.5) * 24.0, by + (noise::rnd(k as i32, 10, sd) - 0.5) * 16.0);
            }
            under.stroke_q();
            // Etiqueta: programa y cuánto tiene en swap.
            let txt = format!("{} · {} swap", name, fmt_bytes(*bytes));
            if let Ok(e) = under.text_extents(&txt) {
                let x = (x0 + len / 2.0 - e.width() / 2.0).clamp(6.0, w - e.width() - 6.0);
                b.labels.push((txt, x, y0 + 12.0 + fs));
            }
            b.strokes.push((pts, 2.2 * sc, 0.0));
        }
        drop(under);
        b
    }

    fn blob(&self, cr: &Context, k: usize, x: f64, y: f64, rx: f64, ry: f64) {
        self.blobs.paint(cr, k, x, y, rx, ry);
    }

    /// Patrón de lava a una escala, desplazado (dx, dy) (baldosa pre-escalada: ruta rápida).
    fn lava_pattern(&self, tile: &ScaledTiles, scale: f64, dx: f64, dy: f64) -> cairo::SurfacePattern {
        tile.pattern(scale, dx, dy)
    }

    pub fn draw(&self, cr: &Context, pal: &Palette, font: &str) {
        let (w, h) = (self.w, self.h);
        let u = self.usage;
        let act = ((u - 0.45) / 0.55).clamp(0.0, 1.0);
        let erupt = ((u - 0.90) / 0.10).clamp(0.0, 1.0);
        let over = ((u - 0.93) / 0.07).clamp(0.0, 1.0);
        let (px, py) = self.peak();
        let hz = self.horizon();
        let c = self.crater();
        let sc = (w / 1000.0).max(0.6);
        let key = (w as i32, h as i32, crate::sim::hash_str(&pal.raw));
        if *self.cache_key.borrow() != Some(key) {
            self.bake(pal);
            *self.cache_key.borrow_mut() = Some(key);
        }
        let tile_ref = self.lava_tile.borrow();
        let Some(tile) = tile_ref.as_ref() else { return };
        let glow = Self::lava(pal, 0.35);

        cr.save_q();
        // Entero: una traslación fraccionaria obliga a filtrar todas las capas (ruta lenta de Cairo).
        cr.translate(self.shake.0.round(), self.shake.1.round());
        let mut laps = crate::draw::Laps::new("volcano");
        let mut lap = |name: &'static str| laps.lap(name);

        lap("sky");
        // Fondo completo horneado por nivel de uso: cielo, estrellas, resplandor del cráter,
        // llanura y montaña con su corte. Una sola copia por fotograma.
        let rock = Self::rock(pal);
        cr.select_font_face(font, cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        let fs = (11.0 * sc).clamp(10.0, 14.0);
        cr.set_font_size(fs);
        // Coladas y fisuras: su parte fija (halo, borde, grietas) se hornea aparte y se funde en el
        // fondo; por fotograma solo quedan los trazos de lava y las etiquetas.
        let pal_h = crate::sim::hash_str(&pal.raw);
        let fkey: FlowKey = (self.stamp, (act * 40.0).round() as i32, w as i32, h as i32, pal_h);
        if self.flow_cache.borrow().as_ref().map(|c| c.0) != Some(fkey) {
            *self.flow_cache.borrow_mut() = Some((fkey, self.bake_flows(pal, font, act, sc, fs)));
        }
        let swkey: FlowKey = (self.stamp, (self.swap * 20.0).round() as i32, w as i32, h as i32, pal_h);
        if self.swap_total > 0.0 && !self.swappers.is_empty() && self.swap_cache.borrow().as_ref().map(|c| c.0) != Some(swkey) {
            *self.swap_cache.borrow_mut() = Some((swkey, self.bake_swap(pal, font, sc, fs)));
        }
        let skey = ((u * 50.0).round() as i32) ^ (((self.cache * 50.0).round() as i32) << 8) ^ ((self.stamp as i32) << 16) ^ (((self.swap * 20.0).round() as i32) << 12);
        if *self.sky_key.borrow() != Some(skey) || self.sky.borrow().is_none() {
            let s = ImageSurface::create(cairo::Format::ARgb32, (w + 20.0).ceil() as i32, (h + 20.0).ceil() as i32).expect("sky");
            {
                let c = &Context::new(&s).expect("cairo");
                c.translate(10.0, 10.0);
                let sky_top = darken(mix(pal.dbg, (0.02, 0.02, 0.05), 0.5), 0.3);
                let sky_hz = mix(mix(pal.bg, pal.dbg, 0.3), glow, 0.10 + 0.45 * act + 0.3 * erupt);
                let _ = c.set_source(lin(0.0, 0.0, 0.0, hz, &[(0.0, sky_top, 1.0), (1.0, sky_hz, 1.0)]));
                c.paint_q();
                for i in 0..90u64 {
                    let (x, y) = (noise::rnd(i as i32, 1, 1) * w, noise::rnd(i as i32, 2, 1) * hz * 0.7);
                    rgba(c, pal.bfg, (0.15 + 0.5 * noise::rnd(i as i32, 3, 1)) * (1.0 - 0.7 * act));
                    let r = if noise::rnd(i as i32, 4, 1) > 0.85 { 2.2 } else { 1.5 };
                    c.rectangle(x.round(), y.round(), r, r);
                    c.fill_q();
                }
                // Resplandor del cráter sobre el cielo: una mancha blanda escalada.
                let glow_r = w * (0.22 + 0.4 * act + 0.3 * erupt);
                rgba(c, glow, 0.30 + 0.45 * act);
                self.blob(c, 0, px, py, glow_r, glow_r);
                // Llanura.
                let _ = c.set_source(lin(0.0, hz, 0.0, h, &[(0.0, mix(rock, pal.bg, 0.3), 1.0), (1.0, darken(pal.dbg, 0.3), 1.0)]));
                c.rectangle(0.0, hz, w, h - hz);
                c.fill_q();
                // Montaña y corte, con el resplandor de la lava sobre la roca de la cumbre.
                if let Some(m) = self.mountain.borrow().as_ref() {
                    let _ = c.set_source_surface(m, 0.0, 0.0);
                    c.paint_q();
                }
                if act > 0.0 || erupt > 0.0 {
                    rgba(c, glow, 0.35 * act + 0.3 * erupt);
                    self.blob(c, 1, px, py + 20.0 * sc, w * (0.14 + 0.25 * act), w * (0.10 + 0.18 * act));
                }
                let cra = self.crater();
                // Nieve de la cumbre (exterior): la memoria libre.
                let free = (1.0 - u - self.cache).clamp(0.0, 1.0);
                let snow_h = (hz - py) * 0.4 * free.powf(1.2);
                if snow_h > 2.0 {
                    let ys = py + snow_h;
                    let mut x = px - cra;
                    c.move_to(px, py);
                    c.line_to(px - cra, self.slope_y(px - cra));
                    while self.slope_y(x) < ys && x > 0.0 {
                        c.line_to(x, self.slope_y(x));
                        x -= 4.0;
                    }
                    let xl = x;
                    let mut xx = xl;
                    while xx <= px {
                        let jag = (noise::fbm1(xx * 0.05, 2, 21) - 0.5) * snow_h * 0.35;
                        c.line_to(xx, (ys + jag).max(self.slope_y(xx)));
                        xx += 6.0;
                    }
                    c.close_path();
                    let snow = mix((0.96, 0.97, 1.0), pal.bfg, 0.2);
                    let _ = c.set_source(lin(0.0, py, 0.0, ys, &[(0.0, snow, 0.95), (1.0, mix(snow, pal.dbg, 0.35), 0.9)]));
                    c.fill_q();
                }

                for cache in [&self.flow_cache, &self.swap_cache] {
                    if let Some((_, b)) = cache.borrow().as_ref() {
                        let _ = c.set_source_surface(&b.under, b.x0, b.y0);
                        c.paint_q();
                    }
                }
            }
            *self.sky.borrow_mut() = Some(s);
            *self.sky_key.borrow_mut() = Some(skey);
        }
        if let Some(s) = self.sky.borrow().as_ref() {
            let _ = cr.set_source_surface(s, -10.0, -10.0);
            cr.paint_q();
        }
        lap("fondo");
        // Latido del resplandor: una mancha pequeña y tenue encima (la grande ya está en el fondo).
        if act > 0.0 {
            let glow_r = w * (0.12 + 0.2 * act) * (0.9 + 0.1 * (self.t * 2.0).sin());
            rgba(cr, glow, 0.10 * act * (0.5 + 0.5 * (self.t * 2.0).sin()));
            self.blob(cr, 0, px, py, glow_r, glow_r);
        }

        // --- Interior del corte: magma dentro de la cámara.
        let ly = self.level_y(u);
        let cy = self.level_y((u + self.cache).min(1.0));
        cr.save_q();
        self.chamber_path(cr);
        cr.clip();
        let _ = cr.set_source(lin(0.0, py, 0.0, hz, &[(0.0, darken(rock, 0.7), 1.0), (1.0, darken(rock, 0.85), 1.0)]));
        cr.paint_q();
        // Caché: magma frío (costra oscura) por encima del fundido.
        if cy < ly - 1.0 {
            let _ = cr.set_source(self.lava_pattern(tile, 1.2 * sc, 0.0, self.t * 3.0));
            cr.rectangle(px, cy, w, ly - cy);
            cr.fill_q();
            rgba(cr, (0.05, 0.02, 0.02), 0.72);
            cr.rectangle(px, cy, w, ly - cy);
            cr.fill_q();
        }
        // Magma fundido: superficie ondulada, fundido con remolinos que sube despacio, balsas de
        // costra que derivan y plumas de convección.
        let molten_ref = self.molten_tile.borrow();
        let crust_ref = self.crust_soft.borrow();
        let (Some(molten), Some(crust_soft)) = (molten_ref.as_ref(), crust_ref.as_ref()) else { return };
        let pulse = 0.5 + 0.5 * (self.t * 2.2).sin();
        let top = |cr: &Context| {
            let amp = 2.2 * sc;
            cr.move_to(px, ly + amp * (self.t * 1.7).sin());
            let mut x = px;
            while x <= w + 8.0 {
                cr.line_to(x, ly + amp * ((x - px) * 0.045 + self.t * 1.7).sin() + amp * 0.5 * ((x - px) * 0.11 - self.t * 2.6).sin());
                x += 8.0;
            }
            cr.line_to(w + 8.0, hz + 4.0);
            cr.line_to(px, hz + 4.0);
            cr.close_path();
        };
        top(cr);
        let _ = cr.set_source(molten.pattern(1.3 * sc, self.t * 5.0, -self.t * 9.0));
        cr.fill_q();
        // Profundidad: el fondo de la cámara se oscurece hacia el rojo (solo brilla la superficie).
        let deep = lava_ramp(pal, 0.18);
        let _ = cr.set_source(lin(0.0, ly, 0.0, hz, &[(0.0, deep, 0.0), (0.45, deep, 0.18), (1.0, deep, 0.42)]));
        cr.rectangle(px, ly - 4.0, w, hz - ly + 8.0);
        cr.fill_q();
        // Balsas de costra oscura que flotan y derivan sobre el fundido.
        for i in 0..3u64 {
            let ph = (self.t * 0.04 * (0.6 + noise::rnd(i as i32, 1, 55)) + noise::rnd(i as i32, 2, 55)).rem_euclid(1.0);
            let y = ly + (hz - ly) * (0.08 + 0.84 * noise::rnd(i as i32, 3, 55));
            if hz - ly < 30.0 * sc {
                break;
            }
            let half = self.chamber_half(y);
            let x = px + half * (0.1 + 0.8 * ph);
            let r = (9.0 + 12.0 * noise::rnd(i as i32, 4, 55)) * sc;
            let _ = cr.set_source(crust_soft.pattern(0.7 * sc, x * 0.5, y * 0.5));
            self.blob(cr, (i as usize) % 3, x, y, r, r * 0.55);
        }
        // Convección: plumas incandescentes que suben y se abren al llegar a la superficie.
        for i in 0..8u64 {
            let ph = (self.t * (0.18 + 0.15 * noise::rnd(i as i32, 1, 44)) + noise::rnd(i as i32, 2, 44) * 7.0).rem_euclid(1.0);
            let y = hz - (hz - ly) * ph;
            let half = self.chamber_half(y);
            let x = px + half * (0.15 + 0.7 * noise::rnd(i as i32, 3, 44)) + 4.0 * sc * (self.t * 1.3 + i as f64).sin();
            let r = (6.0 + 9.0 * noise::rnd(i as i32, 4, 44)) * sc * (0.6 + 0.8 * ph);
            rgba(cr, lava_ramp(pal, 0.92), 0.28 * (1.0 - ph * ph));
            self.blob(cr, (i as usize) % 3, x, y, r, r * (1.2 - 0.5 * ph));
        }
        // Menisco: borde incandescente del nivel que late, con su resplandor hacia arriba.
        let _ = cr.set_source(lin(0.0, ly - 12.0 * sc, 0.0, ly + 7.0 * sc, &[(0.0, lava_ramp(pal, 0.95), 0.0), (0.62, lava_ramp(pal, 0.95), 0.7 + 0.2 * pulse), (1.0, lava_ramp(pal, 0.95), 0.0)]));
        cr.rectangle(px, ly - 12.0 * sc, w, 19.0 * sc);
        cr.fill_q();
        // Luz del magma sobre las paredes de la cámara.
        let g = LinearGradient::new(0.0, ly, 0.0, ly - (hz - py) * 0.35);
        g.add_color_stop_rgba(0.0, glow.0, glow.1, glow.2, 0.55);
        g.add_color_stop_rgba(1.0, glow.0, glow.1, glow.2, 0.0);
        let _ = cr.set_source(&g);
        cr.rectangle(px, py, w, ly - py);
        cr.fill_q();
        cr.restore_q();

        // Borde de la cavidad por encima del magma (la montaña está debajo, en el fondo horneado).
        self.chamber_path(cr);
        rgba(cr, (0.0, 0.0, 0.0), 0.55);
        cr.set_line_width(4.0);
        cr.stroke_q();

        lap("magma");
        // Coladas (exterior): los programas que más RAM usan, con textura de lava. Todo lo que
        // no se mueve (halo, borde, brillo, etiquetas) está horneado; aquí solo se pinta la lava.
        {
            if let Some((_, b)) = self.flow_cache.borrow().as_ref() {
                b.paint(cr, &self.lava_pattern(tile, 0.9 * sc, -self.t * 20.0, self.t * 12.0), lava_ramp(pal, 0.97), pal, fs, self.t);
            }
        }

        lap("flows");
        // Rebose: lava que desborda el cráter y baja en sábana por la ladera exterior.
        if over > 0.0 {
            let reach = w * 0.46 * over;
            cr.move_to(px, py);
            let mut x = px - c;
            while x > px - c - reach {
                cr.line_to(x, self.slope_y(x) - 2.0);
                x -= 4.0;
            }
            let mut xx = x;
            while xx <= px - c {
                cr.line_to(xx, self.slope_y(xx) + (6.0 + 10.0 * (1.0 - (px - c - xx) / reach.max(1.0))) * sc);
                xx += 4.0;
            }
            cr.line_to(px, py + 8.0 * sc);
            cr.close_path();
            let _ = cr.set_source(molten.pattern(1.2 * sc, -self.t * 25.0, self.t * 18.0));
            let _ = cr.fill_preserve();
            rgba(cr, lava_ramp(pal, 0.95), 0.25 * over);
            cr.fill_q();
        }

        lap("overflow");
        // Lago de lava del cráter.
        cr.save_q();
        cr.translate(px, py + 2.0);
        cr.scale(c, c * 0.3);
        cr.arc(0.0, 0.0, 1.0, 0.0, 2.0 * PI);
        cr.restore_q();
        // Fundido con pieles de costra encima (más costra cuanto más tranquilo), y el borde al rojo.
        let _ = cr.set_source(molten.pattern(0.8 * sc, self.t * 10.0, self.t * 5.0));
        cr.fill_keep();
        if act < 0.85 {
            let _ = cr.set_source(crust_soft.pattern(0.5 * sc, -self.t * 4.0, self.t * 2.0));
            cr.fill_keep();
        }
        rgba(cr, (0.08, 0.02, 0.01), 0.35 * (1.0 - act));
        cr.fill_q();

        // Bombas, chispas y humo.
        lap("lake");
        // Bombas agrupadas en cuatro tonos (pocos rellenos grandes en vez de cientos pequeños).
        for bucket in 0..4 {
            let (lo, hi) = (bucket as f64 * 0.25, (bucket + 1) as f64 * 0.25);
            let life = (lo + hi) / 2.0;
            let col = Self::lava(pal, 1.0 - life);
            let sel = || self.bombs.iter().filter(|b| b.life >= lo && b.life < hi);
            if sel().next().is_none() {
                continue;
            }
            // Estela (desenfoque de movimiento) y brasa; sin discos de halo.
            rgba(cr, col, 0.5 * life);
            cr.set_line_width(1.6 * sc);
            for b in sel() {
                cr.move_to(b.x, b.y);
                cr.line_to(b.x - b.vx * 0.08, b.y - b.vy * 0.08);
            }
            cr.stroke_q();
            rgba(cr, lighten(col, 0.2), life.max(0.35));
            for b in sel() {
                cr.new_sub_path();
                cr.arc(b.x, b.y, b.r * 0.7, 0.0, 2.0 * PI);
            }
            cr.fill_q();
        }
        rgba(cr, Self::lava(pal, 0.1), 0.85);
        for e in &self.embers {
            cr.rectangle(e.x.round(), e.y.round(), 2.0 * sc, 2.0 * sc);
        }
        cr.fill_q();
        let smoke = mix((0.36, 0.34, 0.34), pal.dfg, 0.25);
        lap("bombs");
        // Humo: manchas blandas e irregulares (sin círculos), que nacen incandescentes y se apagan.
        for p in &self.puffs {
            let k = p.age / p.max;
            // Nace con un leve rescoldo y enseguida es ceniza gris, grande y transparente.
            let col = mix(Self::lava(pal, 0.7), smoke, (0.45 + k * 4.0).clamp(0.0, 1.0));
            rgba(cr, col, (0.5 * (1.0 - k)) * (0.6 + 0.4 * u));
            let _ = p.rot;
            self.blob(cr, p.kind, p.x, p.y, p.r, p.r * 0.8);
        }

        // Relámpagos: un halo ancho y el hilo brillante, con la ceniza alrededor iluminada.
        for b in &self.bolts {
            let k = b.life / 0.28;
            let (bx, by) = b.pts[b.pts.len() / 2];
            rgba(cr, (0.85, 0.8, 1.0), 0.18 * k);
            self.blob(cr, 2, bx, by, c * 1.6, c * 1.2);
            for (wd, al, col) in [(5.0 * sc, 0.25, (0.7, 0.6, 1.0)), (1.5 * sc, 0.95, (1.0, 0.97, 1.0))] {
                rgba(cr, col, al * k);
                cr.set_line_width(wd);
                cr.move_to(b.pts[0].0, b.pts[0].1);
                for p in &b.pts[1..] {
                    cr.line_to(p.0, p.1);
                }
                cr.stroke_q();
            }
        }
        lap("smoke");
        // Swap: la llanura se agrieta y por las fisuras asoma el magma. Una fisura por programa
        // (las más largas, los que más swap tienen), con su etiqueta; horneadas salvo la lava.
        if self.swap_total > 0.0 && !self.swappers.is_empty() {
            if let Some((_, b)) = self.swap_cache.borrow().as_ref() {
                b.paint(cr, &self.lava_pattern(tile, 0.6 * sc, self.t * 5.0, 0.0), lava_ramp(pal, 0.97), pal, fs, self.t);
            }
        }

        lap("swap");
        // --- Escala: 0–100 % a la derecha del corte, con el nivel actual.
        let xs = (px + self.chamber_half(hz) + 26.0 * sc).min(w - 70.0 * sc);
        rgba(cr, pal.fg, 0.7);
        cr.set_line_width(1.0);
        cr.move_to(xs + 0.5, py);
        cr.line_to(xs + 0.5, hz);
        cr.stroke_q();
        cr.set_font_size(fs * 0.9);
        for i in 0..=10 {
            let y = self.level_y(i as f64 / 10.0).round() + 0.5;
            let major = i % 5 == 0;
            rgba(cr, pal.fg, 0.7);
            cr.move_to(xs, y);
            cr.line_to(xs + if major { 10.0 } else { 5.0 }, y);
            cr.stroke_q();
            if i % 2 == 0 {
                rgba(cr, if major { pal.bfg } else { pal.dfg }, 0.9);
                cr.move_to(xs + 14.0, y + fs * 0.35);
                let _ = cr.show_text(&format!("{}%", i * 10));
            }
        }
        rgba(cr, Self::lava(pal, 0.0), 0.9);
        cr.set_line_width(1.5);
        cr.set_dash(&[6.0, 4.0], 0.0);
        cr.move_to(px, ly.round() + 0.5);
        cr.line_to(xs + 10.0, ly.round() + 0.5);
        cr.stroke_q();
        cr.set_dash(&[], 0.0);
        cr.move_to(xs + 10.0, ly);
        cr.line_to(xs + 2.0, ly - 5.0);
        cr.line_to(xs + 2.0, ly + 5.0);
        cr.close_path();
        cr.fill_q();
        cr.set_font_size(fs + 3.0);
        let txt = format!("{:.0}%", u * 100.0);
        if let Ok(e) = cr.text_extents(&txt) {
            let bx = (xs + 40.0 * sc).min(w - e.width() - 8.0);
            rgba(cr, pal.dbg, 0.75);
            cr.rectangle(bx - 5.0, ly - fs - 6.0, e.width() + 10.0, fs + 12.0);
            cr.fill_q();
            rgba(cr, Self::lava(pal, 0.0), 1.0);
            cr.move_to(bx, ly + fs * 0.4);
            let _ = cr.show_text(&txt);
        }
        cr.set_font_size(fs);
        let label = |cr: &Context, y: f64, txt: &str, col: Rgb| {
            if let Ok(e) = cr.text_extents(txt) {
                let x = px + 10.0;
                rgba(cr, pal.dbg, 0.55);
                cr.rectangle(x - 4.0, y - fs - 1.0, e.width() + 8.0, fs + 6.0);
                cr.fill_q();
                rgba(cr, col, 1.0);
                cr.move_to(x, y);
                let _ = cr.show_text(txt);
            }
        };
        if hz - ly > fs * 2.0 {
            label(cr, (ly + 6.0 + fs).min(hz - 6.0), &format!("{} {}", tr("mem_used"), fmt_bytes(self.used)), lighten(pal.bfg, 0.1));
        }
        if ly - cy > fs * 1.8 {
            label(cr, cy + 6.0 + fs, &format!("{} {}", tr("mem_cache"), fmt_bytes(self.cached)), pal.fg);
        }
        if cy - py > fs * 2.2 {
            label(cr, py + 8.0 + fs, &format!("{} {}", tr("mem_free"), fmt_bytes((self.total - self.used - self.cached).max(0.0))), pal.fg);
        }

        lap("scale");
        // Leyenda de las fisuras, abajo a la derecha.
        if self.swap_total > 0.0 && self.swap > 0.02 {
            cr.set_font_size(fs);
            let txt = format!("{} → {}", trf("mem_swap", &[fmt_bytes(self.swap_used), fmt_bytes(self.swap_total)]), tr("mem_fissures"));
            if let Ok(e) = cr.text_extents(&txt) {
                let (x, y) = (w - e.width() - 16.0, h - 12.0);
                rgba(cr, pal.dbg, 0.65);
                cr.rectangle(x - 6.0, y - fs - 3.0, e.width() + 12.0, fs + 8.0);
                cr.fill_q();
                rgba(cr, Self::lava(pal, 0.05), 1.0);
                cr.move_to(x, y);
                let _ = cr.show_text(&txt);
            }
        }
        // Cifras arriba a la izquierda.
        cr.set_font_size(fs + 2.0);
        let lines = [
            format!("RAM {}", fmt_bytes(self.used)),
            trf("ram_of", &[format!("{:.0}", u * 100.0), fmt_bytes(self.total)]),
            trf("mem_cached", &[fmt_bytes(self.cached)]),
            if self.swap_total > 0.0 { trf("mem_swap", &[fmt_bytes(self.swap_used), fmt_bytes(self.swap_total)]) } else { String::new() },
        ];
        for (i, l) in lines.iter().enumerate().filter(|(_, l)| !l.is_empty()) {
            if let Ok(e) = cr.text_extents(l) {
                rgba(cr, pal.dbg, 0.55);
                cr.rectangle(10.0, 10.0 + (fs + 8.0) * i as f64, e.width() + 12.0, fs + 8.0);
                cr.fill_q();
            }
            rgba(cr, if i == 0 { lighten(pal.bfg, 0.1) } else { pal.fg }, 1.0);
            cr.move_to(16.0, 10.0 + (fs + 8.0) * (i as f64 + 1.0) - 5.0);
            let _ = cr.show_text(l);
        }
        cr.restore_q();
        lap("textos");
        drop(lap);
        laps.done(60);
    }
}

/// Widget: un DrawingArea con su reloj (solo corre mientras está a la vista).
pub fn widget(v: Rc<RefCell<Volcano>>, pal: Rc<RefCell<Palette>>, font: String) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    let (v2, p2) = (v.clone(), pal.clone());
    let prof = std::env::var_os("TA_PROF").is_some();
    let acc: Rc<RefCell<(f64, u32)>> = Rc::new(RefCell::new((0.0, 0)));
    area.set_draw_func(move |_, cr, w, h| {
        let t0 = std::time::Instant::now();
        let mut vol = v2.borrow_mut();
        vol.resize(w as f64, h as f64);
        vol.draw(cr, &p2.borrow(), &font);
        if prof {
            let mut a = acc.borrow_mut();
            a.0 += t0.elapsed().as_secs_f64() * 1000.0;
            a.1 += 1;
            if a.1 == 60 {
                eprintln!("volcano: {:.1} ms/draw", a.0 / 60.0);
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
