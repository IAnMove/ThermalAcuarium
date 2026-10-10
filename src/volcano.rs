//! Pestaña Memoria, skin Arcade: un volcán. La RAM en uso es la actividad del cráter: humo con poca
//! memoria, lava burbujeando a media, fuentes y coladas con mucha, y erupción total con la RAM al
//! máximo. La memoria libre es la nieve de la cumbre (se derrite al llenarse), los programas que más
//! RAM usan son las coladas que bajan por la ladera, y el swap agrieta la llanura.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::cairo::{self, Context, ImageSurface, LinearGradient, RadialGradient};
use gtk::glib;
use gtk::prelude::*;

use crate::draw::{Quiet, lin, rgba};
use crate::i18n::trf;
use crate::reef::noise;
use crate::sim::Rng;
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
    x: f64,
    y: f64,
    r: f64,
    vx: f64,
    vy: f64,
    age: f64,
    max: f64,
}

/// Colada: un programa grande bajando por la ladera.
#[derive(Clone)]
pub struct Flow {
    pub name: String,
    pub mem: f64,
    /// Fracción de la RAM total.
    pub share: f64,
}

pub struct Volcano {
    pub w: f64,
    pub h: f64,
    t: f64,
    /// Uso de RAM (0–1), suavizado; `goal` es la lectura real.
    usage: f64,
    goal: f64,
    free: f64,
    free_goal: f64,
    swap: f64,
    swap_goal: f64,
    pub total: f64,
    pub used: f64,
    pub cached: f64,
    pub swap_used: f64,
    pub swap_total: f64,
    pub flows: Vec<Flow>,
    bombs: Vec<Bomb>,
    puffs: Vec<Puff>,
    bomb_acc: f64,
    puff_acc: f64,
    rng: Rng,
    shake: (f64, f64),
    /// Montaña cacheada (tamaño, paleta) → superficie.
    mountain: RefCell<Option<((i32, i32, u64), ImageSurface)>>,
}

const MAX_BOMBS: usize = 500;

impl Volcano {
    pub fn new() -> Volcano {
        Volcano {
            w: 1000.0,
            h: 600.0,
            t: 0.0,
            usage: 0.3,
            goal: 0.3,
            free: 0.5,
            free_goal: 0.5,
            swap: 0.0,
            swap_goal: 0.0,
            total: 1.0,
            used: 0.0,
            cached: 0.0,
            swap_used: 0.0,
            swap_total: 0.0,
            flows: vec![],
            bombs: vec![],
            puffs: vec![],
            bomb_acc: 0.0,
            puff_acc: 0.0,
            rng: Rng::new(0xB00B5),
            shake: (0.0, 0.0),
            mountain: RefCell::new(None),
        }
    }

    pub fn set_memory(&mut self, used: f64, total: f64, cached: f64, swap_used: f64, swap_total: f64) {
        self.total = total.max(1.0);
        self.used = used;
        self.cached = cached;
        self.swap_used = swap_used;
        self.swap_total = swap_total;
        self.goal = (used / self.total).clamp(0.0, 1.0);
        self.free_goal = (1.0 - self.goal).clamp(0.0, 1.0);
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
        v.truncate(6);
        self.flows = v;
    }

    // --- geometría de la montaña

    fn peak(&self) -> (f64, f64) {
        (self.w * 0.5, self.h * 0.24)
    }

    fn horizon(&self) -> f64 {
        self.h * 0.80
    }

    /// Ancho del cráter.
    fn crater(&self) -> f64 {
        self.w * 0.06 + 14.0
    }

    /// Altura de la ladera en `x` (silueta de la montaña), con rugosidad.
    pub fn slope_y(&self, x: f64) -> f64 {
        let (px, py) = self.peak();
        let c = self.crater();
        let hz = self.horizon();
        let d = (x - px).abs();
        if d <= c {
            return py + (d / c).powi(2) * self.h * 0.012;
        }
        let half = self.w * 0.42;
        let u = ((d - c) / half).clamp(0.0, 1.2);
        // Perfil de estratovolcán: cóncavo (cae rápido y se tiende).
        let base = py + (hz - py) * (1.0 - (1.0 - u.min(1.0)).powf(1.9));
        let rough = (noise::fbm1(x * 0.02, 3, 77) - 0.5) * self.h * 0.03 * u.min(1.0);
        (base + rough).min(hz + self.h * 0.01)
    }

    pub fn resize(&mut self, w: f64, h: f64) {
        if (w - self.w).abs() > 0.5 || (h - self.h).abs() > 0.5 {
            self.w = w;
            self.h = h;
            self.bombs.clear();
            self.puffs.clear();
        }
    }

    pub fn step(&mut self, dt: f64) {
        let dt = dt.clamp(0.0, 0.1);
        self.t += dt;
        let k = |r: f64| 1.0 - (-dt * r).exp();
        self.usage += (self.goal - self.usage) * k(0.8);
        self.free += (self.free_goal - self.free) * k(0.8);
        self.swap += (self.swap_goal - self.swap) * k(0.8);
        let u = self.usage;
        let (px, py) = self.peak();
        let act = ((u - 0.40) / 0.60).clamp(0.0, 1.0); // actividad de lava
        let erupt = ((u - 0.88) / 0.12).clamp(0.0, 1.0); // erupción total

        // Bombas de lava.
        self.bomb_acc += (4.0 + 260.0 * act.powf(1.5) + 700.0 * erupt) * dt;
        while self.bomb_acc >= 1.0 {
            self.bomb_acc -= 1.0;
            if self.bombs.len() >= MAX_BOMBS {
                break;
            }
            let sp = self.h * (0.45 + 0.7 * act + 0.6 * erupt) * self.rng.range(0.6, 1.0);
            let ang = -PI / 2.0 + self.rng.gauss() * (0.12 + 0.18 * erupt);
            self.bombs.push(Bomb {
                x: px + self.rng.gauss() * self.crater() * 0.3,
                y: py,
                vx: ang.cos() * sp,
                vy: ang.sin() * sp,
                r: (1.8 + 3.5 * self.rng.next() + 4.0 * erupt * self.rng.next()) * (self.w / 1000.0).max(0.6),
                life: 1.0,
            });
        }
        let g = self.h * 0.9;
        let mut bombs = std::mem::take(&mut self.bombs);
        for b in &mut bombs {
            b.vy += g * dt;
            b.x += b.vx * dt;
            b.y += b.vy * dt;
            b.life -= dt * 0.35;
            if b.y > self.slope_y(b.x) && b.vy > 0.0 {
                b.life -= dt * 4.0; // se apaga al caer en la ladera
                b.vx *= 0.5;
                b.vy = 0.0;
                b.y = self.slope_y(b.x);
            }
        }
        let hh = self.h;
        bombs.retain(|b| b.life > 0.0 && b.y < hh + 20.0);
        self.bombs = bombs;

        // Humo y ceniza.
        self.puff_acc += (1.5 + 6.0 * u + 25.0 * erupt) * dt;
        while self.puff_acc >= 1.0 {
            self.puff_acc -= 1.0;
            if self.puffs.len() > 160 {
                break;
            }
            let max = self.rng.range(4.0, 8.0);
            self.puffs.push(Puff {
                x: px + self.rng.gauss() * self.crater() * 0.4,
                y: py - 4.0,
                r: self.crater() * (0.25 + 0.3 * self.rng.next()),
                vx: self.rng.gauss() * 6.0 + 10.0,
                vy: -(18.0 + 30.0 * u + 60.0 * erupt) * self.rng.range(0.7, 1.3),
                age: 0.0,
                max,
            });
        }
        for p in &mut self.puffs {
            p.age += dt;
            p.x += (p.vx + (self.t * 0.7 + p.y * 0.01).sin() * 8.0) * dt;
            p.y += p.vy * dt;
            p.vy *= 1.0 - 0.25 * dt;
            p.r += (8.0 + 14.0 * u) * dt;
        }
        self.puffs.retain(|p| p.age < p.max);

        // Temblor con la erupción.
        self.shake = if erupt > 0.0 {
            let a = 3.0 * erupt * (self.w / 1000.0).max(0.5);
            ((self.t * 37.0).sin() * a, (self.t * 29.0).cos() * a * 0.6)
        } else {
            (0.0, 0.0)
        };
    }

    // --- dibujo

    fn lava(pal: &Palette, k: f64) -> Rgb {
        // 0 = incandescente, 1 = enfriándose. La lava es lava (amarillo → naranja → rojo oscuro);
        // el tema solo la tiñe un poco.
        let (y, o, r) = (mix((1.0, 0.93, 0.55), pal.yellow, 0.25), mix((1.0, 0.45, 0.08), pal.orange, 0.3), mix((0.45, 0.06, 0.02), pal.red, 0.3));
        if k < 0.5 { mix(y, o, k * 2.0) } else { mix(o, r, (k - 0.5) * 2.0) }
    }

    /// Montaña y llanura (estáticas): roca, estratos y nieve de la cumbre se pintan aparte.
    fn mountain_surface(&self, pal: &Palette) -> ImageSurface {
        let (w, h) = (self.w, self.h);
        let surf = ImageSurface::create(cairo::Format::ARgb32, w.ceil() as i32, h.ceil() as i32).expect("mountain");
        let cr = &Context::new(&surf).expect("cairo");
        let hz = self.horizon();
        let rock = mix((0.30, 0.27, 0.26), mix(pal.dfg, pal.dbg, 0.5), 0.35);
        // Llanura.
        let _ = cr.set_source(lin(0.0, hz, 0.0, h, &[(0.0, mix(rock, pal.bg, 0.3), 1.0), (1.0, darken(pal.dbg, 0.3), 1.0)]));
        cr.rectangle(0.0, hz, w, h - hz);
        cr.fill_q();
        // Montaña.
        cr.move_to(0.0, hz + 2.0);
        let mut x = 0.0;
        while x <= w + 4.0 {
            cr.line_to(x, self.slope_y(x));
            x += 4.0;
        }
        cr.line_to(w, hz + 2.0);
        cr.close_path();
        let (px, py) = self.peak();
        let _ = cr.set_source(lin(0.0, py, 0.0, hz, &[(0.0, lighten(rock, 0.18), 1.0), (0.55, rock, 1.0), (1.0, darken(rock, 0.45), 1.0)]));
        cr.fill_keep();
        cr.save_q();
        cr.clip();
        // Luz desde la izquierda y sombra a la derecha.
        let _ = cr.set_source(lin(px - w * 0.4, 0.0, px + w * 0.4, 0.0, &[(0.0, (1.0, 1.0, 1.0), 0.10), (0.5, (0.0, 0.0, 0.0), 0.0), (1.0, (0.0, 0.0, 0.0), 0.30)]));
        cr.paint_q();
        // Textura de roca.
        let tile = noise::grey_tile(256, 24, 3, 7, 0.9);
        let pat = cairo::SurfacePattern::create(&tile);
        pat.set_extend(cairo::Extend::Repeat);
        let _ = cr.set_source(&pat);
        cr.set_operator(cairo::Operator::SoftLight);
        let _ = cr.paint_with_alpha(0.6);
        cr.set_operator(cairo::Operator::Over);
        // Estratos y barrancos.
        for i in 0..18u64 {
            let y0 = py + (hz - py) * (0.15 + 0.85 * (i as f64 / 18.0));
            rgba(cr, darken(rock, 0.6), 0.10 + 0.12 * noise::rnd(i as i32, 3, 9));
            cr.set_line_width(1.0 + 2.0 * noise::rnd(i as i32, 5, 9));
            cr.move_to(0.0, y0);
            let mut x = 0.0;
            while x <= w {
                cr.line_to(x, y0 + (noise::fbm1(x * 0.015 + i as f64 * 7.0, 2, 5) - 0.5) * 18.0);
                x += 12.0;
            }
            cr.stroke_q();
        }
        for i in 0..14u64 {
            let side = if i % 2 == 0 { -1.0 } else { 1.0 };
            let x0 = px + side * (self.crater() + noise::rnd(i as i32, 11, 2) * w * 0.05);
            let x1 = px + side * (w * 0.12 + noise::rnd(i as i32, 13, 2) * w * 0.33);
            rgba(cr, darken(rock, 0.7), 0.35);
            cr.set_line_width(1.5);
            cr.move_to(x0, self.slope_y(x0) + 2.0);
            let n = 10;
            for k in 1..=n {
                let x = x0 + (x1 - x0) * k as f64 / n as f64;
                cr.line_to(x + (noise::rnd(k, i as i32, 4) - 0.5) * 8.0, self.slope_y(x) + 2.0);
            }
            cr.stroke_q();
        }
        cr.restore_q();
        surf
    }

    pub fn draw(&self, cr: &Context, pal: &Palette, font: &str) {
        let (w, h) = (self.w, self.h);
        let u = self.usage;
        let act = ((u - 0.40) / 0.60).clamp(0.0, 1.0);
        let erupt = ((u - 0.88) / 0.12).clamp(0.0, 1.0);
        let (px, py) = self.peak();
        let hz = self.horizon();
        let c = self.crater();
        cr.save_q();
        cr.translate(self.shake.0, self.shake.1);

        // Cielo: noche del tema, enrojecido por la lava.
        let glow = Self::lava(pal, 0.4);
        let sky_top = darken(mix(pal.dbg, (0.02, 0.02, 0.05), 0.5), 0.3);
        let sky_hz = mix(mix(pal.bg, pal.dbg, 0.3), glow, 0.12 + 0.45 * act + 0.3 * erupt);
        let _ = cr.set_source(lin(0.0, 0.0, 0.0, hz, &[(0.0, sky_top, 1.0), (1.0, sky_hz, 1.0)]));
        cr.rectangle(-10.0, -10.0, w + 20.0, h + 20.0);
        cr.fill_q();
        for i in 0..90u64 {
            let (x, y) = (noise::rnd(i as i32, 1, 1) * w, noise::rnd(i as i32, 2, 1) * hz * 0.7);
            let tw = 0.5 + 0.5 * (self.t * 1.5 + i as f64).sin();
            rgba(cr, pal.bfg, (0.15 + 0.5 * noise::rnd(i as i32, 3, 1)) * tw * (1.0 - 0.7 * act));
            cr.rectangle(x.round(), y.round(), 1.5, 1.5);
            cr.fill_q();
        }
        // Resplandor del cráter sobre el cielo y la ladera.
        let g = RadialGradient::new(px, py, 0.0, px, py, w * (0.18 + 0.35 * act + 0.3 * erupt));
        g.add_color_stop_rgba(0.0, glow.0, glow.1, glow.2, 0.25 + 0.5 * act);
        g.add_color_stop_rgba(1.0, glow.0, glow.1, glow.2, 0.0);
        let _ = cr.set_source(&g);
        cr.rectangle(-10.0, -10.0, w + 20.0, h + 20.0);
        cr.fill_q();

        // Montaña (cacheada).
        let key = (w as i32, h as i32, crate::sim::hash_str(&pal.raw));
        {
            let mut m = self.mountain.borrow_mut();
            if m.as_ref().map(|(k, _)| *k) != Some(key) {
                *m = Some((key, self.mountain_surface(pal)));
            }
            if let Some((_, s)) = m.as_ref() {
                let _ = cr.set_source_surface(s, 0.0, 0.0);
                cr.paint_q();
            }
        }
        // Luz de la lava sobre la roca.
        let g = RadialGradient::new(px, py, 0.0, px, py, w * (0.12 + 0.25 * act));
        g.add_color_stop_rgba(0.0, glow.0, glow.1, glow.2, 0.35 * act + 0.3 * erupt);
        g.add_color_stop_rgba(1.0, glow.0, glow.1, glow.2, 0.0);
        let _ = cr.set_source(&g);
        cr.rectangle(0.0, py - 10.0, w, hz - py + 10.0);
        cr.fill_q();

        // Nieve de la cumbre: la memoria libre. Se derrite conforme sube el uso.
        let snow_h = (hz - py) * 0.42 * self.free.powf(1.3);
        if snow_h > 2.0 {
            let ys = py + snow_h;
            cr.move_to(px - c - 1.0, self.slope_y(px - c - 1.0));
            let mut x = px - c;
            // Lado izquierdo hacia abajo siguiendo la ladera hasta la cota de nieve.
            while self.slope_y(x) < ys && x > 0.0 {
                cr.line_to(x, self.slope_y(x));
                x -= 4.0;
            }
            let xl = x;
            let mut xx = xl;
            while xx <= 2.0 * px - xl {
                let jag = (noise::fbm1(xx * 0.05, 2, 21) - 0.5) * snow_h * 0.35;
                cr.line_to(xx, (ys + jag).max(self.slope_y(xx)));
                xx += 6.0;
            }
            let mut x = 2.0 * px - xl;
            while x > px + c {
                cr.line_to(x, self.slope_y(x));
                x -= 4.0;
            }
            cr.close_path();
            let snow = mix((0.96, 0.97, 1.0), pal.bfg, 0.2);
            let _ = cr.set_source(lin(0.0, py, 0.0, ys, &[(0.0, snow, 0.95), (1.0, mix(snow, pal.dbg, 0.35), 0.9)]));
            cr.fill_q();
        }

        // Coladas: los programas que más RAM usan.
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Round);
        cr.select_font_face(font, cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        let fs = (11.0 * (w / 1100.0)).clamp(10.0, 14.0);
        cr.set_font_size(fs);
        for (i, f) in self.flows.iter().enumerate() {
            let side = if i % 2 == 0 { 1.0 } else { -1.0 };
            let len = (0.18 + 3.2 * f.share).clamp(0.18, 1.0) * (0.35 + 0.65 * act);
            let wd = (2.0 + 26.0 * f.share.sqrt()) * (w / 1000.0).max(0.6);
            let x0 = px + side * (c * 0.6 + (i / 2) as f64 * c * 0.35);
            let x1 = px + side * (c + (w * 0.44 - c) * len);
            let n = 24;
            let pts: Vec<(f64, f64)> = (0..=n)
                .map(|k| {
                    let x = x0 + (x1 - x0) * k as f64 / n as f64;
                    let wig = (noise::fbm1(x * 0.02 + i as f64 * 9.0, 2, 33 + i as u32) - 0.5) * 14.0 * (k as f64 / n as f64);
                    (x, self.slope_y(x + wig) + 1.0)
                })
                .collect();
            let path = |cr: &Context| {
                cr.move_to(pts[0].0, pts[0].1);
                for p in &pts[1..] {
                    cr.line_to(p.0, p.1);
                }
            };
            let grad = LinearGradient::new(x0, 0.0, x1, 0.0);
            let (c0, c1, c2) = (Self::lava(pal, 0.0), Self::lava(pal, 0.5), Self::lava(pal, 1.0));
            grad.add_color_stop_rgba(0.0, c0.0, c0.1, c0.2, 1.0);
            grad.add_color_stop_rgba(0.5, c1.0, c1.1, c1.2, 1.0);
            grad.add_color_stop_rgba(1.0, c2.0, c2.1, c2.2, 0.9);
            path(cr);
            rgba(cr, c1, 0.25);
            cr.set_line_width(wd * 2.2);
            cr.stroke_q();
            path(cr);
            let _ = cr.set_source(&grad);
            cr.set_line_width(wd);
            cr.stroke_q();
            // Costra oscura en el centro de las coladas anchas.
            if wd > 8.0 {
                path(cr);
                rgba(cr, darken(pal.red, 0.6), 0.35);
                cr.set_line_width(wd * 0.35);
                cr.stroke_q();
            }
            // Etiqueta al final de la colada.
            let txt = format!("{} · {}", f.name, fmt_bytes(f.mem));
            if let Ok(e) = cr.text_extents(&txt) {
                let (lx, ly) = (pts[n].0 + side * 8.0, pts[n].1 - 6.0 + if (i / 2) % 2 == 1 { fs + 8.0 } else { 0.0 });
                let x = if side > 0.0 { lx } else { lx - e.width() };
                rgba(cr, pal.dbg, 0.6);
                cr.rectangle(x - 4.0, ly - fs - 1.0, e.width() + 8.0, fs + 6.0);
                cr.fill_q();
                rgba(cr, lighten(pal.bfg, 0.1), 1.0);
                cr.move_to(x, ly);
                let _ = cr.show_text(&txt);
            }
        }

        // Boca del cráter: lago de lava que burbujea con la actividad.
        cr.save_q();
        cr.translate(px, py + 2.0);
        cr.scale(c, c * 0.28);
        let g = RadialGradient::new(0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
        let (lc0, lc1) = (Self::lava(pal, 0.05), Self::lava(pal, 0.8));
        let a = 0.35 + 0.65 * act;
        g.add_color_stop_rgba(0.0, lc0.0, lc0.1, lc0.2, a);
        g.add_color_stop_rgba(0.7, lc1.0, lc1.1, lc1.2, a);
        g.add_color_stop_rgba(1.0, darken(pal.dbg, 0.5).0, darken(pal.dbg, 0.5).1, darken(pal.dbg, 0.5).2, 1.0);
        let _ = cr.set_source(&g);
        cr.arc(0.0, 0.0, 1.0, 0.0, 2.0 * PI);
        cr.fill_q();
        cr.restore_q();
        for i in 0..6 {
            let ph = self.t * (1.5 + i as f64 * 0.3) + i as f64;
            let k = ph.rem_euclid(2.0);
            if k < 1.0 {
                let r = c * 0.12 * k * (0.5 + act);
                rgba(cr, Self::lava(pal, 0.0), (1.0 - k) * a);
                cr.arc(px + (i as f64 - 2.5) * c * 0.3, py + 2.0 - k * 2.0, r, 0.0, 2.0 * PI);
                cr.fill_q();
            }
        }

        // Bombas de lava.
        cr.set_line_cap(cairo::LineCap::Round);
        for b in &self.bombs {
            let col = Self::lava(pal, 1.0 - b.life);
            rgba(cr, col, 0.30 * b.life);
            cr.arc(b.x, b.y, b.r * 2.6, 0.0, 2.0 * PI);
            cr.fill_q();
            // Estela.
            rgba(cr, col, 0.45 * b.life);
            cr.set_line_width(b.r * 1.2);
            cr.move_to(b.x, b.y);
            cr.line_to(b.x - b.vx * 0.06, b.y - b.vy * 0.06);
            cr.stroke_q();
            rgba(cr, col, b.life.clamp(0.0, 1.0));
            cr.arc(b.x, b.y, b.r, 0.0, 2.0 * PI);
            cr.fill_q();
        }
        // Humo y ceniza.
        let smoke = mix((0.12, 0.11, 0.12), pal.dfg, 0.2);
        for p in &self.puffs {
            let k = p.age / p.max;
            let col = mix(Self::lava(pal, 0.6), smoke, (k * 3.0).clamp(0.0, 1.0));
            rgba(cr, col, (0.45 * (1.0 - k)) * (0.6 + 0.4 * u));
            cr.arc(p.x, p.y, p.r, 0.0, 2.0 * PI);
            cr.fill_q();
        }

        // Grietas del swap en la llanura.
        let ncr = (self.swap * 14.0).round() as usize;
        if ncr > 0 {
            for i in 0..ncr {
                let sd = i as u32 * 31 + 7;
                let x0 = noise::rnd(i as i32, 1, sd) * w;
                let y0 = hz + 6.0 + noise::rnd(i as i32, 2, sd) * (h - hz - 12.0);
                let len = w * (0.04 + 0.12 * noise::rnd(i as i32, 3, sd));
                let c0 = Self::lava(pal, 0.2);
                for (wd, a) in [(9.0, 0.22 * self.swap + 0.1), (2.2, 0.95)] {
                    rgba(cr, c0, a);
                    cr.set_line_width(wd);
                    cr.move_to(x0, y0);
                    for k in 1..=6 {
                        cr.line_to(x0 + len * k as f64 / 6.0, y0 + (noise::rnd(k, i as i32, sd) - 0.5) * 10.0);
                    }
                    cr.stroke_q();
                }
            }
        }

        // Cifras.
        cr.set_font_size(fs + 2.0);
        let pct = u * 100.0;
        let line1 = trf("ram_of", &[format!("{pct:.0}"), fmt_bytes(self.total)]);
        let line2 = format!("{} · {}", fmt_bytes(self.used), trf("mem_cached", &[fmt_bytes(self.cached)]));
        let line3 = if self.swap_total > 0.0 { trf("mem_swap", &[fmt_bytes(self.swap_used), fmt_bytes(self.swap_total)]) } else { String::new() };
        for (i, l) in [format!("RAM {}", fmt_bytes(self.used)), line1, line2, line3].iter().enumerate() {
            if l.is_empty() {
                continue;
            }
            rgba(cr, pal.dbg, 0.55);
            if let Ok(e) = cr.text_extents(l) {
                cr.rectangle(10.0, 10.0 + (fs + 8.0) * i as f64, e.width() + 12.0, fs + 8.0);
                cr.fill_q();
            }
            rgba(cr, if i == 0 { lighten(pal.bfg, 0.1) } else { pal.fg }, 1.0);
            cr.move_to(16.0, 10.0 + (fs + 8.0) * (i as f64 + 1.0) - 5.0);
            let _ = cr.show_text(l);
        }
        cr.restore_q();
    }
}

/// Widget: un DrawingArea con su reloj (solo corre mientras está a la vista).
pub fn widget(v: Rc<RefCell<Volcano>>, pal: Rc<RefCell<Palette>>, font: String) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    let (v2, p2) = (v.clone(), pal.clone());
    area.set_draw_func(move |_, cr, w, h| {
        let mut vol = v2.borrow_mut();
        vol.resize(w as f64, h as f64);
        vol.draw(cr, &p2.borrow(), &font);
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
    let _: Option<VecDeque<f64>> = None;
    area
}
