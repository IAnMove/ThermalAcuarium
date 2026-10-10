//! Escena del estilo realista: un acuario de arrecife iluminado desde arriba. Las capas estáticas
//! (arrecife lejano, rayos, roca viva, arena, cristal) se pintan una vez con Cairo y se reutilizan;
//! lo que se mueve (cáusticas, burbujas, peces) se compone encima. Lo comparten el renderizador
//! Cairo de referencia (`--snapshot`) y el de GPU (`gpu.rs`).

use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::cairo::{self, Context, ImageSurface, RadialGradient};

use super::noise;
use crate::draw::{Quiet, View, closed_smooth, lin, rgba};
use crate::sim::{BubbleKind, Life, Pick, Tank, hash_str, unit};
use crate::theme::{Palette, Rgb, darken, lighten, luma, mix, tone};

// ---------------------------------------------------------------- colores del agua

/// Colores del agua derivados del tema activo de Omarchy: el matiz sale de su azul/cian, la
/// luminosidad de si el tema es oscuro (acuario de noche) o claro (de día).
#[derive(Clone, Copy)]
pub struct Water {
    pub top: Rgb,
    pub mid: Rgb,
    pub bot: Rgb,
    /// Tono oscuro para viñeta, bordes del cristal y siluetas del fondo.
    pub abyss: Rgb,
    /// Aire por encima de la superficie.
    pub air: Rgb,
    pub ray: Rgb,
    pub far1: Rgb,
    pub far2: Rgb,
    pub dark: bool,
}

/// Realza la saturación: los temas apagados (nord, gruvbox…) dan un agua demasiado gris si no.
fn boost(c: Rgb, k: f64) -> Rgb {
    let l = luma(c);
    ((l + (c.0 - l) * k).clamp(0.0, 1.0), (l + (c.1 - l) * k).clamp(0.0, 1.0), (l + (c.2 - l) * k).clamp(0.0, 1.0))
}

impl Water {
    pub fn from(p: &Palette) -> Water {
        let natural = (0.10, 0.50, 0.62);
        let hue = mix(natural, boost(mix(p.blue, p.cyan, 0.5), 1.3), 0.75);
        let dark = p.dark;
        let (lt, lm, lb) = if dark { (0.44, 0.22, 0.05) } else { (0.66, 0.48, 0.28) };
        Water {
            top: tone(hue, lt),
            mid: tone(hue, lm),
            bot: tone(mix(hue, p.dbg, 0.35), lb),
            abyss: tone(hue, if dark { 0.02 } else { 0.12 }),
            air: if dark { darken(p.dbg, 0.45) } else { p.bg },
            ray: lighten(tone(hue, lt), 0.7),
            far1: tone(hue, if dark { 0.10 } else { 0.40 }),
            far2: tone(hue, if dark { 0.045 } else { 0.30 }),
            dark,
        }
    }

    /// Degradado del agua de arriba abajo; con calor se vuelve ámbar, sobre todo en el fondo.
    pub fn stops(&self, boil: f64) -> [(f64, Rgb); 3] {
        [
            (0.0, mix(self.top, (0.55, 0.46, 0.30), 0.18 * boil)),
            (0.38, mix(self.mid, (0.36, 0.20, 0.12), 0.30 * boil)),
            (1.0, mix(self.bot, (0.36, 0.08, 0.03), 0.60 * boil)),
        ]
    }
}

// ---------------------------------------------------------------- capas

/// Imagen que cubre el rectángulo lógico (x, y, w, h) de la pecera.
pub struct Layer {
    pub surf: ImageSurface,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

pub struct Layers {
    pub far: Layer,
    pub rays: Layer,
    pub rocks: Layer,
    pub sand: Layer,
    pub glass: Layer,
    /// Baldosa de cáusticas (repetible).
    pub caustic: ImageSurface,
}

/// Pinta una capa a resolución `ds * q` (las capas difusas usan q < 1: se ven más suaves y pesan menos).
fn layer(x: f64, y: f64, w: f64, h: f64, ds: f64, q: f64, paint: impl FnOnce(&Context)) -> Layer {
    let k = ds * q;
    let surf = ImageSurface::create(cairo::Format::ARgb32, (w * k).ceil().max(1.0) as i32, (h * k).ceil().max(1.0) as i32).expect("layer");
    surf.set_device_scale(k, k);
    {
        let cr = Context::new(&surf).expect("cairo");
        cr.translate(-x, -y);
        paint(&cr);
    }
    Layer { surf, x, y, w, h }
}

pub fn bake(t: &Tank, ds: f64, wt: &Water) -> Layers {
    let (w, h) = (t.w, t.h);
    let grain = noise::grey_tile(256, 40, 2, 99, 1.6);
    let rough = noise::grey_tile(256, 8, 4, 7, 1.8);
    Layers {
        far: layer(0.0, 0.0, w, h, ds, 0.18, |cr| paint_far(cr, w, h, wt)),
        rays: layer(0.0, 0.0, w, h, ds, 0.25, |cr| paint_rays(cr, w, h, wt)),
        rocks: layer(0.0, 0.0, w, h, ds, 1.0, |cr| paint_rocks(cr, t, &rough, wt)),
        sand: {
            let y0 = (h * 0.88).floor();
            layer(0.0, y0, w, h - y0, ds, 1.0, |cr| paint_sand(cr, t, &grain, wt))
        },
        glass: layer(0.0, 0.0, w, h, ds, 0.5, |cr| paint_glass(cr, w, h, wt)),
        caustic: noise::caustic_tile(256, 31),
    }
}

/// Reflejo de la pared del fondo del acuario: siluetas de arrecife desenfocadas (la capa se pinta
/// a baja resolución, el escalado hace de desenfoque).
fn paint_far(cr: &Context, w: f64, h: f64, wt: &Water) {
    for (base, amp, col, a, seed) in [(0.62, 0.26, wt.far1, 0.50, 11u32), (0.80, 0.34, wt.far2, 0.66, 23)] {
        cr.move_to(0.0, h);
        let mut x = 0.0;
        while x <= w + 4.0 {
            let n = ((noise::fbm1(x / w * 4.0 + seed as f64, 4, seed) - 0.5) * 2.8 + 0.5).clamp(0.0, 1.0);
            // Pináculos: columnas de coral que sobresalen.
            let mut peak = 0.0;
            for k in 0..4 {
                let c = unit(seed as u64 * 17 + k) * w;
                peak += (-((x - c) / (w * 0.035)).powi(2)).exp() * (0.5 + 0.5 * unit(seed as u64 * 31 + k));
            }
            let y = h * base - h * amp * (n.powf(1.4) * 1.3 + 0.55 * peak);
            cr.line_to(x, y);
            x += 5.0;
        }
        cr.line_to(w, h);
        cr.close_path();
        let _ = cr.set_source(lin(0.0, h * (base - amp), 0.0, h, &[(0.0, col, a), (1.0, darken(col, 0.6), a)]));
        cr.fill_q();
    }
}

/// Haces de luz que bajan desde la superficie.
fn paint_rays(cr: &Context, w: f64, h: f64, wt: &Water) {
    let col = wt.ray;
    for i in 0..9 {
        let x0 = w * (0.02 + 0.115 * i as f64) + w * 0.03 * (i as f64 * 2.1).sin();
        let bw = w * (0.012 + 0.018 * unit(i as u64 + 5));
        let slant = w * 0.14;
        let end = h * 0.97;
        let a = 0.10 + 0.12 * unit(i as u64 + 50);
        let _ = cr.set_source(lin(0.0, 0.0, 0.0, end, &[(0.0, col, a), (0.55, col, a * 0.35), (1.0, col, 0.0)]));
        cr.move_to(x0 - bw, 0.0);
        cr.line_to(x0 + bw, 0.0);
        cr.line_to(x0 + bw * 3.2 + slant, end);
        cr.line_to(x0 - bw * 0.6 + slant, end);
        cr.close_path();
        cr.fill_q();
    }
}

/// Roca viva: pilas de rocas con algas coralinas, grietas y luz desde arriba.
fn paint_rocks(cr: &Context, t: &Tank, rough: &ImageSurface, wt: &Water) {
    let (w, h) = (t.w, t.h);
    // (centro x, ancho, alto) relativos; las pilas de los lados enmarcan la escena.
    let spec: [(f64, f64, f64); 9] = [
        (0.045, 0.16, 0.30),
        (0.15, 0.13, 0.19),
        (0.01, 0.10, 0.20),
        (0.10, 0.09, 0.34),
        (0.93, 0.17, 0.27),
        (0.83, 0.12, 0.17),
        (0.99, 0.10, 0.34),
        (0.46, 0.15, 0.10),
        (0.58, 0.11, 0.075),
    ];
    for (i, &(cx, rw, rh)) in spec.iter().enumerate() {
        let x = cx * w;
        let base = t.floor_y(x) + 4.0;
        rock(cr, x, base, rw * w, rh * h, i as u64 * 131 + 7, rough);
    }
    // Todo está bajo el agua: tiñe de azul lo ya pintado (solo donde hay roca).
    cr.set_operator(cairo::Operator::Atop);
    let _ = cr.set_source(lin(0.0, 0.0, 0.0, h, &[(0.0, wt.top, 0.20), (1.0, wt.bot, 0.50)]));
    cr.rectangle(0.0, 0.0, w, h);
    cr.fill_q();
    cr.set_operator(cairo::Operator::Over);
}

pub(crate) fn rock(cr: &Context, cx: f64, base: f64, w: f64, h: f64, seed: u64, rough: &ImageSurface) {
    let n = 16;
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(n + 2);
    for i in 0..n {
        let a = PI + PI * i as f64 / (n - 1) as f64;
        let r = 1.0 + (noise::fbm(i as f64 * 0.45, seed as f64 * 0.1, 3, seed as u32, 0) - 0.5) * 0.7;
        pts.push((cx + a.cos() * w * 0.5 * r, base + a.sin() * h * r));
    }
    pts.push((cx + w * 0.55, base + 8.0));
    pts.push((cx - w * 0.55, base + 8.0));
    cr.save_q();
    cr.new_path();
    closed_smooth(cr, &pts);
    let path = cr.copy_path().ok();
    let _ = cr.set_source(lin(0.0, base - h, 0.0, base, &[(0.0, (0.46, 0.43, 0.48), 1.0), (0.45, (0.30, 0.27, 0.32), 1.0), (1.0, (0.10, 0.08, 0.12), 1.0)]));
    cr.fill_keep();
    cr.clip();
    // Textura rugosa.
    let pat = cairo::SurfacePattern::create(rough);
    pat.set_extend(cairo::Extend::Repeat);
    let mut m = cairo::Matrix::identity();
    m.scale(1.0 / (h / 140.0).max(0.6), 1.0 / (h / 140.0).max(0.6));
    pat.set_matrix(m);
    cr.set_operator(cairo::Operator::SoftLight);
    let _ = cr.set_source(&pat);
    let _ = cr.paint_with_alpha(0.95);
    cr.set_operator(cairo::Operator::Over);
    // Algas coralinas (rosa/morado) y película verde en la parte baja.
    for k in 0..26u64 {
        let sd = seed * 977 + k * 13;
        let px = cx + (unit(sd) - 0.5) * w * 0.95;
        let py = base - unit(sd ^ 0x99) * h * 0.95;
        let rr = (0.015 + 0.05 * unit(sd ^ 0x33)) * w;
        let col = if k % 3 == 0 { (0.78, 0.36, 0.56) } else if k % 3 == 1 { (0.50, 0.28, 0.66) } else { (0.28, 0.5, 0.25) };
        rgba(cr, col, 0.30 + 0.25 * unit(sd ^ 0x77));
        cr.save_q();
        cr.translate(px, py);
        cr.scale(1.0, 0.55 + 0.4 * unit(sd ^ 0x21));
        cr.arc(0.0, 0.0, rr, 0.0, 2.0 * PI);
        cr.restore_q();
        cr.fill_q();
    }
    // Agujeros y grietas.
    for k in 0..5u64 {
        let sd = seed * 311 + k * 29;
        let px = cx + (unit(sd) - 0.5) * w * 0.7;
        let py = base - (0.15 + 0.7 * unit(sd ^ 0x44)) * h;
        let rr = (0.012 + 0.03 * unit(sd ^ 0x12)) * w;
        let g = RadialGradient::new(px, py, 0.0, px, py, rr);
        g.add_color_stop_rgba(0.0, 0.0, 0.0, 0.02, 0.85);
        g.add_color_stop_rgba(1.0, 0.0, 0.0, 0.02, 0.0);
        let _ = cr.set_source(&g);
        cr.arc(px, py, rr, 0.0, 2.0 * PI);
        cr.fill_q();
    }
    // Luz desde arriba y oclusión en la base.
    let _ = cr.set_source(lin(0.0, base - h, 0.0, base - h * 0.55, &[(0.0, (0.8, 0.95, 1.0), 0.28), (1.0, (0.8, 0.95, 1.0), 0.0)]));
    let _ = cr.paint();
    let _ = cr.set_source(lin(0.0, base - h * 0.30, 0.0, base + 6.0, &[(0.0, (0.0, 0.0, 0.02), 0.0), (1.0, (0.0, 0.0, 0.02), 0.55)]));
    let _ = cr.paint();
    cr.restore_q();
    if let Some(p) = path {
        cr.new_path();
        cr.append_path(&p);
        rgba(cr, (0.0, 0.0, 0.03), 0.35);
        cr.set_line_width(1.0);
        cr.stroke_q();
    }
}

/// Arena de aragonito con granos, guijarros, conchas y ondulaciones.
fn paint_sand(cr: &Context, t: &Tank, grain: &ImageSurface, wt: &Water) {
    let (w, h) = (t.w, t.h);
    let y0 = (h * 0.88).floor();
    cr.move_to(0.0, h);
    cr.line_to(0.0, t.floor_y(0.0));
    let mut x = 0.0;
    while x < w + 6.0 {
        cr.line_to(x, t.floor_y(x));
        x += 5.0;
    }
    cr.line_to(w, h);
    cr.close_path();
    cr.save_q();
    let _ = cr.set_source(lin(0.0, y0, 0.0, h, &[(0.0, (0.93, 0.85, 0.68), 1.0), (1.0, (0.50, 0.43, 0.33), 1.0)]));
    cr.fill_keep();
    cr.clip();
    // Ondulaciones del lecho.
    for k in 0..5 {
        let mut xx = 0.0;
        cr.new_path();
        while xx <= w + 8.0 {
            let y = t.floor_y(xx) + h * 0.02 * (k as f64 + 1.0) + 3.0 * (xx * 0.03 + k as f64 * 1.7).sin();
            if xx == 0.0 {
                cr.move_to(xx, y);
            } else {
                cr.line_to(xx, y);
            }
            xx += 8.0;
        }
        rgba(cr, if k % 2 == 0 { (1.0, 0.96, 0.85) } else { (0.2, 0.15, 0.1) }, 0.10);
        cr.set_line_width(2.5 + k as f64);
        cr.stroke_q();
    }
    // Grano fino.
    let pat = cairo::SurfacePattern::create(grain);
    pat.set_extend(cairo::Extend::Repeat);
    cr.set_operator(cairo::Operator::SoftLight);
    let _ = cr.set_source(&pat);
    let _ = cr.paint_with_alpha(0.95);
    cr.set_operator(cairo::Operator::Over);
    // Guijarros y conchas, cada uno con su sombra.
    for i in 0..110u64 {
        let px = unit(i * 3 + 1) * w;
        let py = t.floor_y(px) + (0.15 + 0.85 * unit(i * 3 + 2).powf(0.8)) * (h - t.floor_y(px));
        let sz = (1.2 + 3.4 * unit(i * 3 + 3).powi(2)) * (0.6 + (py - y0) / (h - y0) * 0.9);
        let kind = i % 4;
        let col = match kind {
            0 => (0.93, 0.90, 0.82),
            1 => (0.55, 0.52, 0.50),
            2 => (0.70, 0.56, 0.42),
            _ => (0.30, 0.28, 0.27),
        };
        rgba(cr, (0.0, 0.0, 0.0), 0.28);
        cr.save_q();
        cr.translate(px + sz * 0.25, py + sz * 0.35);
        cr.scale(1.0, 0.55);
        cr.arc(0.0, 0.0, sz * 1.05, 0.0, 2.0 * PI);
        cr.restore_q();
        cr.fill_q();
        let g = RadialGradient::new(px - sz * 0.3, py - sz * 0.3, 0.0, px, py, sz * 1.1);
        g.add_color_stop_rgba(0.0, lighten(col, 0.35).0, lighten(col, 0.35).1, lighten(col, 0.35).2, 1.0);
        g.add_color_stop_rgba(1.0, darken(col, 0.4).0, darken(col, 0.4).1, darken(col, 0.4).2, 1.0);
        let _ = cr.set_source(&g);
        cr.save_q();
        cr.translate(px, py);
        cr.scale(1.0, 0.7);
        cr.arc(0.0, 0.0, sz, 0.0, 2.0 * PI);
        cr.restore_q();
        cr.fill_q();
    }
    // Está bajo el agua: tinte azul y oscurecimiento hacia el fondo.
    let _ = cr.set_source(lin(0.0, y0, 0.0, h, &[(0.0, wt.mid, 0.22), (1.0, wt.bot, 0.50)]));
    let _ = cr.paint();
    cr.restore_q();
    // Borde de la arena contra el agua.
    cr.new_path();
    let mut x = 0.0;
    cr.move_to(0.0, t.floor_y(0.0));
    while x < w + 6.0 {
        cr.line_to(x, t.floor_y(x));
        x += 5.0;
    }
    rgba(cr, (0.9, 0.95, 1.0), 0.18);
    cr.set_line_width(1.4);
    cr.stroke_q();
}

/// Cristal y marco: reflejos, bordes que refractan, sombra de la tapa y brillo de la luminaria.
fn paint_glass(cr: &Context, w: f64, h: f64, wt: &Water) {
    let ab = wt.abyss;
    let k = if wt.dark { 1.0 } else { 0.55 };
    // Sombra de la tapa con el foco de luz de la luminaria.
    let _ = cr.set_source(lin(0.0, 0.0, 0.0, h * 0.10, &[(0.0, ab, 0.55 * k), (1.0, ab, 0.0)]));
    cr.rectangle(0.0, 0.0, w, h * 0.10);
    cr.fill_q();
    let g = RadialGradient::new(w * 0.5, 0.0, 0.0, w * 0.5, 0.0, w * 0.55);
    g.add_color_stop_rgba(0.0, 0.85, 0.97, 1.0, 0.20);
    g.add_color_stop_rgba(1.0, 0.85, 0.97, 1.0, 0.0);
    let _ = cr.set_source(&g);
    cr.rectangle(0.0, 0.0, w, h * 0.5);
    cr.fill_q();
    // Bordes laterales: el cristal grueso oscurece y refracta.
    for (x0, x1) in [(0.0, w * 0.05), (w, w * 0.95)] {
        let _ = cr.set_source(lin(x0, 0.0, x1, 0.0, &[(0.0, ab, 0.55 * k), (1.0, ab, 0.0)]));
        cr.rectangle(0.0f64.min(x0).min(x1), 0.0, (x1 - x0).abs(), h);
        cr.fill_q();
    }
    let _ = cr.set_source(lin(0.0, 0.0, 6.0, 0.0, &[(0.0, (0.8, 0.95, 1.0), 0.20), (1.0, (0.8, 0.95, 1.0), 0.0)]));
    cr.rectangle(0.0, 0.0, 6.0, h);
    cr.fill_q();
    let _ = cr.set_source(lin(w, 0.0, w - 6.0, 0.0, &[(0.0, (0.8, 0.95, 1.0), 0.16), (1.0, (0.8, 0.95, 1.0), 0.0)]));
    cr.rectangle(w - 6.0, 0.0, 6.0, h);
    cr.fill_q();
    // Reflejos diagonales en el cristal.
    for (x, wd, a) in [(0.16, 0.10, 0.07), (0.28, 0.025, 0.06), (0.74, 0.07, 0.045)] {
        let (x, wd) = (x * w, wd * w);
        let _ = cr.set_source(lin(0.0, 0.0, 0.0, h, &[(0.0, (1.0, 1.0, 1.0), a), (0.65, (1.0, 1.0, 1.0), a * 0.3), (1.0, (1.0, 1.0, 1.0), 0.0)]));
        cr.move_to(x, 0.0);
        cr.line_to(x + wd, 0.0);
        cr.line_to(x + wd - h * 0.28, h);
        cr.line_to(x - h * 0.28, h);
        cr.close_path();
        cr.fill_q();
    }
    // Viñeta general.
    let g = RadialGradient::new(w / 2.0, h / 2.0, w.min(h) * 0.38, w / 2.0, h / 2.0, w.max(h) * 0.78);
    g.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, 0.0);
    g.add_color_stop_rgba(1.0, ab.0, ab.1, ab.2, 0.50 * k);
    let _ = cr.set_source(&g);
    cr.rectangle(0.0, 0.0, w, h);
    cr.fill_q();
}

// ---------------------------------------------------------------- renderizador Cairo (referencia)

thread_local! {
    static LAYERS: RefCell<Option<((i32, i32, i32, u64), Rc<Layers>)>> = const { RefCell::new(None) };
}

fn layers(t: &Tank, ds: f64, wt: &Water, pal_key: u64) -> Rc<Layers> {
    let key = (t.w as i32, t.h as i32, (ds * 100.0) as i32, pal_key);
    LAYERS.with(|c| {
        let mut c = c.borrow_mut();
        if let Some((k, l)) = c.as_ref()
            && *k == key {
                return l.clone();
            }
        let l = Rc::new(bake(t, ds, wt));
        *c = Some((key, l.clone()));
        l
    })
}

pub fn draw_layer(cr: &Context, l: &Layer, alpha: f64) {
    let _ = cr.set_source_surface(&l.surf, l.x, l.y);
    let _ = cr.paint_with_alpha(alpha);
}

/// Cáusticas: dos capas de la baldosa a distinta escala y rumbo, visibles hacia el fondo.
pub fn caustics(cr: &Context, ly: &Layers, t: &Tank) {
    let (w, h) = (t.w, t.h);
    let top = h * 0.40;
    cr.save_q();
    cr.rectangle(0.0, top, w, h - top);
    cr.clip();
    // La luz solo dibuja la red de cáusticas sobre superficies: se recorta con la silueta de roca y arena.
    cr.push_group();
    draw_layer(cr, &ly.rocks, 1.0);
    draw_layer(cr, &ly.sand, 1.0);
    cr.set_operator(cairo::Operator::In);
    cr.push_group();
    for (scale, vx, vy, a) in [(2.8, 14.0, 6.0, 0.52), (2.0, -9.0, 10.0, 0.40)] {
        let pat = cairo::SurfacePattern::create(&ly.caustic);
        pat.set_extend(cairo::Extend::Repeat);
        pat.set_filter(cairo::Filter::Good);
        let mut m = cairo::Matrix::identity();
        m.translate(-(t.t * vx), -(t.t * vy));
        m.scale(1.0 / scale, 1.0 / scale);
        pat.set_matrix(m);
        let _ = cr.set_source(&pat);
        let _ = cr.paint_with_alpha(a * (0.85 + 0.15 * (t.t * 1.7).sin()));
    }
    let _ = cr.pop_group_to_source();
    let _ = cr.paint();
    cr.set_operator(cairo::Operator::Over);
    let _ = cr.pop_group_to_source();
    let fade = lin(0.0, top, 0.0, h, &[(0.0, (0.0, 0.0, 0.0), 0.35), (1.0, (0.0, 0.0, 0.0), 1.0)]);
    let _ = cr.mask(&fade);
    cr.restore_q();
}

pub fn draw_tank(cr: &Context, v: &View) {
    let t = v.tank;
    let ds = cr.target().device_scale().0.max(1.0);
    let wt = Water::from(v.pal);
    let ly = layers(t, ds, &wt, hash_str(&v.pal.raw));
    crate::draw::reset_budget();

    let stops: Vec<_> = wt.stops(t.boil).iter().map(|&(o, c)| (o, c, 1.0)).collect();
    let _ = cr.set_source(lin(0.0, 0.0, 0.0, t.h, &stops));
    cr.rectangle(0.0, 0.0, t.w, t.h);
    cr.fill_q();

    draw_layer(cr, &ly.far, 1.0);
    cr.save_q();
    cr.translate((t.t * 0.15).sin() * t.w * 0.03, 0.0);
    draw_layer(cr, &ly.rays, (1.0 - 0.5 * t.boil) * (0.85 + 0.15 * (t.t * 0.5).sin()));
    cr.restore_q();
    motes(cr, t);
    draw_layer(cr, &ly.rocks, 1.0);
    heat_glow(cr, t);
    draw_layer(cr, &ly.sand, 1.0);
    caustics(cr, &ly, t);

    for (i, d) in t.decor.iter().enumerate() {
        let x = d.xf * t.w;
        let base = t.floor_y(x) + 5.0;
        super::decor::draw(cr, d, x, base, t.t, t.boil, ds);
        if v.hover == Some(Pick::Decor(i)) {
            decor_outline(cr, d, x, base, v);
        }
    }
    // Sombras de los peces sobre la arena: anclan la escena.
    for f in t.fish.iter().filter(|f| f.life == Life::Alive) {
        let fy = t.floor_y(f.x);
        let k = (1.0 - (fy - f.y) / (t.h * 0.7)).clamp(0.0, 1.0);
        if k < 0.05 {
            continue;
        }
        cr.save_q();
        cr.translate(f.x, fy + 2.0);
        cr.scale(f.len * 0.45, f.len * 0.09 + 2.0);
        let g = RadialGradient::new(0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
        g.add_color_stop_rgba(0.0, 0.0, 0.0, 0.02, 0.32 * k * f.alpha);
        g.add_color_stop_rgba(1.0, 0.0, 0.0, 0.02, 0.0);
        let _ = cr.set_source(&g);
        cr.arc(0.0, 0.0, 1.0, 0.0, 2.0 * PI);
        cr.restore_q();
        cr.fill_q();
    }
    for &i in &t.order {
        crate::draw::fish(cr, &t.fish[i], v.pal, v.hover == Some(Pick::Fish(i)), ds, v.style);
    }
    bubbles(cr, t, ds);

    // Atenuación con la profundidad: la luz baja de arriba y el agua absorbe.
    let _ = cr.set_source(lin(0.0, 0.0, 0.0, t.h, &[(0.0, wt.top, 0.0), (0.6, wt.bot, 0.10), (1.0, wt.bot, 0.36)]));
    cr.rectangle(0.0, 0.0, t.w, t.h);
    cr.fill_q();

    surface(cr, t, &wt);
    overlays(cr, v);
    draw_layer(cr, &ly.glass, 1.0);
    crate::draw::labels(cr, v);
}

/// Marco del elemento de decoración señalado.
pub fn decor_outline(cr: &Context, d: &crate::sim::Decor, x: f64, base: f64, v: &View) {
    rgba(cr, v.pal.accent, 0.6);
    cr.set_line_width(1.5);
    cr.rectangle(x - d.size * 0.4, base - d.size, d.size * 0.8, d.size + 6.0);
    cr.stroke_q();
}

// ---------------------------------------------------------------- elementos dinámicos

pub fn motes(cr: &Context, t: &Tank) {
    for m in &t.motes {
        let tw = 0.6 + 0.4 * ((t.t * 1.3 + m.x * 0.05).sin() * 0.5 + 0.5);
        rgba(cr, (0.80, 0.93, 1.0), (0.05 + 0.20 * m.z) * tw);
        cr.arc(m.x, m.y, 0.6 + 1.5 * m.z, 0.0, 2.0 * PI);
        cr.fill_q();
    }
}

/// Resplandor ámbar del fondo cuando el agua hierve.
pub fn heat_glow(cr: &Context, t: &Tank) {
    let b = t.boil;
    if b <= 0.01 {
        return;
    }
    let fy = t.h * 0.915;
    let gh = t.h * 0.55;
    let glow = (1.0, 0.55, 0.16);
    let _ = cr.set_source(lin(0.0, fy, 0.0, fy - gh, &[(0.0, glow, 0.62 * b), (1.0, glow, 0.0)]));
    cr.rectangle(0.0, fy - gh, t.w, gh + t.h * 0.1);
    cr.fill_q();
    for i in 0..5 {
        let vx = t.vent_x(i);
        let r = t.w * 0.07;
        let g = RadialGradient::new(vx, t.floor_y(vx), 0.0, vx, t.floor_y(vx), r);
        g.add_color_stop_rgba(0.0, 1.0, 0.80, 0.40, 0.5 * b);
        g.add_color_stop_rgba(1.0, 1.0, 0.55, 0.15, 0.0);
        let _ = cr.set_source(&g);
        cr.rectangle(vx - r, t.floor_y(vx) - r, 2.0 * r, 2.0 * r);
        cr.fill_q();
    }
}

thread_local! {
    static BUBBLES: RefCell<Vec<Option<ImageSurface>>> = const { RefCell::new(Vec::new()) };
    static BUBBLE_DS: RefCell<f64> = const { RefCell::new(0.0) };
}

/// Radio (en medios píxeles) → cubo del sprite de burbuja.
pub fn bubble_bucket(r: f64) -> usize {
    ((r * 2.0).round() as usize).clamp(2, 34)
}

/// Burbuja realista: borde que refracta (claro arriba a la izquierda, tenue abajo a la derecha),
/// interior casi transparente y un reflejo especular.
pub fn bubble_sprite(bucket: usize, ds: f64) -> ImageSurface {
    let r = bucket as f64 / 2.0;
    let side = r * 2.0 + 4.0;
    let px = (side * ds).ceil() as i32;
    let surf = ImageSurface::create(cairo::Format::ARgb32, px, px).expect("bubble");
    surf.set_device_scale(ds, ds);
    {
        let cr = Context::new(&surf).expect("cairo");
        let c = side / 2.0;
        // Interior: casi transparente, un poco más denso hacia el borde.
        let g = RadialGradient::new(c, c, r * 0.2, c, c, r);
        g.add_color_stop_rgba(0.0, 0.7, 0.95, 1.0, 0.0);
        g.add_color_stop_rgba(0.85, 0.7, 0.95, 1.0, 0.10);
        g.add_color_stop_rgba(1.0, 0.8, 0.98, 1.0, 0.30);
        let _ = cr.set_source(&g);
        cr.arc(c, c, r, 0.0, 2.0 * PI);
        cr.fill_q();
        // Borde oscuro fino y arcos de luz.
        rgba(&cr, (0.0, 0.12, 0.25), 0.28);
        cr.set_line_width(0.8);
        cr.arc(c, c, r, 0.0, 2.0 * PI);
        cr.stroke_q();
        cr.set_line_cap(cairo::LineCap::Round);
        rgba(&cr, (1.0, 1.0, 1.0), 0.65);
        cr.set_line_width((r * 0.16).clamp(0.8, 2.2));
        cr.arc(c, c, r * 0.86, PI * 1.05, PI * 1.55);
        cr.stroke_q();
        rgba(&cr, (0.85, 0.97, 1.0), 0.32);
        cr.set_line_width((r * 0.12).clamp(0.6, 1.6));
        cr.arc(c, c, r * 0.86, PI * 0.05, PI * 0.45);
        cr.stroke_q();
        if r > 3.0 {
            rgba(&cr, (1.0, 1.0, 1.0), 0.85);
            cr.save_q();
            cr.translate(c - r * 0.35, c - r * 0.38);
            cr.scale(1.0, 0.7);
            cr.arc(0.0, 0.0, r * 0.15, 0.0, 2.0 * PI);
            cr.restore_q();
            cr.fill_q();
        }
    }
    surf
}

fn bubbles(cr: &Context, t: &Tank, ds: f64) {
    BUBBLES.with(|cache| {
        BUBBLE_DS.with(|d| {
            let mut cache = cache.borrow_mut();
            if (*d.borrow() - ds).abs() > 0.01 {
                cache.clear();
                *d.borrow_mut() = ds;
            }
            if cache.len() < 35 {
                cache.resize_with(35, || None);
            }
            for b in &t.bubbles {
                match b.kind {
                    BubbleKind::Air => {
                        if b.r < 1.7 {
                            rgba(cr, (0.85, 0.96, 1.0), 0.45);
                            cr.arc(b.x, b.y, b.r.max(0.7), 0.0, 2.0 * PI);
                            cr.fill_q();
                            continue;
                        }
                        let k = bubble_bucket(b.r);
                        let spr = cache[k].get_or_insert_with(|| bubble_sprite(k, ds));
                        let off = k as f64 / 2.0 + 2.0;
                        let _ = cr.set_source_surface(&*spr, b.x - off, b.y - off);
                        let _ = cr.paint();
                    }
                    BubbleKind::Foam => {
                        let a = (b.life / b.max_life).clamp(0.0, 1.0).sqrt();
                        rgba(cr, (0.92, 0.98, 1.0), 0.38 * a);
                        cr.arc(b.x, b.y, b.r, 0.0, 2.0 * PI);
                        cr.fill_q();
                    }
                }
            }
        })
    });
}

/// Superficie vista desde el lateral: el aire arriba, un menisco brillante y la franja plateada
/// justo debajo, donde el agua refleja el interior del acuario.
pub fn surface(cr: &Context, t: &Tank, wt: &Water) {
    use crate::draw::{surface_at, surface_path};
    surface_path(cr, t);
    cr.line_to(t.w, 0.0);
    cr.line_to(0.0, 0.0);
    cr.close_path();
    let _ = cr.set_source(lin(0.0, 0.0, 0.0, t.surface_y() + 10.0, &[(0.0, wt.air, 1.0), (1.0, mix(wt.air, wt.mid, 0.35), 1.0)]));
    cr.fill_q();
    // Franja plateada bajo el menisco (reflexión total).
    let sy = t.surface_y();
    let g = lin(0.0, sy - 4.0, 0.0, sy + 26.0, &[(0.0, (0.75, 0.93, 1.0), 0.55), (0.25, (0.55, 0.85, 0.95), 0.25), (1.0, (0.3, 0.7, 0.9), 0.0)]);
    surface_path(cr, t);
    cr.line_to(t.w, sy + 26.0);
    cr.line_to(0.0, sy + 26.0);
    cr.close_path();
    let _ = cr.set_source(&g);
    cr.fill_q();
    surface_path(cr, t);
    rgba(cr, (0.92, 0.99, 1.0), 0.80);
    cr.set_line_width(1.6);
    cr.stroke_q();
    for &(x, age) in &t.ripples {
        let k = age / 0.9;
        rgba(cr, (0.9, 0.98, 1.0), 0.5 * (1.0 - k));
        cr.set_line_width(1.1);
        cr.save_q();
        cr.translate(x, surface_at(t, x) + 2.0);
        cr.scale(1.0, 0.25);
        cr.arc(0.0, 0.0, 3.0 + 18.0 * k, 0.0, 2.0 * PI);
        cr.restore_q();
        cr.stroke_q();
    }
}

pub fn overlays(cr: &Context, v: &View) {
    let t = v.tank;
    // Turbidez con la RAM llena.
    let k = ((t.ram_frac - 0.5) / 0.5).clamp(0.0, 1.0);
    if k > 0.01 {
        rgba(cr, (0.18, 0.30, 0.30), 0.34 * k * k);
        cr.rectangle(0.0, t.surface_y(), t.w, t.h);
        cr.fill_q();
    }
    if t.boil > 0.3 {
        let hot = (1.0, 0.45, 0.12);
        let g = RadialGradient::new(t.w / 2.0, t.h * 1.05, t.h * 0.3, t.w / 2.0, t.h * 1.05, t.w.max(t.h) * 0.85);
        g.add_color_stop_rgba(0.0, hot.0, hot.1, hot.2, 0.0);
        g.add_color_stop_rgba(1.0, hot.0, hot.1, hot.2, 0.26 * t.boil);
        let _ = cr.set_source(&g);
        cr.rectangle(0.0, 0.0, t.w, t.h);
        cr.fill_q();
    }
}

/// Galería de especies para afinar el dibujo: `--snapshot out.png --gallery [--len 150]`.
pub fn gallery(cr: &Context, w: f64, h: f64, len: f64, font: &str, pal: &crate::theme::Palette) {
    use crate::draw::FishArt;
    use crate::sim::{Look, Pattern};
    use crate::theme::Style;
    let _ = cr.set_source(lin(0.0, 0.0, 0.0, h, &[(0.0, (0.10, 0.45, 0.60), 1.0), (1.0, (0.01, 0.10, 0.20), 1.0)]));
    cr.rectangle(0.0, 0.0, w, h);
    cr.fill_q();
    cr.select_font_face(font, cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(12.0);
    let n = super::species::SPECIES.len();
    let (cols, rows) = (5, n.div_ceil(5));
    for i in 0..n {
        let (cx, cy) = ((i % cols) as f64 + 0.5, (i / cols) as f64 + 0.5);
        let (cx, cy) = (cx * w / cols as f64, cy * h / rows as f64);
        let look = Look { species: 0, real: i, pattern: Pattern::Plain, c1: 0, c2: 1, tint: 0.0, seed: 1000 + i as u64 * 77 };
        let art = FishArt { look, len, phase: 1.3, amp: 0.6, dying: false, style: Style::Realistic };
        cr.save_q();
        cr.translate(cx, cy);
        super::fish::paint(cr, &art, pal);
        cr.restore_q();
        let name = super::species::SPECIES[i].name;
        rgba(cr, (1.0, 1.0, 1.0), 0.9);
        cr.move_to(cx - 40.0, cy + h / rows as f64 * 0.45);
        let _ = cr.show_text(name);
    }
    let _ = pal;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hornear las capas debe ser razonablemente rápido (ocurre al abrir y al terminar de redimensionar).
    #[test]
    fn bake_layers_is_fast_enough() {
        let t = Tank::new(1180.0, 680.0, 1);
        let start = std::time::Instant::now();
        let ly = bake(&t, 1.0, &Water::from(&Palette::load()));
        let ms = start.elapsed().as_millis();
        eprintln!("bake 1180x680 @1x: {ms} ms");
        assert!(ms < 1500, "bake tardó {ms} ms");
        assert!(ly.sand.surf.width() > 0);
    }
}
