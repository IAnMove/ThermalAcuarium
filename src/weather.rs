//! Pestaña Sensores, skin Arcade: el clima. La temperatura de la CPU es el tiempo que hace: con la
//! CPU fresca, cielo despejado y sol suave; templada, calima; caliente, ola de calor con el aire
//! vibrando; crítica, tormenta de fuego con relámpagos y ceniza. Los ventiladores son el viento (las
//! nubes corren, la hierba y los árboles se doblan) y la carga media es la nubosidad. GPU y NVMe son
//! dos termómetros más. Una escala de temperatura al lado lo hace legible de un vistazo.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::cairo::{self, Context, ImageSurface, RadialGradient};
use gtk::glib;
use gtk::prelude::*;

use crate::draw::{Quiet, lin, rgba};
use crate::i18n::tr;
use crate::reef::noise;
use crate::sim::Rng;
use crate::stamps::Stamps;
use crate::sys::Reading;
use crate::theme::{Palette, Rgb, darken, lighten, mix};

pub const WARM: f64 = 75.0;
pub const HOT: f64 = 88.0;
pub const CRIT: f64 = 95.0;
const T_MIN: f64 = 30.0;
const T_MAX: f64 = 105.0;

struct Cloud {
    x: f64,
    y: f64,
    w: f64,
    /// (dx, dy, radio, variante de sello)
    puffs: Vec<(f64, f64, f64, usize)>,
    z: f64,
}

struct Bolt {
    pts: Vec<(f64, f64)>,
    life: f64,
}

struct Ash {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    s: f64,
}

pub struct Weather {
    pub w: f64,
    pub h: f64,
    t: f64,
    temp: f64,
    temp_goal: f64,
    have_temp: bool,
    gpu: Option<f64>,
    nvme: Option<f64>,
    wind: f64,
    wind_goal: f64,
    fan: Option<i64>,
    fan_max: Option<i64>,
    cover: f64,
    cover_goal: f64,
    load: f64,
    ncpu: f64,
    clouds: Vec<Cloud>,
    bolts: Vec<Bolt>,
    ash: Vec<Ash>,
    bolt_timer: f64,
    hist: VecDeque<f64>,
    rng: Rng,
    cache_key: RefCell<Option<(i32, i32, u64)>>,
    land: RefCell<Option<ImageSurface>>,
    sky_key: RefCell<Option<i32>>,
    sky: RefCell<Option<ImageSurface>>,
    blobs: Stamps,
    /// Hierba horneada por (fase de inclinación, calor, tamaño).
    grass: RefCell<Vec<((i32, i32, i32, i32), ImageSurface)>>,
}

impl Weather {
    pub fn new(ncpu: f64) -> Weather {
        let mut rng = Rng::new(0xC10D);
        let clouds = (0..9)
            .map(|i| {
                let z = 0.4 + 0.6 * rng.next();
                let w = (70.0 + 120.0 * rng.next()) * z;
                let puffs = (0..7)
                    .map(|k| {
                        let u = (k as f64 / 6.0) - 0.5;
                        // Más altas y gordas en el centro, bajas en los extremos.
                        let r = w * (0.20 + 0.18 * (1.0 - u.abs() * 1.6).max(0.2) + 0.06 * rng.next());
                        (u * w * 0.9, -r * 0.35 * (1.0 - u.abs() * 1.4).max(0.0) + rng.gauss() * w * 0.03, r, (rng.next() * 3.0) as usize)
                    })
                    .collect();
                Cloud { x: rng.next() * 1400.0, y: 40.0 + 120.0 * rng.next() * z + i as f64 * 6.0, w, puffs, z }
            })
            .collect();
        Weather {
            w: 1000.0,
            h: 600.0,
            t: 0.0,
            temp: 50.0,
            temp_goal: 50.0,
            have_temp: false,
            gpu: None,
            nvme: None,
            wind: 0.2,
            wind_goal: 0.2,
            fan: None,
            fan_max: None,
            cover: 0.3,
            cover_goal: 0.3,
            load: 0.0,
            ncpu,
            clouds,
            bolts: vec![],
            ash: vec![],
            bolt_timer: 2.0,
            hist: VecDeque::new(),
            rng,
            cache_key: RefCell::new(None),
            land: RefCell::new(None),
            sky_key: RefCell::new(None),
            sky: RefCell::new(None),
            blobs: Stamps::new(3, 3100),
            grass: RefCell::new(vec![]),
        }
    }

    pub fn set_reading(&mut self, r: &Reading, temp: Option<f64>) {
        if let Some(t) = temp {
            if !self.have_temp {
                self.temp = t;
            }
            self.temp_goal = t;
            self.have_temp = true;
            if self.hist.len() == 180 {
                self.hist.pop_front();
            }
            self.hist.push_back(t);
        }
        self.gpu = r.gpu;
        self.nvme = r.nvme;
        self.fan = r.fan;
        self.fan_max = r.fan_max;
        self.wind_goal = match (r.fan, r.fan_max) {
            (Some(f), Some(m)) if m > 0 => (f as f64 / m as f64).clamp(0.0, 1.0),
            (Some(f), _) => (f as f64 / 5000.0).clamp(0.0, 1.0),
            _ => 0.15 + 0.5 * ((self.temp_goal - 50.0) / 50.0).clamp(0.0, 1.0),
        };
        self.load = r.load;
        self.cover_goal = (r.load / self.ncpu.max(1.0)).clamp(0.0, 1.0);
    }

    pub fn resize(&mut self, w: f64, h: f64) {
        if (w - self.w).abs() > 0.5 || (h - self.h).abs() > 0.5 {
            self.w = w;
            self.h = h;
        }
    }

    fn heat(&self) -> f64 {
        ((self.temp - T_MIN) / (T_MAX - T_MIN)).clamp(0.0, 1.0)
    }
    fn hot(&self) -> f64 {
        ((self.temp - WARM) / (HOT - WARM)).clamp(0.0, 1.0)
    }
    fn crit(&self) -> f64 {
        ((self.temp - HOT) / (CRIT - HOT)).clamp(0.0, 1.0)
    }
    fn horizon(&self) -> f64 {
        self.h * 0.72
    }

    pub fn step(&mut self, dt: f64) {
        let dt = dt.clamp(0.0, 0.1);
        self.t += dt;
        let k = |r: f64| 1.0 - (-dt * r).exp();
        self.temp += (self.temp_goal - self.temp) * k(1.2);
        self.wind += (self.wind_goal - self.wind) * k(0.8);
        self.cover += (self.cover_goal - self.cover) * k(0.5);
        let speed = 6.0 + 90.0 * self.wind;
        for c in &mut self.clouds {
            c.x += speed * c.z * dt;
            if c.x - c.w > self.w + 20.0 {
                c.x = -c.w - 10.0;
                c.y = 30.0 + self.rng.next() * self.h * 0.22;
            }
        }
        let crit = self.crit();
        // Relámpagos en la tormenta.
        self.bolt_timer -= dt * (0.3 + 2.5 * crit);
        if crit > 0.1 && self.bolt_timer <= 0.0 {
            self.bolt_timer = self.rng.range(0.6, 2.5);
            let x0 = self.rng.range(self.w * 0.1, self.w * 0.9);
            let mut pts = vec![(x0, self.h * 0.12)];
            let mut y = self.h * 0.12;
            let mut x = x0;
            while y < self.horizon() * 0.9 {
                y += self.rng.range(14.0, 34.0);
                x += self.rng.gauss() * 18.0;
                pts.push((x, y));
            }
            self.bolts.push(Bolt { pts, life: 0.35 });
        }
        for b in &mut self.bolts {
            b.life -= dt;
        }
        self.bolts.retain(|b| b.life > 0.0);
        // Ceniza/brasas que caen con la tormenta de fuego.
        if crit > 0.0 && self.ash.len() < 300 {
            for _ in 0..((6.0 * crit) as usize + 1) {
                self.ash.push(Ash { x: self.rng.next() * self.w, y: -5.0, vx: 20.0 + 80.0 * self.wind, vy: 30.0 + 50.0 * self.rng.next(), s: 1.0 + 2.0 * self.rng.next() });
            }
        }
        for a in &mut self.ash {
            a.x += (a.vx + (self.t * 2.0 + a.y * 0.02).sin() * 20.0) * dt;
            a.y += a.vy * dt;
        }
        let h = self.h;
        self.ash.retain(|a| a.y < h);
    }

    // --- colores del cielo según la temperatura

    fn sky(&self, pal: &Palette) -> (Rgb, Rgb) {
        let heat = self.heat();
        let cool_top = mix((0.25, 0.50, 0.80), pal.blue, 0.3);
        let cool_hz = mix((0.72, 0.84, 0.94), lighten(pal.blue, 0.5), 0.3);
        let warm_top = mix((0.55, 0.52, 0.55), pal.dfg, 0.3);
        let warm_hz = mix((0.92, 0.78, 0.55), pal.yellow, 0.3);
        let hot_top = mix((0.45, 0.18, 0.08), pal.red, 0.3);
        let hot_hz = mix((1.0, 0.55, 0.15), pal.orange, 0.3);
        let crit_top = mix((0.10, 0.03, 0.03), pal.dbg, 0.4);
        let crit_hz = mix((0.75, 0.15, 0.05), pal.red, 0.4);
        let seg = |a: Rgb, b: Rgb, c: Rgb, d: Rgb| {
            if heat < 0.6 {
                mix(a, b, heat / 0.6)
            } else if heat < 0.78 {
                mix(b, c, (heat - 0.6) / 0.18)
            } else {
                mix(c, d, (heat - 0.78) / 0.22)
            }
        };
        (seg(cool_top, warm_top, hot_top, crit_top), seg(cool_hz, warm_hz, hot_hz, crit_hz))
    }

    fn bake(&self, pal: &Palette) {
        let (w, h) = (self.w, self.h);
        let hz = self.horizon();
        let surf = ImageSurface::create(cairo::Format::ARgb32, w.ceil() as i32, h.ceil() as i32).expect("land");
        let cr = &Context::new(&surf).expect("cairo");
        // Montañas lejanas en capas (perspectiva atmosférica), luego colinas y el prado.
        for (i, base, amp, k) in [(0u32, 0.86, 0.18, 0.25), (1, 0.92, 0.14, 0.5), (2, 0.985, 0.08, 0.8)] {
            cr.move_to(0.0, hz + 20.0);
            let mut x = 0.0;
            while x <= w + 6.0 {
                let n = noise::fbm1(x / w * (3.0 + i as f64) + i as f64 * 11.0, 4, 500 + i);
                cr.line_to(x, hz * base - hz * amp * n.powf(1.2));
                x += 5.0;
            }
            cr.line_to(w, hz + 20.0);
            cr.close_path();
            let c = mix(mix((0.42, 0.50, 0.44), pal.dfg, 0.4), (0.22, 0.34, 0.20), k);
            // Más claras en las crestas (luz) y más oscuras en la falda, con bruma entre capas.
            let _ = cr.set_source(lin(0.0, hz * (base - amp), 0.0, hz, &[(0.0, lighten(c, 0.18), 1.0), (1.0, darken(c, 0.25), 1.0)]));
            cr.fill_keep();
            cr.save_q();
            cr.clip();
            let mtile = noise::grey_tile(256, 28, 3, 707 + i, 1.1);
            let mpat = cairo::SurfacePattern::create(&mtile);
            mpat.set_extend(cairo::Extend::Repeat);
            cr.set_operator(cairo::Operator::SoftLight);
            let _ = cr.set_source(&mpat);
            let _ = cr.paint_with_alpha(0.28 - 0.06 * i as f64);
            cr.set_operator(cairo::Operator::Over);
            cr.restore_q();
        }
        let grass = mix((0.30, 0.50, 0.24), mix(pal.green, pal.dbg, 0.4), 0.3);
        let _ = cr.set_source(lin(0.0, hz, 0.0, h, &[(0.0, lighten(grass, 0.1), 1.0), (1.0, darken(grass, 0.4), 1.0)]));
        cr.rectangle(0.0, hz, w, h - hz);
        cr.fill_q();
        let tile = noise::grey_tile(256, 10, 3, 606, 1.2);
        let pat = cairo::SurfacePattern::create(&tile);
        pat.set_extend(cairo::Extend::Repeat);
        cr.save_q();
        cr.rectangle(0.0, hz, w, h - hz);
        cr.clip();
        cr.set_operator(cairo::Operator::SoftLight);
        let _ = cr.set_source(&pat);
        let _ = cr.paint_with_alpha(0.7);
        cr.set_operator(cairo::Operator::Over);
        cr.restore_q();
        *self.land.borrow_mut() = Some(surf);
    }

    fn tree(cr: &Context, x: f64, y: f64, s: f64, bend: f64, col: Rgb) {
        // Tronco y tres pisos de copa, inclinados por el viento.
        rgba(cr, darken(col, 0.6), 1.0);
        cr.set_line_width((s * 0.12).max(1.5));
        cr.move_to(x, y);
        cr.line_to(x + bend * s * 0.5, y - s * 1.1);
        cr.stroke_q();
        for k in 0..3 {
            let yy = y - s * (0.45 + 0.4 * k as f64);
            let ww = s * (0.9 - 0.22 * k as f64);
            let bx = bend * s * (0.2 + 0.3 * k as f64);
            cr.move_to(x + bx, yy - ww * 0.9);
            cr.line_to(x + bx + ww * 0.6 + bend * s * 0.15, yy + ww * 0.25);
            cr.line_to(x + bx - ww * 0.6 + bend * s * 0.15, yy + ww * 0.25);
            cr.close_path();
            rgba(cr, if k == 2 { lighten(col, 0.15) } else { col }, 1.0);
            cr.fill_q();
        }
    }

    fn bush(cr: &Context, blobs: &Stamps, x: f64, y: f64, s: f64, bend: f64, col: Rgb) {
        // Tronco corto y copa redonda de tres manchas, inclinada por el viento.
        rgba(cr, darken(col, 0.6), 1.0);
        cr.set_line_width((s * 0.14).max(1.5));
        cr.move_to(x, y);
        cr.line_to(x + bend * s * 0.3, y - s * 0.6);
        cr.stroke_q();
        for (k, (dx, dy, r)) in [(0.0, -0.95, 0.55), (-0.45, -0.7, 0.42), (0.45, -0.72, 0.4)].into_iter().enumerate() {
            rgba(cr, if k == 0 { lighten(col, 0.12) } else { darken(col, 0.1) }, 1.0);
            blobs.paint(cr, k, x + dx * s + bend * s * (0.35 - dy * 0.4), y + dy * s, r * s, r * s * 0.85);
        }
    }

    pub fn draw(&self, cr: &Context, pal: &Palette, font: &str) {
        let (w, h) = (self.w, self.h);
        let hz = self.horizon();
        let heat = self.heat();
        let hot = self.hot();
        let crit = self.crit();
        let sc = (w / 1000.0).max(0.6);
        let key = (w as i32, h as i32, crate::sim::hash_str(&pal.raw));
        if *self.cache_key.borrow() != Some(key) {
            self.bake(pal);
            *self.cache_key.borrow_mut() = Some(key);
        }
        cr.select_font_face(font, cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        let fs = (11.0 * sc).clamp(10.0, 14.0);
        cr.set_font_size(fs);
        let mut laps = crate::draw::Laps::new("weather");

        // Cielo y sol: se hornean por grado de temperatura (cambian despacio).
        let (top, hzc) = self.sky(pal);
        let skey = (self.temp * 2.0).round() as i32;
        if *self.sky_key.borrow() != Some(skey) || self.sky.borrow().is_none() {
            let s = ImageSurface::create(cairo::Format::ARgb32, w.ceil() as i32, hz.ceil() as i32 + 1).expect("sky");
            {
                let c = &Context::new(&s).expect("cairo");
                let _ = c.set_source(lin(0.0, 0.0, 0.0, hz, &[(0.0, top, 1.0), (1.0, hzc, 1.0)]));
                c.paint_q();
                let (sx, sy) = (w * 0.72, hz * (0.22 + 0.45 * heat));
                let sr = (28.0 + 60.0 * heat) * sc;
                let sun = mix(mix((1.0, 0.97, 0.85), (1.0, 0.55, 0.15), hot), (0.9, 0.2, 0.05), crit);
                let g = RadialGradient::new(sx, sy, sr * 0.6, sx, sy, sr * (3.0 + 2.0 * hot));
                g.add_color_stop_rgba(0.0, sun.0, sun.1, sun.2, 0.55 + 0.3 * hot);
                g.add_color_stop_rgba(1.0, sun.0, sun.1, sun.2, 0.0);
                let _ = c.set_source(&g);
                c.paint_q();
                rgba(c, sun, 1.0);
                c.arc(sx, sy, sr, 0.0, 2.0 * PI);
                c.fill_q();
            }
            *self.sky.borrow_mut() = Some(s);
            *self.sky_key.borrow_mut() = Some(skey);
        }
        if let Some(s) = self.sky.borrow().as_ref() {
            let _ = cr.set_source_surface(s, 0.0, 0.0);
            cr.paint_q();
        }

        // Tierra.
        if let Some(l) = self.land.borrow().as_ref() {
            let _ = cr.set_source_surface(l, 0.0, 0.0);
            cr.paint_q();
        }
        // Tinte del suelo según el calor (la hierba se agosta) y ceniza con la tormenta.
        rgba(cr, mix((0.95, 0.75, 0.3), (0.25, 0.08, 0.04), crit), 0.35 * hot + 0.3 * crit);
        cr.rectangle(0.0, hz * 0.8, w, h - hz * 0.8);
        cr.fill_q();

        laps.lap("fondo");
        // Árboles y hierba que se doblan con el viento. De lejos a cerca, más grandes cuanto más cerca.
        let bend = self.wind * (0.6 + 0.4 * (self.t * 1.7).sin().abs()) + 0.08 * (self.t * 5.0).sin() * self.wind;
        let tree_col = mix(mix((0.16, 0.36, 0.18), pal.green, 0.25), (0.35, 0.22, 0.1), 0.6 * hot);
        let mut trees: Vec<(f64, f64, f64, f64, bool)> = (0..26u64)
            .map(|i| {
                let x = noise::rnd(i as i32, 1, 77) * w;
                let d = noise::rnd(i as i32, 2, 77);
                let yy = hz + 6.0 + d * (h - hz) * 0.9;
                let s = (9.0 + 16.0 * noise::rnd(i as i32, 3, 77)) * (0.45 + 1.1 * d) * sc;
                (x, yy, s, bend * (0.8 + 0.4 * noise::rnd(i as i32, 4, 77)), noise::rnd(i as i32, 8, 77) > 0.55)
            })
            .collect();
        trees.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        for (x, yy, s, b, round) in trees {
            // Sombra en el suelo.
            rgba(cr, (0.0, 0.0, 0.0), 0.18);
            self.blobs.paint(cr, 0, x + s * 0.3, yy + 1.0, s * 0.8, s * 0.22);
            if round {
                Self::bush(cr, &self.blobs, x, yy, s, b, tree_col);
            } else {
                Self::tree(cr, x, yy, s, b, tree_col);
            }
        }
        laps.lap("arboles");
        // Matas de hierba: tres briznas por mata. Horneadas por fase de inclinación (12 fases) y
        // tono de calor; cada fotograma solo se copia la fase más cercana.
        {
            let gp = ((bend.clamp(0.0, 1.2) / 1.2) * 11.0).round() as i32;
            let hk = (hot * 6.0).round() as i32;
            let key = (gp, hk, w as i32, h as i32);
            let mut gc = self.grass.borrow_mut();
            if !gc.iter().any(|(k, _)| *k == key) {
                let gh = h - hz + 2.0;
                let surf = ImageSurface::create(cairo::Format::ARgb32, w.ceil() as i32, gh.ceil() as i32).expect("hierba");
                {
                    let c = &Context::new(&surf).expect("cairo");
                    c.translate(0.0, -(hz - 2.0));
                    let b0 = gp as f64 / 11.0 * 1.2;
                    rgba(c, darken(tree_col, 0.15), 0.65);
                    c.set_line_width(1.0);
                    for i in 0..200u64 {
                        let x = noise::rnd(i as i32, 5, 78) * w;
                        let d = noise::rnd(i as i32, 6, 78);
                        let yy = hz + 4.0 + d * (h - hz - 6.0);
                        let l = (3.0 + 7.0 * noise::rnd(i as i32, 7, 78)) * (0.5 + d) * sc;
                        let b = b0 * (0.9 + 0.5 * (x * 0.05).sin());
                        for k in -1..=1 {
                            c.move_to(x + k as f64 * 1.5, yy);
                            c.line_to(x + k as f64 * 2.5 + b * l, yy - l * (1.0 - 0.2 * (k as f64).abs()));
                        }
                    }
                    c.stroke_q();
                }
                if gc.len() > 24 {
                    gc.remove(0);
                }
                gc.push((key, surf));
            }
            if let Some((_, surf)) = gc.iter().find(|(k, _)| *k == key) {
                let _ = cr.set_source_surface(surf, 0.0, hz - 2.0);
                cr.paint_q();
            }
        }

        laps.lap("hierba");
        // Nubes: cobertura = carga; oscuras y bajas en la tormenta.
        let shown = (self.clouds.len() as f64 * self.cover).ceil() as usize;
        let cloud_c = mix(mix((0.98, 0.98, 1.0), (0.85, 0.75, 0.7), hot), (0.25, 0.12, 0.1), crit);
        for c in self.clouds.iter().take(shown.max(if self.cover > 0.05 { 1 } else { 0 })) {
            let y = c.y * (1.0 + 0.4 * crit);
            let a = 0.9 * (0.6 + 0.4 * c.z);
            // Panza sombreada: la misma mancha, más oscura y un poco más abajo; luego la luz encima.
            for &(dx, dy, r, k) in &c.puffs {
                rgba(cr, darken(cloud_c, 0.3), a * 0.75);
                self.blobs.paint(cr, k, c.x + dx, y + dy + r * 0.28, r * 1.05, r * 0.85);
            }
            for &(dx, dy, r, k) in &c.puffs {
                rgba(cr, cloud_c, a);
                self.blobs.paint(cr, k, c.x + dx, y + dy, r, r * 0.8);
            }
        }
        laps.lap("nubes");
        // Calima y aire vibrando con el calor.
        if hot > 0.0 {
            for i in 0..12 {
                let y = hz - (i as f64 + 0.5) * hz * 0.045;
                let a = 0.06 * hot * (1.0 - i as f64 / 12.0);
                rgba(cr, hzc, a);
                let off = (self.t * 3.0 + i as f64 * 1.3).sin() * 6.0 * hot;
                cr.rectangle(off, y, w, hz * 0.03);
                cr.fill_q();
            }
        }
        // Relámpagos y ceniza.
        for b in &self.bolts {
            for (wd, al) in [(6.0, 0.2), (1.8, 0.95)] {
                rgba(cr, (1.0, 0.95, 0.8), al * (b.life / 0.35));
                cr.set_line_width(wd);
                cr.move_to(b.pts[0].0, b.pts[0].1);
                for p in &b.pts[1..] {
                    cr.line_to(p.0, p.1);
                }
                cr.stroke_q();
            }
            rgba(cr, (1.0, 1.0, 1.0), 0.12 * (b.life / 0.35));
            cr.rectangle(0.0, 0.0, w, h);
            cr.fill_q();
        }
        for a in &self.ash {
            rgba(cr, if a.s > 2.2 { (1.0, 0.5, 0.1) } else { (0.3, 0.25, 0.25) }, 0.8);
            cr.rectangle(a.x, a.y, a.s, a.s);
            cr.fill_q();
        }

        laps.lap("calima");
        // --- Termómetro: escala de 30 a 105 °C con las zonas y los tres sensores.
        let (tx, ty0, ty1) = (w - 60.0 * sc, h * 0.08, h * 0.62);
        let yof = |t: f64| ty1 - (ty1 - ty0) * ((t - T_MIN) / (T_MAX - T_MIN)).clamp(0.0, 1.0);
        // El panel se ensancha hasta cubrir la etiqueta más larga de los sensores.
        let label_w = ["CPU", "GPU", "NVME"].iter().map(|n| cr.text_extents(&format!("{n} 100°")).map(|e| e.width()).unwrap_or(60.0)).fold(0.0f64, f64::max);
        let panel_l = tx - 18.0 - label_w - 10.0;
        rgba(cr, pal.dbg, 0.55);
        cr.rectangle(panel_l, ty0 - 14.0, w - 6.0 - panel_l, ty1 - ty0 + 28.0 + fs);
        cr.fill_q();
        for (a, b, c) in [(T_MIN, WARM, pal.blue), (WARM, HOT, pal.yellow), (HOT, CRIT, pal.orange), (CRIT, T_MAX, pal.red)] {
            rgba(cr, c, 0.35);
            cr.rectangle(tx - 4.0, yof(b), 8.0, yof(a) - yof(b));
            cr.fill_q();
        }
        cr.set_font_size(fs * 0.85);
        for t in (30..=100).step_by(10) {
            let y = yof(t as f64).round() + 0.5;
            rgba(cr, pal.bfg, 0.8);
            cr.move_to(tx + 6.0, y);
            cr.line_to(tx + 12.0, y);
            cr.stroke_q();
            cr.move_to(tx + 15.0, y + fs * 0.3);
            let _ = cr.show_text(&format!("{t}°"));
        }
        cr.set_font_size(fs);
        let mut marks: Vec<(f64, &str, Rgb)> = vec![(self.temp, "CPU", pal.heat(self.temp))];
        if let Some(g) = self.gpu {
            marks.push((g, "GPU", pal.heat(g)));
        }
        if let Some(n) = self.nvme {
            marks.push((n, "NVME", pal.heat(n)));
        }
        // Marcadores en el termómetro; las etiquetas se separan si dos sensores están cerca.
        let mut ordered: Vec<(f64, &str, Rgb)> = marks.clone();
        ordered.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut label_y: Vec<f64> = ordered.iter().map(|(t, _, _)| yof(*t)).collect();
        for i in 1..label_y.len() {
            if label_y[i] - label_y[i - 1] < fs + 4.0 {
                label_y[i] = label_y[i - 1] + fs + 4.0;
            }
        }
        for (i, (t, name, col)) in ordered.iter().enumerate() {
            let y = yof(*t);
            rgba(cr, *col, 1.0);
            cr.move_to(tx - 6.0, y);
            cr.line_to(tx - 14.0, y - 5.0);
            cr.line_to(tx - 14.0, y + 5.0);
            cr.close_path();
            cr.fill_q();
            let txt = format!("{name} {t:.0}°");
            if let Ok(e) = cr.text_extents(&txt) {
                cr.move_to(tx - 18.0 - e.width(), label_y[i] + fs * 0.35);
                let _ = cr.show_text(&txt);
            }
        }
        // Mercurio de la CPU.
        rgba(cr, pal.heat(self.temp), 0.95);
        cr.rectangle(tx - 2.0, yof(self.temp), 4.0, ty1 - yof(self.temp));
        cr.fill_q();

        // Leyenda: qué significa cada cosa de la escena, con el valor real al lado.
        let state = if self.temp >= CRIT {
            tr("wx_crit")
        } else if self.temp >= HOT {
            tr("wx_hot")
        } else if self.temp >= WARM {
            tr("wx_warm")
        } else {
            tr("wx_cool")
        };
        let wind_txt = match (self.fan, self.fan_max) {
            (Some(f), Some(m)) if m > 0 => format!("{f} rpm · {:.0}%", f as f64 / m as f64 * 100.0),
            (Some(f), _) => format!("{f} rpm"),
            _ => "--".into(),
        };
        let lines: Vec<(String, Rgb)> = vec![
            (format!("{} · {:.0}°", state, self.temp), pal.heat(self.temp)),
            (format!("{}  →  CPU {:.0}°", tr("wx_leg_sun"), self.temp), lighten(pal.bfg, 0.1)),
            (format!("{}  →  {} {}", tr("wx_leg_wind"), tr("sens_fan"), wind_txt), lighten(pal.bfg, 0.1)),
            (format!("{}  →  {} {:.2} ({:.0}%)", tr("wx_leg_clouds"), tr("sens_load"), self.load, self.cover_goal * 100.0), lighten(pal.bfg, 0.1)),
            (format!("GPU {}  ·  NVME {}", self.gpu.map_or("--".into(), |v| format!("{v:.0}°")), self.nvme.map_or("--".into(), |v| format!("{v:.0}°"))), pal.fg),
        ];
        let lw = lines.iter().map(|(l, _)| cr.text_extents(l).map(|e| e.width()).unwrap_or(0.0)).fold(0.0f64, f64::max) + 20.0;
        rgba(cr, pal.dbg, 0.62);
        cr.rectangle(10.0, 10.0, lw.max(200.0), (fs + 9.0) * lines.len() as f64 + 14.0);
        cr.fill_q();
        for (i, (l, col)) in lines.iter().enumerate() {
            cr.set_font_size(if i == 0 { fs + 3.0 } else { fs });
            rgba(cr, *col, 1.0);
            cr.move_to(18.0, 10.0 + 8.0 + (fs + 9.0) * (i as f64 + 1.0) - 4.0);
            let _ = cr.show_text(l);
        }
        cr.set_font_size(fs);
        // Historial de temperatura (3 min) abajo a la izquierda, con su título y las bandas.
        if self.hist.len() > 2 {
            let (gx, gy, gw, gh) = (12.0, h - 12.0 - 70.0 * sc, w * 0.32, 70.0 * sc);
            rgba(cr, pal.dbg, 0.62);
            cr.rectangle(gx, gy, gw, gh);
            cr.fill_q();
            for (a, b, c) in [(WARM, HOT, pal.yellow), (HOT, CRIT, pal.orange), (CRIT, T_MAX, pal.red)] {
                let (ya, yb) = (gy + gh - gh * ((a - T_MIN) / (T_MAX - T_MIN)), gy + gh - gh * ((b - T_MIN) / (T_MAX - T_MIN)));
                rgba(cr, c, 0.12);
                cr.rectangle(gx, yb, gw, ya - yb);
                cr.fill_q();
            }
            cr.set_font_size(fs * 0.8);
            rgba(cr, pal.dfg, 1.0);
            cr.move_to(gx + 6.0, gy + fs * 0.8 + 3.0);
            let _ = cr.show_text(&format!("{} · 3 min", tr("sens_cpu")));
            cr.set_font_size(fs);
            let n = 180.0;
            for (i, &t) in self.hist.iter().enumerate() {
                let x = gx + gw * (i as f64 + (n - self.hist.len() as f64)) / (n - 1.0);
                let y = gy + gh - gh * ((t - T_MIN) / (T_MAX - T_MIN)).clamp(0.0, 1.0);
                if i == 0 {
                    cr.move_to(x, y);
                } else {
                    cr.line_to(x, y);
                }
            }
            rgba(cr, pal.heat(self.temp), 1.0);
            cr.set_line_width(1.5);
            cr.stroke_q();
            // Valor actual al final de la curva.
            let y = gy + gh - gh * ((self.temp - T_MIN) / (T_MAX - T_MIN)).clamp(0.0, 1.0);
            cr.set_font_size(fs * 0.9);
            let txt = format!("{:.0}°", self.temp);
            if let Ok(e) = cr.text_extents(&txt) {
                cr.move_to(gx + gw - e.width() - 4.0, (y - 4.0).clamp(gy + fs, gy + gh - 3.0));
                let _ = cr.show_text(&txt);
            }
            cr.set_font_size(fs);
        }
        laps.lap("paneles");
        laps.done(60);
    }
}

pub fn widget(v: Rc<RefCell<Weather>>, pal: Rc<RefCell<Palette>>, font: String) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    let (v2, p2) = (v.clone(), pal.clone());
    let prof = std::env::var_os("TA_PROF").is_some();
    let acc: Rc<RefCell<(f64, u32)>> = Rc::new(RefCell::new((0.0, 0)));
    area.set_draw_func(move |_, cr, w, h| {
        let t0 = std::time::Instant::now();
        let mut x = v2.borrow_mut();
        x.resize(w as f64, h as f64);
        x.draw(cr, &p2.borrow(), &font);
        if prof {
            let mut a = acc.borrow_mut();
            a.0 += t0.elapsed().as_secs_f64() * 1000.0;
            a.1 += 1;
            if a.1 == 60 {
                eprintln!("weather: {:.1} ms/draw", a.0 / 60.0);
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
