//! Compositor de la pecera en GPU (GSK). Pintar todo con Cairo cuesta ~25 ms por fotograma (la CPU
//! rasteriza y GTK sube la imagen entera cada vez); aquí cada cosa se pinta con Cairo solo cuando
//! cambia y GTK la compone en la GPU:
//!   - peces y decoración: sprites (texturas) que se mueven, voltean y giran con transformaciones;
//!   - agua, brillos y viñeta: degradados nativos;
//!   - burbujas: bordes redondeados;
//!   - texto: Pango.
//! El renderizador Cairo de `draw.rs` sigue siendo la referencia para `--snapshot`.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::cairo;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};

use crate::draw::{self, SKey, View};
use crate::sim::{BubbleKind, Life, Pick};
use crate::reef::gpu_scene::RealGpu;
use crate::theme::{Rgb, Style, lighten, mix};

// ---------------------------------------------------------------- widget

type RenderFn = Box<dyn Fn(&TankWidget, &gtk::Snapshot)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TankWidget {
        pub render: RefCell<Option<RenderFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TankWidget {
        const NAME: &'static str = "ThermalAcuariumTank";
        type Type = super::TankWidget;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TankWidget {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }
    }

    impl WidgetImpl for TankWidget {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (320, 640, -1, -1),
                _ => (240, 400, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(f) = self.render.borrow().as_ref() {
                f(&self.obj(), snapshot);
            }
        }
    }
}

glib::wrapper! {
    pub struct TankWidget(ObjectSubclass<imp::TankWidget>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TankWidget {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Función que construye el árbol de nodos de cada fotograma.
    pub fn set_render(&self, f: impl Fn(&TankWidget, &gtk::Snapshot) + 'static) {
        *self.imp().render.borrow_mut() = Some(Box::new(f));
    }
}

// ---------------------------------------------------------------- caché

struct GpuSprite {
    tex: gdk::MemoryTexture,
    /// Tamaño y origen en px lógicos.
    w: f32,
    h: f32,
    ox: f32,
    oy: f32,
    len: f64,
}

struct DecorTex {
    tex: gdk::MemoryTexture,
    rect: graphene::Rect,
}

#[derive(Default)]
pub struct GpuCache {
    pal: String,
    ds: f64,
    size: (i32, i32),
    sprites: HashMap<SKey, GpuSprite>,
    bytes: usize,
    rays: Option<gdk::MemoryTexture>,
    sand: Option<(gdk::MemoryTexture, f32)>,
    decor: Vec<DecorTex>,
    decor_stamp: i64,
    budget: u32,
    style: Option<Style>,
    /// Capas y sprites del estilo realista.
    pub(crate) real: RealGpu,
}

const MAX_SPRITE_BYTES: usize = 160 << 20;
/// Cuántas veces por segundo se repinta la decoración que se mece.
const DECOR_HZ: f64 = 10.0;

pub(crate) fn texture_of(mut surf: cairo::ImageSurface) -> Option<gdk::MemoryTexture> {
    let (w, h, stride) = (surf.width(), surf.height(), surf.stride() as usize);
    surf.flush();
    let data = surf.data().ok()?;
    let bytes = glib::Bytes::from(&data[..]);
    Some(gdk::MemoryTexture::new(w, h, gdk::MemoryFormat::B8g8r8a8Premultiplied, &bytes, stride))
}

/// Pinta con Cairo una capa de `w`×`h` px lógicos a escala `ds` y la convierte en textura.
/// Como `bake`, pero a una celda por píxel: se amplía luego con `ScalingFilter::Nearest`.
fn bake_px(w: f64, h: f64, cell: f64, paint: impl FnOnce(&cairo::Context)) -> Option<gdk::MemoryTexture> {
    let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, (w / cell).ceil().max(1.0) as i32, (h / cell).ceil().max(1.0) as i32).ok()?;
    {
        let cr = cairo::Context::new(&surf).ok()?;
        cr.set_antialias(cairo::Antialias::None);
        cr.scale(1.0 / cell, 1.0 / cell);
        paint(&cr);
    }
    texture_of(surf)
}

fn bake(w: f64, h: f64, ds: f64, paint: impl FnOnce(&cairo::Context)) -> Option<gdk::MemoryTexture> {
    let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, (w * ds).ceil().max(1.0) as i32, (h * ds).ceil().max(1.0) as i32).ok()?;
    surf.set_device_scale(ds, ds);
    {
        let cr = cairo::Context::new(&surf).ok()?;
        paint(&cr);
    }
    texture_of(surf)
}

// ---------------------------------------------------------------- helpers de GSK

pub(crate) fn rect(x: f64, y: f64, w: f64, h: f64) -> graphene::Rect {
    graphene::Rect::new(x as f32, y as f32, w as f32, h as f32)
}

pub(crate) fn pt(x: f64, y: f64) -> graphene::Point {
    graphene::Point::new(x as f32, y as f32)
}

pub(crate) fn col(c: Rgb, a: f64) -> gdk::RGBA {
    gdk::RGBA::new(c.0 as f32, c.1 as f32, c.2 as f32, a as f32)
}

pub(crate) fn stops(s: &[(f32, Rgb, f64)]) -> Vec<gsk::ColorStop> {
    s.iter().map(|&(o, c, a)| gsk::ColorStop::new(o, col(c, a))).collect()
}

pub(crate) fn ring(snap: &gtk::Snapshot, bounds: &graphene::Rect, radius: f32, width: f32, c: &gdk::RGBA) {
    let rr = gsk::RoundedRect::from_rect(*bounds, radius);
    snap.append_border(&rr, &[width; 4], &[*c, *c, *c, *c]);
}

/// Marco de 1.5 px alrededor de un rectángulo.
pub(crate) fn outline(snap: &gtk::Snapshot, r: &graphene::Rect, c: &gdk::RGBA) {
    ring(snap, r, 0.0, 1.5, c);
}

pub(crate) fn disc(snap: &gtk::Snapshot, bounds: &graphene::Rect, radius: f32, c: &gdk::RGBA) {
    let rr = gsk::RoundedRect::from_rect(*bounds, radius);
    snap.push_rounded_clip(&rr);
    snap.append_color(c, bounds);
    snap.pop();
}

fn text(widget: &gtk::Widget, s: &str, font: &str, size: f64) -> pango::Layout {
    let l = widget.create_pango_layout(Some(s));
    let mut fd = pango::FontDescription::new();
    fd.set_family(font);
    fd.set_weight(pango::Weight::Bold);
    fd.set_absolute_size(size * pango::SCALE as f64);
    l.set_font_description(Some(&fd));
    l
}

/// Texto con sombra; `y` es la línea base.
fn put_text(snap: &gtk::Snapshot, l: &pango::Layout, x: f64, y: f64, c: Rgb) {
    let top = y - l.baseline() as f64 / pango::SCALE as f64;
    for (dx, color) in [(1.0, col((0.0, 0.0, 0.0), 0.6)), (0.0, col(c, 1.0))] {
        snap.save();
        snap.translate(&pt(x + dx, top + dx));
        snap.append_layout(l, &color);
        snap.restore();
    }
}

// ---------------------------------------------------------------- fotograma

pub fn render(widget: &gtk::Widget, snap: &gtk::Snapshot, v: &View, cache: &mut GpuCache) {
    let t = v.tank;
    let (w, h) = (t.w, t.h);
    if w < 2.0 || h < 2.0 {
        return;
    }
    let ds = widget.scale_factor().max(1) as f64;
    let size = (w as i32, h as i32);
    if cache.style != Some(v.style) {
        cache.sprites.clear();
        cache.bytes = 0;
        cache.real.clear();
        cache.style = Some(v.style);
    }
    if cache.pal != v.pal.raw || (cache.ds - ds).abs() > 0.01 || cache.size != size {
        if cache.pal != v.pal.raw {
            // Otro tema de Omarchy: el agua, la arena y los peces del estilo realista se recalculan.
            cache.real.clear();
        }
        cache.sprites.clear();
        cache.bytes = 0;
        cache.rays = None;
        cache.sand = None;
        cache.decor.clear();
        cache.decor_stamp = -1;
        cache.pal = v.pal.raw.clone();
        cache.ds = ds;
        cache.size = size;
    }
    if cache.bytes > MAX_SPRITE_BYTES {
        cache.sprites.clear();
        cache.bytes = 0;
    }
    cache.budget = if v.style == Style::Realistic { 3 } else { 6 };
    if v.style == Style::Realistic {
        if (cache.ds - ds).abs() > 0.01 {
            cache.real.clear();
        }
        crate::reef::gpu_scene::render(widget, snap, v, cache, ds);
        return;
    }

    let px = v.style == Style::Pixel;
    draw::set_cell(t.scale());
    let cell = draw::cell();
    let full = rect(0.0, 0.0, w, h);
    let (top_c, bot_c) = draw::water_colors(v);
    let b = t.boil;

    // Agua: degradado, o franjas planas en el estilo Pixel.
    if px {
        for (y0, y1, c) in crate::pixel::water_bands(v) {
            snap.append_color(&col(c, 1.0), &rect(0.0, y0, w, y1 - y0 + 0.5));
        }
    } else {
        snap.append_linear_gradient(&full, &pt(0.0, 0.0), &pt(0.0, h), &stops(&[(0.0, top_c, 1.0), (1.0, bot_c, 1.0)]));
    }

    // Rayos de luz: una textura estática que se desplaza despacio.
    if cache.rays.is_none() {
        cache.rays = if px { bake_px(w, h, cell, |cr| draw::rays(cr, v, 0.0, 0.0)) } else { bake(w, h, ds, |cr| draw::rays(cr, v, 0.0, 0.0)) };
    }
    if let Some(tex) = &cache.rays {
        snap.save();
        let dx = (t.t * 0.15).sin() * w * 0.03;
        snap.translate(&pt(if px { draw::snap(dx) } else { dx }, 0.0));
        snap.push_opacity(1.0 - 0.5 * b);
        if px {
            snap.append_scaled_texture(tex, gsk::ScalingFilter::Nearest, &full);
        } else {
            snap.append_texture(tex, &full);
        }
        snap.pop();
        snap.restore();
    }

    // Nieve marina.
    let mote = col(lighten(top_c, 0.7), 1.0);
    for m in &t.motes {
        let r = 0.7 + 1.4 * m.z;
        let mut c = mote;
        c.set_alpha((0.10 + 0.22 * m.z) as f32);
        if px {
            snap.append_color(&c, &rect(draw::snap(m.x), draw::snap(m.y), cell, cell));
        } else {
            snap.append_color(&c, &rect(m.x - r, m.y - r, 2.0 * r, 2.0 * r));
        }
    }

    // Resplandor del fondo, arena, brillo de los respiraderos.
    let fy = h * 0.915;
    if b > 0.01 {
        let glow = mix(v.pal.orange, v.pal.yellow, 0.35);
        let gh = h * 0.55;
        snap.append_linear_gradient(
            &rect(0.0, fy - gh, w, gh + h * 0.1),
            &pt(0.0, fy),
            &pt(0.0, fy - gh),
            &stops(&[(0.0, glow, 0.75 * b), (1.0, glow, 0.0)]),
        );
    }
    if cache.sand.is_none() {
        let y0 = (h * 0.88).floor();
        cache.sand = bake(w, h - y0, ds, |cr| {
            cr.translate(0.0, -y0);
            draw::floor(cr, v, false);
        })
        .map(|tex| (tex, y0 as f32));
    }
    if let Some((tex, y0)) = &cache.sand {
        snap.append_texture(tex, &graphene::Rect::new(0.0, *y0, w as f32, h as f32 - *y0));
    }
    if b > 0.01 {
        for i in 0..5 {
            let vx = t.vent_x(i);
            let r = w * 0.07;
            let vy = t.floor_y(vx);
            snap.append_radial_gradient(
                &rect(vx - r, vy - r * 0.7, 2.0 * r, r * 1.4),
                &pt(vx, vy),
                r as f32,
                (r * 0.7) as f32,
                0.0,
                1.0,
                &stops(&[(0.0, (1.0, 0.85, 0.45), 0.55 * b), (1.0, (1.0, 0.6, 0.2), 0.0)]),
            );
        }
    }

    decor(snap, cache, v, ds);

    // Los compañeros de banco del pez señalado se iluminan suavemente.
    if let Some(Pick::Fish(hi)) = v.hover
        && let Some(hf) = t.fish.get(hi) {
            for f in t.fish.iter().filter(|f| f.kh == hf.kh && f.life == Life::Alive) {
                let r = f.len * 0.75;
                snap.append_radial_gradient(
                    &rect(f.x - r, f.y - r, 2.0 * r, 2.0 * r),
                    &pt(f.x, f.y),
                    r as f32,
                    r as f32,
                    0.0,
                    1.0,
                    &stops(&[(0.0, v.pal.accent, 0.22), (1.0, v.pal.accent, 0.0)]),
                );
            }
        }

    for &i in &t.order {
        fish(snap, cache, v, i, ds);
    }
    if px {
        pixel_bubbles(snap, v, top_c);
    } else {
        bubbles(snap, v, top_c);
    }

    // Superficie: una franja fina que sí se repinta cada fotograma.
    let strip = t.surface_y() + 30.0 * t.scale();
    {
        let cr = snap.append_cairo(&rect(0.0, 0.0, w, strip));
        if px {
            crate::pixel::surface(&cr, v);
        } else {
            draw::surface(&cr, v);
        }
    }

    // Turbidez, viñeta y tinte de calor.
    let k = ((t.ram_frac - 0.5) / 0.5).clamp(0.0, 1.0);
    if k > 0.01 {
        let murk = mix(top_c, v.pal.bg, 0.4);
        snap.append_color(&col(murk, 0.34 * k * k), &rect(0.0, t.surface_y(), w, h));
    }
    let big = w.max(h) * 0.75;
    snap.append_radial_gradient(
        &full,
        &pt(w / 2.0, h / 2.0),
        big as f32,
        big as f32,
        (w.min(h) * 0.35 / big) as f32,
        1.0,
        &stops(&[(0.0, (0.0, 0.0, 0.0), 0.0), (1.0, (0.0, 0.0, 0.0), 0.32)]),
    );
    if b > 0.3 {
        let hot = mix(v.pal.orange, v.pal.red, 0.5);
        let r = w.max(h) * 0.85;
        snap.push_opacity(b);
        snap.append_radial_gradient(
            &full,
            &pt(w / 2.0, h * 1.05),
            r as f32,
            r as f32,
            (h * 0.3 / r) as f32,
            1.0,
            &stops(&[(0.0, hot, 0.0), (1.0, hot, 0.30)]),
        );
        snap.pop();
    }

    labels(snap, widget, v);
}

// ---------------------------------------------------------------- decoración

fn decor(snap: &gtk::Snapshot, cache: &mut GpuCache, v: &View, ds: f64) {
    let t = v.tank;
    let px = v.style == Style::Pixel;
    // Se repinta a pocos Hz: la decoración se mece despacio y Cairo es lo más caro del fotograma.
    let stamp = (t.t * DECOR_HZ).floor() as i64;
    if stamp != cache.decor_stamp || cache.decor.len() != t.decor.len() {
        cache.decor_stamp = stamp;
        cache.decor = t
            .decor
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                let (bx, by, bw, bh) = draw::decor_box(t, d);
                let hover = v.hover == Some(Pick::Decor(i));
                let tex = if px {
                    bake_px(bw, bh, draw::cell(), |cr| {
                        cr.translate(-bx, -by);
                        draw::decor(cr, v, d, hover);
                    })?
                } else {
                    bake(bw, bh, ds, |cr| {
                        cr.translate(-bx, -by);
                        draw::decor(cr, v, d, hover);
                    })?
                };
                Some(DecorTex { tex, rect: rect(bx, by, bw, bh) })
            })
            .collect();
    }
    for d in &cache.decor {
        if px {
            snap.append_scaled_texture(&d.tex, gsk::ScalingFilter::Nearest, &d.rect);
        } else {
            snap.append_texture(&d.tex, &d.rect);
        }
    }
}

// ---------------------------------------------------------------- peces

/// Sprite del pez `i` (renderizado con Cairo una sola vez por variante).
fn sprite<'a>(cache: &'a mut GpuCache, v: &View, i: usize, ds: f64) -> Option<&'a GpuSprite> {
    let f = &v.tank.fish[i];
    let (key, blen) = draw::sprite_key(f, v.style);
    if !cache.sprites.contains_key(&key) {
        let np = draw::nphases(f.len);
        // Si ya hay un sprite de una fase vecina, vale mientras no sobre presupuesto de renderizado.
        let near = (1..=np / 2).find_map(|d| {
            [(key.phase as usize + d) % np, (key.phase as usize + np - d) % np]
                .into_iter()
                .map(|p| SKey { phase: p as u8, ..key })
                .find(|k| cache.sprites.contains_key(k))
        });
        if let Some(k) = near
            && cache.budget == 0 {
                return cache.sprites.get(&k);
            }
        cache.budget = cache.budget.saturating_sub(1);
        let spr = draw::render_sprite(f, &key, blen, v.pal, ds);
        let (wp, hp) = (spr.surf.width(), spr.surf.height());
        let tex = texture_of(spr.surf)?;
        cache.bytes += (wp * hp * 4) as usize;
        // Los sprites de píxeles no llevan escala de dispositivo: se amplían por su factor entero.
        let k = if key.style == 1 { key.size as f64 } else { 1.0 / ds };
        cache.sprites.insert(
            key,
            GpuSprite { tex, w: (wp as f64 * k) as f32, h: (hp as f64 * k) as f32, ox: spr.ox as f32, oy: spr.oy as f32, len: spr.len },
        );
    }
    cache.sprites.get(&key)
}

pub(crate) fn fish(snap: &gtk::Snapshot, cache: &mut GpuCache, v: &View, i: usize, ds: f64) {
    let f = &v.tank.fish[i];
    if f.alpha <= 0.01 || f.len < 4.0 {
        return;
    }
    let hl = v.hover == Some(Pick::Fish(i));
    let Some(s) = sprite(cache, v, i, ds) else { return };
    let px = v.style == Style::Pixel;
    let nz = |x: f64| if x.abs() < 0.03 { 0.03f64.copysign(if x == 0.0 { 1.0 } else { x }) } else { x } as f32;
    snap.save();
    if px {
        // Píxeles: posición en la rejilla, volteo entero, sin giro ni escalado fraccionario.
        snap.translate(&pt(draw::snap(f.x), draw::snap(f.y)));
        snap.scale(nz(f.face).signum(), nz(f.flipv).signum());
    } else {
        let k = (f.len / s.len) as f32;
        snap.translate(&pt(f.x, f.y));
        snap.scale(nz(f.face), nz(f.flipv));
        snap.rotate((-f.pitch).to_degrees() as f32);
        snap.scale(k, k);
    }
    let alpha = f.alpha * draw::depth_alpha(f, v.style);
    let fade = alpha < 0.99;
    if fade {
        snap.push_opacity(alpha.clamp(0.0, 1.0));
    }
    let bounds = graphene::Rect::new(-s.ox, -s.oy, s.w, s.h);
    if px {
        snap.append_scaled_texture(&s.tex, gsk::ScalingFilter::Nearest, &bounds);
    } else {
        snap.append_texture(&s.tex, &bounds);
    }
    if fade {
        snap.pop();
    }
    snap.restore();

    if hl {
        // Anillo de selección alrededor del pez señalado.
        let a = f.len * 0.55 + 6.0;
        let bh = draw::body_half_height(f, v.style) * 1.5 + 6.0;
        snap.save();
        snap.translate(&pt(f.x, f.y));
        snap.rotate((-f.pitch * f.face.signum()).to_degrees() as f32);
        ring(snap, &rect(-a, -bh, 2.0 * a, 2.0 * bh), a.min(bh) as f32, 2.0, &col(v.pal.accent, 0.95));
        snap.restore();
    }
}

// ---------------------------------------------------------------- burbujas

fn bubbles(snap: &gtk::Snapshot, v: &View, top_c: Rgb) {
    let c = lighten(top_c, 0.85);
    let (ring_c, fill_c, hi_c) = (col(c, 0.55), col(c, 0.10), col((1.0, 1.0, 1.0), 0.7));
    for b in &v.tank.bubbles {
        match b.kind {
            BubbleKind::Air => {
                let r = b.r;
                if r < 1.8 {
                    // Las diminutas son un punto.
                    snap.append_color(&col(c, 0.4), &rect(b.x - r, b.y - r, 2.0 * r, 2.0 * r));
                    continue;
                }
                let bounds = rect(b.x - r, b.y - r, 2.0 * r, 2.0 * r);
                if r > 2.5 {
                    disc(snap, &bounds, r as f32, &fill_c);
                }
                ring(snap, &bounds, r as f32, 1.0, &ring_c);
                if r > 3.5 {
                    snap.append_color(&hi_c, &rect(b.x - b.r * 0.41, b.y - b.r * 0.41, b.r * 0.44, b.r * 0.44));
                }
            }
            BubbleKind::Foam => {
                let k = (b.life / b.max_life).clamp(0.0, 1.0);
                disc(snap, &rect(b.x - b.r, b.y - b.r, 2.0 * b.r, 2.0 * b.r), b.r as f32, &col((1.0, 1.0, 1.0), 0.40 * k.sqrt()));
            }
        }
    }
}

/// Burbujas del estilo Pixel: cuadrados huecos en la rejilla.
fn pixel_bubbles(snap: &gtk::Snapshot, v: &View, top_c: Rgb) {
    let c = lighten(top_c, 0.85);
    let cell = draw::cell();
    let (edge, fill, dot, foam) = (col(c, 0.45), col(c, 0.12), col(c, 0.55), col(v.pal.bfg, 1.0));
    for b in &v.tank.bubbles {
        let k = (b.r * 2.0 / cell).round().clamp(1.0, 4.0);
        let (x, y) = (draw::snap(b.x - b.r), draw::snap(b.y - b.r));
        match b.kind {
            BubbleKind::Air if k <= 1.0 => snap.append_color(&dot, &rect(x, y, cell, cell)),
            BubbleKind::Air => {
                snap.append_color(&fill, &rect(x, y, k * cell, k * cell));
                snap.append_color(&edge, &rect(x, y, k * cell, cell));
                snap.append_color(&edge, &rect(x, y + (k - 1.0) * cell, k * cell, cell));
                snap.append_color(&edge, &rect(x, y, cell, k * cell));
                snap.append_color(&edge, &rect(x + (k - 1.0) * cell, y, cell, k * cell));
            }
            BubbleKind::Foam => {
                let mut f = foam;
                f.set_alpha((0.40 * (b.life / b.max_life).clamp(0.0, 1.0).sqrt()) as f32);
                snap.append_color(&f, &rect(x, y, k * cell, cell));
            }
        }
    }
}

// ---------------------------------------------------------------- texto

pub(crate) fn labels(snap: &gtk::Snapshot, widget: &gtk::Widget, v: &View) {
    let t = v.tank;
    let p = v.pal;
    let plan = draw::plan_labels(v);
    let fs = plan.fs;
    for n in &plan.names {
        let l = text(widget, &n.text, v.font, fs);
        let (tw, _) = l.pixel_size();
        put_text(snap, &l, n.x - tw as f64 / 2.0, n.y, draw::label_color(p));
    }
    if let Some(hid) = &plan.hidden {
        put_text(snap, &text(widget, hid, v.font, fs), 12.0, t.h - 12.0, mix(draw::label_color(p), (0.5, 0.6, 0.7), 0.35));
    }
    if plan.paused {
        let l = text(widget, crate::i18n::tr("paused"), v.font, fs + 3.0);
        let (tw, _) = l.pixel_size();
        put_text(snap, &l, t.w - tw as f64 - 14.0, t.surface_y() + fs + 12.0, p.yellow);
    }
    let Some(card) = &plan.card else { return };
    let layouts: Vec<pango::Layout> = card.lines.iter().map(|s| text(widget, s, v.font, fs)).collect();
    let tw = layouts.iter().map(|l| l.pixel_size().0).max().unwrap_or(0) as f64;
    let (bx, by, bw, bh, lh) = draw::card_rect(card, tw, fs, t);
    let r = rect(bx, by, bw, bh);
    snap.append_color(&col(p.dbg, 0.92), &r);
    ring(snap, &r, 0.0, 1.0, &col(p.accent, 0.9));
    for (i, l) in layouts.iter().enumerate() {
        put_text(snap, l, bx + 10.0, by + lh * (i as f64 + 1.0), if i == 0 { p.bfg } else { p.fg });
    }
}
