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
use crate::i18n::{tr, trf};
use crate::reef::noise;
use crate::sim::Rng;
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
    puffs: Vec<(f64, f64, f64)>,
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
}

impl Weather {
    pub fn new(ncpu: f64) -> Weather {
        let mut rng = Rng::new(0xC10D);
        let clouds = (0..9)
            .map(|i| {
                let z = 0.4 + 0.6 * rng.next();
                let w = (70.0 + 120.0 * rng.next()) * z;
                let puffs = (0..6).map(|k| (((k as f64 / 5.0) - 0.5) * w * 0.8, rng.gauss() * w * 0.08, w * (0.22 + 0.16 * rng.next()))).collect();
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
            rgba(cr, c, 1.0);
            cr.fill_q();
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

        // Árboles y hierba que se doblan con el viento.
        let bend = self.wind * (0.6 + 0.4 * (self.t * 1.7).sin().abs()) + 0.08 * (self.t * 5.0).sin() * self.wind;
        let tree_col = mix(mix((0.16, 0.36, 0.18), pal.green, 0.25), (0.35, 0.22, 0.1), 0.6 * hot);
        for i in 0..18u64 {
            let x = noise::rnd(i as i32, 1, 77) * w;
            let yy = hz + 6.0 + noise::rnd(i as i32, 2, 77) * (h - hz) * 0.9;
            let s = (10.0 + 18.0 * noise::rnd(i as i32, 3, 77)) * (0.5 + (yy - hz) / (h - hz)) * sc;
            Self::tree(cr, x, yy, s, bend * (0.8 + 0.4 * noise::rnd(i as i32, 4, 77)), tree_col);
        }
        rgba(cr, darken(tree_col, 0.2), 0.6);
        cr.set_line_width(1.0);
        for i in 0..160u64 {
            let x = noise::rnd(i as i32, 5, 78) * w;
            let yy = hz + 4.0 + noise::rnd(i as i32, 6, 78) * (h - hz - 6.0);
            let l = (4.0 + 6.0 * noise::rnd(i as i32, 7, 78)) * sc;
            let b = bend * (0.9 + 0.5 * (self.t * 3.0 + x * 0.05).sin());
            cr.move_to(x, yy);
            cr.line_to(x + b * l, yy - l);
        }
        cr.stroke_q();

        // Nubes: cobertura = carga; oscuras y bajas en la tormenta.
        let shown = (self.clouds.len() as f64 * self.cover).ceil() as usize;
        let cloud_c = mix(mix((0.98, 0.98, 1.0), (0.85, 0.75, 0.7), hot), (0.25, 0.12, 0.1), crit);
        for c in self.clouds.iter().take(shown.max(if self.cover > 0.05 { 1 } else { 0 })) {
            let y = c.y * (1.0 + 0.4 * crit);
            for &(dx, dy, r) in &c.puffs {
                let col = mix(darken(cloud_c, 0.25), cloud_c, 0.5 + 0.5 * (-dy / r.max(1.0)).clamp(-1.0, 1.0));
                rgba(cr, col, 0.85 * (0.6 + 0.4 * c.z));
                cr.arc(c.x + dx, y + dy, r, 0.0, 2.0 * PI);
                cr.fill_q();
            }
        }
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

        // --- Termómetro: escala de 30 a 105 °C con las zonas y los tres sensores.
        let (tx, ty0, ty1) = (w - 60.0 * sc, h * 0.08, h * 0.62);
        let yof = |t: f64| ty1 - (ty1 - ty0) * ((t - T_MIN) / (T_MAX - T_MIN)).clamp(0.0, 1.0);
        rgba(cr, pal.dbg, 0.55);
        cr.rectangle(tx - 46.0 * sc, ty0 - 14.0, 100.0 * sc, ty1 - ty0 + 28.0 + fs);
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
        for (i, (t, name, col)) in marks.iter().enumerate() {
            let y = yof(*t);
            rgba(cr, *col, 1.0);
            cr.move_to(tx - 6.0, y);
            cr.line_to(tx - 14.0, y - 5.0);
            cr.line_to(tx - 14.0, y + 5.0);
            cr.close_path();
            cr.fill_q();
            let txt = format!("{name} {t:.0}°");
            if let Ok(e) = cr.text_extents(&txt) {
                let yy = y + fs * 0.35 + if i > 0 && (marks[0].0 - t).abs() < 6.0 { fs * i as f64 } else { 0.0 };
                cr.move_to(tx - 18.0 - e.width(), yy);
                let _ = cr.show_text(&txt);
            }
        }
        // Mercurio de la CPU.
        rgba(cr, pal.heat(self.temp), 0.95);
        cr.rectangle(tx - 2.0, yof(self.temp), 4.0, ty1 - yof(self.temp));
        cr.fill_q();

        // Cifras: estado del tiempo, viento, nubes.
        let state = if self.temp >= CRIT {
            tr("wx_crit")
        } else if self.temp >= HOT {
            tr("wx_hot")
        } else if self.temp >= WARM {
            tr("wx_warm")
        } else {
            tr("wx_cool")
        };
        cr.set_font_size(fs + 3.0);
        let lines = [
            format!("{} · {:.0}°", state, self.temp),
            self.fan.map_or(trf("wx_wind", &["--".into()]), |f| trf("wx_wind", &[f.to_string()])),
            trf("wx_clouds", &[format!("{:.2}", self.load)]),
        ];
        for (i, l) in lines.iter().enumerate() {
            if i == 1 {
                cr.set_font_size(fs);
            }
            if let Ok(e) = cr.text_extents(l) {
                rgba(cr, pal.dbg, 0.6);
                cr.rectangle(10.0, 10.0 + (fs + 10.0) * i as f64, e.width() + 12.0, fs + 10.0);
                cr.fill_q();
            }
            rgba(cr, if i == 0 { pal.heat(self.temp) } else { lighten(pal.bfg, 0.1) }, 1.0);
            cr.move_to(16.0, 10.0 + (fs + 10.0) * (i as f64 + 1.0) - 6.0);
            let _ = cr.show_text(l);
        }
        // Historial de temperatura abajo a la izquierda.
        if self.hist.len() > 2 {
            let (gx, gy, gw, gh) = (12.0, h - 12.0 - 60.0 * sc, w * 0.32, 60.0 * sc);
            rgba(cr, pal.dbg, 0.55);
            cr.rectangle(gx, gy, gw, gh);
            cr.fill_q();
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
        }
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
