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

use crate::draw::{self, DKey, SKey, View};
use crate::reef::gpu_scene::RealGpu;
use crate::sim::{BubbleKind, Life, Pick};
use crate::theme::{Rgb, Style, lighten, mix};
use crate::worker::{Done, Job, Pixels, Worker};

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
    /// Último fotograma en que se usó y lo que ocupa, para expulsar los que llevan tiempo sin usarse.
    used: u32,
    bytes: usize,
}

struct DecorTex {
    tex: gdk::MemoryTexture,
    w: f32,
    h: f32,
    ox: f32,
    oy: f32,
    /// Tamaño con el que se pintó.
    size: f64,
}

/// Familia de un sprite de pez: todo menos tamaño, fase y amplitud (los sustitutos válidos).
type Family = (u8, u8, u8, u8, i8, bool, u8);

fn family(k: &SKey) -> Family {
    (k.species, k.pattern, k.c1, k.c2, k.tint, k.dying, k.style)
}

#[derive(Default)]
pub struct GpuCache {
    pal: String,
    font: String,
    ds: f64,
    size: (i32, i32),
    sprites: HashMap<SKey, GpuSprite>,
    /// Sprites que existen, por familia: para elegir rápido el sustituto más parecido.
    index: HashMap<Family, Vec<SKey>>,
    /// Pedidos al hilo de sprites y en qué fotograma se pidieron.
    pending: HashMap<SKey, u32>,
    worker: Option<Worker>,
    /// Sprites terminados aún no subidos (se suben pocos por fotograma: cada textura nueva cuesta
    /// una subida a la GPU en el fotograma en que aparece).
    inbox: Vec<Done>,
    /// Sube cada vez que se vacía la caché: los sprites de otra generación se descartan.
    era: u32,
    frame: u32,
    bytes: usize,
    rays: Option<gdk::MemoryTexture>,
    scan: Option<gdk::MemoryTexture>,
    caustic: Option<gdk::MemoryTexture>,
    sand: Option<(gdk::MemoryTexture, f32)>,
    /// Agua del estilo Pixel (franjas y tramado), por tono.
    pixel_water: Option<(i32, gdk::MemoryTexture)>,
    decor: HashMap<DKey, DecorTex>,
    decor_bytes: usize,
    dpending: HashMap<DKey, u32>,
    decor_budget: u32,
    layouts: HashMap<(String, u32), pango::Layout>,
    budget: u32,
    style: Option<Style>,
    /// Capas y sprites del estilo realista.
    pub(crate) real: RealGpu,
}

impl GpuCache {
    pub fn sprite_count(&self) -> usize {
        self.sprites.len()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    fn clear_sprites(&mut self) {
        self.sprites.clear();
        self.index.clear();
        self.pending.clear();
        self.decor.clear();
        self.decor_bytes = 0;
        self.dpending.clear();
        self.inbox.clear();
        self.bytes = 0;
        self.era = self.era.wrapping_add(1);
    }

    fn worker(&mut self, pal: &crate::theme::Palette, ds: f64) -> &Worker {
        self.worker.get_or_insert_with(|| Worker::spawn(pal, ds))
    }

    fn insert_sprite(&mut self, key: SKey, tex: gdk::MemoryTexture, wp: i32, hp: i32, ox: f64, oy: f64, len: f64, ds: f64) {
        let bytes = (wp * hp * 4) as usize;
        self.bytes += bytes;
        // Los sprites de píxeles no llevan escala de dispositivo: se amplían por su factor entero.
        let k = if key.style == 1 { key.size as f64 } else { 1.0 / ds };
        self.sprites.insert(key, GpuSprite { tex, w: (wp as f64 * k) as f32, h: (hp as f64 * k) as f32, ox: ox as f32, oy: oy as f32, len, used: self.frame, bytes });
        self.index.entry(family(&key)).or_default().push(key);
    }

    /// Si la caché pesa demasiado, suelta los sprites menos usados recientemente hasta volver a
    /// tres cuartos del límite (nunca se vacía de golpe: eso hacía desaparecer a todos los peces).
    fn evict(&mut self) {
        // La decoración se cuenta aparte (sus bytes no están en `sprites`): si se pasa, se
        // suelta entera y se rehace a dos piezas por fotograma.
        if self.decor_bytes > MAX_DECOR_BYTES {
            self.decor.clear();
            self.decor_bytes = 0;
            self.dpending.clear();
        }
        if self.bytes <= MAX_SPRITE_BYTES {
            return;
        }
        if std::env::var_os("TA_PROF").is_some() {
            eprintln!("expulsión: {} sprites · {} MB", self.sprites.len(), self.bytes >> 20);
        }
        let frame = self.frame;
        let mut by_age: Vec<(u32, SKey, usize)> = self.sprites.iter().map(|(k, s)| (frame.wrapping_sub(s.used), *k, s.bytes)).collect();
        by_age.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        let target = MAX_SPRITE_BYTES * 3 / 4;
        for (_, k, b) in by_age {
            if self.bytes <= target {
                break;
            }
            self.sprites.remove(&k);
            self.bytes = self.bytes.saturating_sub(b);
        }
        self.index.clear();
        for k in self.sprites.keys() {
            self.index.entry(family(k)).or_default().push(*k);
        }
    }

    /// Recoge los sprites que el hilo haya terminado.
    fn collect(&mut self, ds: f64) {
        let Some(w) = &self.worker else { return };
        self.inbox.extend(w.poll());
        if self.inbox.is_empty() {
            return;
        }
        let n = self.inbox.len().min(MAX_UPLOADS_PER_FRAME);
        let done: Vec<Done> = self.inbox.drain(..n).collect();
        if std::env::var_os("TA_PROF").is_some() && self.inbox.len() > 40 {
            eprintln!("sprites: {} en espera de subir · {} + {} pendientes", self.inbox.len(), self.pending.len(), self.dpending.len());
        }
        let tex_of = |px: Pixels| {
            let bytes = glib::Bytes::from_owned(px.data);
            gdk::MemoryTexture::new(px.w, px.h, gdk::MemoryFormat::B8g8r8a8Premultiplied, &bytes, px.stride as usize)
        };
        for d in done {
            match d {
                Done::Fish { key, era, px, ox, oy, len } => {
                    self.pending.remove(&key);
                    if era != self.era || self.sprites.contains_key(&key) {
                        continue;
                    }
                    let (wp, hp) = (px.w, px.h);
                    self.insert_sprite(key, tex_of(px), wp, hp, ox, oy, len, ds);
                }
                Done::Decor { key, era, px, w, h, ox, oy, size } => {
                    self.dpending.remove(&key);
                    if era != self.era || self.decor.contains_key(&key) {
                        continue;
                    }
                    self.decor_bytes += (px.w * px.h * 4) as usize;
                    self.decor.insert(key, DecorTex { tex: tex_of(px), w: w as f32, h: h as f32, ox: ox as f32, oy: oy as f32, size });
                }
            }
        }
    }
}

const MAX_SPRITE_BYTES: usize = 160 << 20;
/// Texturas nuevas por fotograma (sprites recién pintados).
const MAX_UPLOADS_PER_FRAME: usize = 12;
const MAX_DECOR_BYTES: usize = 48 << 20;

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
    cache.frame = cache.frame.wrapping_add(1);
    let prof = std::env::var_os("TA_PROF").is_some();
    if cache.style != Some(v.style) {
        if prof {
            eprintln!("caché vaciada: cambio de estilo");
        }
        cache.clear_sprites();
        cache.real.clear();
        cache.style = Some(v.style);
    }
    if cache.pal != v.pal.raw || (cache.ds - ds).abs() > 0.01 {
        if prof {
            eprintln!("caché vaciada: paleta {} · ds {}→{}", cache.pal != v.pal.raw, cache.ds, ds);
        }
        // Otro tema de Omarchy u otra escala: todo se recalcula.
        cache.real.clear();
        cache.clear_sprites();
    }
    if cache.pal != v.pal.raw || (cache.ds - ds).abs() > 0.01 || cache.size != size {
        // Los sprites de los peces no dependen del tamaño de la pecera: solo se rehacen las capas
        // del fondo (la cabecera puede cambiar unos píxeles el tamaño cada segundo).
        cache.rays = None;
        cache.scan = None;
        cache.caustic = None;
        cache.sand = None;
        cache.pixel_water = None;
        cache.pal = v.pal.raw.clone();
        cache.ds = ds;
        cache.size = size;
        cache.worker(v.pal, ds).set_env(v.pal, ds);
    }
    if cache.font != v.font {
        cache.layouts.clear();
        cache.font = v.font.to_string();
    }
    cache.evict();
    cache.collect(ds);
    // Renderizado síncrono solo de emergencia (un pez sin ningún sprite parecido): el resto lo
    // hace el hilo de sprites.
    cache.budget = 1;
    cache.decor_budget = 1;
    if v.style == Style::Realistic {
        if (cache.ds - ds).abs() > 0.01 {
            cache.real.clear();
        }
        crate::reef::gpu_scene::render(widget, snap, v, cache, ds);
        return;
    }
    if v.style == Style::Sonar {
        sonar_render(widget, snap, v, cache, ds);
        return;
    }

    let mut laps = draw::Laps::new("tank");
    let px = v.style == Style::Pixel;
    draw::set_cell(t.scale());
    let cell = draw::cell();
    let full = rect(0.0, 0.0, w, h);
    let (top_c, bot_c) = draw::water_colors(v);
    let b = t.boil;

    // Agua: degradado, o franjas planas con tramado en el estilo Pixel (una textura a una celda
    // por píxel, horneada por tono del agua: el tramado son más de mil celdas por fotograma).
    if px {
        let wkey = (b * 20.0).round() as i32;
        if cache.pixel_water.as_ref().map(|c| c.0) != Some(wkey) {
            let tex = bake_px(w, h, cell, |cr| {
                cr.set_antialias(cairo::Antialias::None);
                for (y0, y1, c) in crate::pixel::water_bands(v) {
                    cr.set_source_rgb(c.0, c.1, c.2);
                    cr.rectangle(0.0, y0, w, y1 - y0 + cell);
                    let _ = cr.fill();
                }
                for (x, y, c) in crate::pixel::dither_cells(v) {
                    cr.set_source_rgb(c.0, c.1, c.2);
                    cr.rectangle(x, y, cell, cell);
                    let _ = cr.fill();
                }
            });
            cache.pixel_water = tex.map(|t| (wkey, t));
        }
        if let Some((_, tex)) = &cache.pixel_water {
            snap.append_scaled_texture(tex, gsk::ScalingFilter::Nearest, &full);
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

    laps.lap("agua");
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
    // Cáusticas sobre la arena (estilizado): baldosa repetida que se desplaza, recortada a la arena.
    if !px {
        if cache.caustic.is_none() {
            cache.caustic = texture_of(crate::reef::noise::caustic_tile(256, 31));
        }
        if let Some(tile) = &cache.caustic {
            let y0 = h * 0.905;
            snap.push_clip(&rect(0.0, y0, w, h - y0));
            snap.push_opacity(0.13);
            let (sx, sy) = (3.0, 1.4);
            let (ox, oy) = ((t.t * 9.0).rem_euclid(256.0 * sx), (t.t * 4.0).rem_euclid(256.0 * sy));
            snap.save();
            snap.translate(&pt(-ox - 256.0 * sx, -oy - 256.0 * sy));
            snap.scale(sx as f32, sy as f32);
            let child = rect(0.0, 0.0, 256.0, 256.0);
            snap.push_repeat(&rect(0.0, 0.0, w / sx + 3.0 * 256.0, h / sy + 3.0 * 256.0), Some(&child));
            snap.append_texture(tile, &child);
            snap.pop();
            snap.restore();
            snap.pop();
            snap.pop();
        }
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

    laps.lap("fondo");
    decor(snap, cache, v, ds);
    laps.lap("decor");

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
    laps.lap("peces");
    if px {
        pixel_bubbles(snap, v, top_c);
    } else {
        bubbles(snap, v, top_c);
    }
    laps.lap("burbujas");

    // Superficie: una franja fina que sí se repinta cada fotograma (en Pixel, a una celda por píxel).
    let strip = t.surface_y() + 30.0 * t.scale();
    if px {
        if let Some(tex) = bake_px(w, strip, cell, |cr| crate::pixel::surface(cr, v)) {
            snap.append_scaled_texture(&tex, gsk::ScalingFilter::Nearest, &rect(0.0, 0.0, w, strip));
        }
    } else {
        let cr = snap.append_cairo(&rect(0.0, 0.0, w, strip));
        draw::surface(&cr, v);
    }
    laps.lap("superficie");

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

    laps.lap("velos");
    labels(snap, widget, v, cache);
    laps.lap("textos");
    laps.done(60);
}

// ---------------------------------------------------------------- sonar

/// Escena del estilo Sonar con nodos GSK: fondo, rejilla, barrido, arena y superficie como tiras
/// Cairo finas, peces con sus sprites de eco, burbujas como anillos.
fn sonar_render(widget: &gtk::Widget, snap: &gtk::Snapshot, v: &View, cache: &mut GpuCache, ds: f64) {
    use crate::sonar;
    let t = v.tank;
    let p = v.pal;
    let (w, h) = (t.w, t.h);
    let lc = sonar::line_color(p, t.boil);
    let full = rect(0.0, 0.0, w, h);
    snap.append_color(&col(p.dbg, 1.0), &full);
    snap.append_linear_gradient(&full, &pt(0.0, 0.0), &pt(0.0, h), &stops(&[(0.0, lc, 0.0), (1.0, lc, 0.06 + 0.16 * t.boil)]));
    for (x0, y0, x1, y1, bright) in sonar::grid_lines(t) {
        let c = col(lc, if bright { 0.16 } else { 0.07 });
        if (x1 - x0).abs() < 0.5 {
            snap.append_color(&c, &rect(x0, 0.0, 1.0, h));
        } else {
            snap.append_color(&c, &rect(0.0, y0, w, 1.0));
        }
        let _ = (y1, x1);
    }
    // Líneas de barrido: una textura de 1×3 repetida.
    if cache.scan.is_none() {
        cache.scan = bake(1.0, 3.0, 1.0, |cr| {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.18);
            cr.rectangle(0.0, 0.0, 1.0, 1.0);
            let _ = cr.fill();
        });
    }
    if let Some(tex) = &cache.scan {
        snap.push_repeat(&full, Some(&rect(0.0, 0.0, 1.0, 3.0)));
        snap.append_texture(tex, &rect(0.0, 0.0, 1.0, 3.0));
        snap.pop();
    }
    // Anillos de alcance, estela del barrido (degradado cónico) y brazo.
    let (cx, cy, ang) = sonar::sweep(t);
    for k in 1..=4 {
        let r = h * 1.25 * k as f64 / 4.0;
        ring(snap, &rect(cx - r, cy - r, 2.0 * r, 2.0 * r), r as f32, 1.0, &col(lc, 0.12));
    }
    snap.append_conic_gradient(&full, &pt(cx, cy), ang.to_degrees() as f32, &stops(&[(0.0, lc, 0.0), (0.76, lc, 0.0), (1.0, lc, 0.42)]));
    snap.save();
    snap.translate(&pt(cx, cy));
    snap.rotate(ang.to_degrees() as f32);
    snap.append_color(&col(lc, 0.9), &rect(-0.75, -h * 1.5, 1.5, h * 1.5));
    snap.restore();
    for m in &t.motes {
        snap.append_color(&col(lc, 0.08 + 0.25 * m.z), &rect(m.x.round(), m.y.round(), 1.0, 1.0));
    }
    // Arena: tira Cairo con el perfil y su halo.
    {
        let y0 = h * 0.86;
        let cr = snap.append_cairo(&rect(0.0, y0, w, h - y0));
        sonar::floor_path(&cr, t);
        cr.line_to(w, h);
        cr.line_to(0.0, h);
        cr.close_path();
        cr.set_source_rgba(lc.0, lc.1, lc.2, 0.10);
        let _ = cr.fill();
        sonar::floor_path(&cr, t);
        draw::glow_stroke(&cr, lc, 120.0);
    }
    decor(snap, cache, v, ds);
    if let Some(Pick::Fish(hi)) = v.hover
        && let Some(hf) = t.fish.get(hi)
    {
        for f in t.fish.iter().filter(|f| f.kh == hf.kh && f.life == Life::Alive) {
            let r = f.len * 0.65;
            ring(snap, &rect(f.x - r, f.y - r, 2.0 * r, 2.0 * r), r as f32, 1.0, &col(lc, 0.5));
        }
    }
    snap.push_opacity(sonar::flicker(t));
    for &i in &t.order {
        fish(snap, cache, v, i, ds);
    }
    snap.pop();
    for b in &t.bubbles {
        match b.kind {
            BubbleKind::Air if b.r < 2.0 => snap.append_color(&col(lc, 0.5), &rect(b.x.round(), b.y.round(), 1.0, 1.0)),
            BubbleKind::Air => ring(snap, &rect(b.x - b.r, b.y - b.r, 2.0 * b.r, 2.0 * b.r), b.r as f32, 1.0, &col(lc, 0.55)),
            BubbleKind::Foam => snap.append_color(&col(lc, 0.5 * (b.life / b.max_life).clamp(0.0, 1.0)), &rect(b.x - b.r, b.y, 2.0 * b.r, 1.0)),
        }
    }
    {
        let strip = t.surface_y() + 30.0 * t.scale();
        let cr = snap.append_cairo(&rect(0.0, 0.0, w, strip));
        draw::surface_path(&cr, t);
        draw::glow_stroke(&cr, lc, 120.0);
    }
    let big = w.max(h) * 0.8;
    snap.append_radial_gradient(&full, &pt(w / 2.0, h / 2.0), big as f32, big as f32, (w.min(h) * 0.4 / big) as f32, 1.0, &stops(&[(0.0, (0.0, 0.0, 0.0), 0.0), (1.0, (0.0, 0.0, 0.0), 0.45)]));
    labels(snap, widget, v, cache);
}

// ---------------------------------------------------------------- decoración

/// Sprite de decoración `k`: si no existe se pide al hilo de sprites y se devuelve la fase más
/// cercana que ya exista. Solo si no hay ninguna de la pieza se pinta en el acto (una por fotograma).
fn ensure_decor(cache: &mut GpuCache, v: &View, d: &crate::sim::Decor, k: DKey, bs: f64, ds: f64) -> Option<DKey> {
    if cache.decor.contains_key(&k) {
        return Some(k);
    }
    let frame = cache.frame;
    let stale = cache.dpending.get(&k).is_some_and(|&fr| frame.wrapping_sub(fr) > 90);
    if (stale || !cache.dpending.contains_key(&k)) && cache.dpending.len() < 64 {
        let job = Job::Decor { key: k, d: Box::new(d.clone()), size: bs, style: v.style, cell: draw::cell(), era: cache.era };
        cache.worker(v.pal, ds).request(job);
        cache.dpending.insert(k, frame);
    }
    let np = draw::decor_phases(d.kind) as i32;
    let near = cache
        .decor
        .keys()
        .filter(|o| o.kind == k.kind && o.seed == k.seed && o.size == k.size)
        .min_by_key(|o| {
            let dp = (o.phase as i32 - k.phase as i32).abs();
            dp.min(np - dp) + (o.agit as i32 - k.agit as i32).abs() * 4
        })
        .copied();
    if near.is_some() || cache.decor_budget == 0 {
        return near;
    }
    cache.decor_budget -= 1;
    let r = draw::render_decor(d, &k, bs, v.style, draw::cell(), v.pal, ds)?;
    let (wp, hp) = (r.surf.width(), r.surf.height());
    let tex = texture_of(r.surf)?;
    cache.decor_bytes += (wp * hp * 4) as usize;
    cache.decor.insert(k, DecorTex { tex, w: r.w as f32, h: r.h as f32, ox: r.ox as f32, oy: r.oy as f32, size: r.size });
    cache.dpending.remove(&k);
    Some(k)
}

/// Decoración estilizada: sprites por fase de balanceo (una vez cada uno), mezclados de dos en dos
/// en la GPU para que el movimiento sea continuo. En Pixel no se mezcla: salta de fase en fase.
fn decor(snap: &gtk::Snapshot, cache: &mut GpuCache, v: &View, ds: f64) {
    let t = v.tank;
    let px = v.style == Style::Pixel;
    for (i, d) in t.decor.iter().enumerate() {
        let np = draw::decor_phases(d.kind);
        let pose = draw::decor_pose(t, d, false);
        let (b, bs) = crate::reef::decor::bucket(d.size);
        let agit = (t.boil * 3.0).round().clamp(0.0, 3.0) as u8;
        let f = pose.phase / (2.0 * std::f64::consts::PI) * np as f64;
        let (i0, mixp) = (f.floor() as usize % np, f - f.floor());
        let k0 = DKey { kind: d.kind as u8, seed: d.seed, size: b, phase: i0 as u8, agit };
        let Some(k0) = ensure_decor(cache, v, d, k0, bs, ds) else { continue };
        let k1 = (np > 1 && !px).then(|| DKey { phase: ((i0 + 1) % np) as u8, ..k0 }).and_then(|k| ensure_decor(cache, v, d, k, bs, ds));
        let (sx, sy) = if px { (draw::snap(pose.x), draw::snap(pose.y)) } else { (pose.x, pose.y) };
        let a = {
            let s0 = &cache.decor[&k0];
            (s0.w, s0.h, s0.ox, s0.oy, s0.size)
        };
        let sc = (d.size / a.4) as f32;
        let r = graphene::Rect::new(-a.2, -a.3, a.0, a.1);
        snap.save();
        snap.translate(&pt(sx, sy));
        snap.scale(sc, sc);
        match k1 {
            Some(k1) if k1 != k0 => {
                snap.push_cross_fade(mixp);
                snap.append_texture(&cache.decor[&k0].tex, &r);
                snap.pop();
                snap.append_texture(&cache.decor[&k1].tex, &r);
                snap.pop();
            }
            _ => {
                if px {
                    snap.append_scaled_texture(&cache.decor[&k0].tex, gsk::ScalingFilter::Nearest, &r);
                } else {
                    snap.append_texture(&cache.decor[&k0].tex, &r);
                }
            }
        }
        snap.restore();
        if v.hover == Some(Pick::Decor(i)) {
            let (bx, by, bw, bh) = draw::decor_box(t, d);
            outline(snap, &rect(bx, by, bw, bh), &col(v.pal.accent, 0.6));
        }
    }
}

// ---------------------------------------------------------------- peces

/// Sprite del pez `i`. Si aún no existe se pide al hilo de sprites y, mientras tanto, se usa el
/// más parecido que haya (misma familia: especie, patrón y colores; tamaño y fase más cercanos).
/// Solo si no hay ninguno de la familia se pinta en el acto, para que el pez no sea invisible.
fn sprite<'a>(cache: &'a mut GpuCache, v: &View, i: usize, ds: f64) -> Option<&'a GpuSprite> {
    let f = &v.tank.fish[i];
    let (key, blen) = draw::sprite_key(f, v.style);
    let frame = cache.frame;
    let sync = |cache: &mut GpuCache| {
        let spr = draw::render_sprite(&f.look, f.len, &key, blen, v.pal, ds);
        let (wp, hp) = (spr.surf.width(), spr.surf.height());
        let tex = texture_of(spr.surf)?;
        cache.insert_sprite(key, tex, wp, hp, spr.ox, spr.oy, spr.len, ds);
        cache.pending.remove(&key);
        Some(())
    };
    let chosen: Option<SKey> = if cache.sprites.contains_key(&key) {
        Some(key)
    } else if key.style == 1 {
        // Píxeles: dibujados a mano, baratos; en el acto.
        sync(cache).map(|_| key)
    } else {
        let stale = cache.pending.get(&key).is_some_and(|&fr| frame.wrapping_sub(fr) > 90);
        if (stale || !cache.pending.contains_key(&key)) && cache.pending.len() < 256 {
            let job = Job::Fish { key, look: f.look, len: f.len, blen, era: cache.era };
            cache.worker(v.pal, ds).request(job);
            cache.pending.insert(key, frame);
        }
        let np = draw::nphases(f.len) as i32;
        let near = cache.index.get(&family(&key)).and_then(|ks| {
            ks.iter()
                .min_by_key(|k| {
                    let dp = (k.phase as i32 - key.phase as i32).abs();
                    ((k.size - key.size).abs() as i32) * 16 + dp.min(np - dp) + (k.amp as i32 - key.amp as i32).abs() * 2
                })
                .copied()
        });
        match near {
            Some(k) => Some(k),
            None if cache.budget > 0 => {
                cache.budget -= 1;
                sync(cache).map(|_| key)
            }
            None => None,
        }
    };
    let k = chosen?;
    let s = cache.sprites.get_mut(&k)?;
    s.used = frame;
    Some(&*s)
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
    let mut alpha = f.alpha * draw::depth_alpha(f, v.style);
    if v.style == Style::Sonar {
        alpha *= 0.55 + 0.45 * crate::sonar::echo(v.tank, f.x, f.y);
    }
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

/// Layout de Pango cacheado por texto y tamaño: maquetar texto cuesta decenas de µs y los nombres
/// de los bancos se repiten fotograma tras fotograma.
fn layout(cache: &mut GpuCache, widget: &gtk::Widget, s: &str, font: &str, size: f64) -> pango::Layout {
    if cache.layouts.len() > 600 {
        cache.layouts.clear();
    }
    cache.layouts.entry((s.to_string(), (size * 10.0) as u32)).or_insert_with(|| text(widget, s, font, size)).clone()
}

pub(crate) fn labels(snap: &gtk::Snapshot, widget: &gtk::Widget, v: &View, cache: &mut GpuCache) {
    let t = v.tank;
    let p = v.pal;
    let plan = draw::plan_labels(v);
    let fs = plan.fs;
    for n in &plan.names {
        let l = layout(cache, widget, &n.text, v.font, fs);
        let (tw, _) = l.pixel_size();
        snap.append_color(&col(p.dbg, 0.55), &rect(n.x - tw as f64 / 2.0 - 5.0, n.y - fs - 1.0, tw as f64 + 10.0, fs + 6.0));
        put_text(snap, &l, n.x - tw as f64 / 2.0, n.y, draw::label_color(p));
    }
    if let Some(hid) = &plan.hidden {
        put_text(snap, &layout(cache, widget, hid, v.font, fs), 12.0, t.h - 12.0, mix(draw::label_color(p), (0.5, 0.6, 0.7), 0.35));
    }
    if plan.paused {
        let l = layout(cache, widget, crate::i18n::tr("paused"), v.font, fs + 3.0);
        let (tw, _) = l.pixel_size();
        put_text(snap, &l, t.w - tw as f64 - 14.0, t.surface_y() + fs + 12.0, p.yellow);
    }
    let Some(card) = &plan.card else { return };
    let layouts: Vec<pango::Layout> = card.lines.iter().map(|s| layout(cache, widget, s, v.font, fs)).collect();
    let tw = layouts.iter().map(|l| l.pixel_size().0).max().unwrap_or(0) as f64;
    let (bx, by, bw, bh, lh) = draw::card_rect(card, tw, fs, t);
    let r = rect(bx, by, bw, bh);
    snap.append_color(&col(p.dbg, 0.92), &r);
    ring(snap, &r, 0.0, 1.0, &col(p.accent, 0.9));
    for (i, l) in layouts.iter().enumerate() {
        put_text(snap, l, bx + 10.0, by + lh * (i as f64 + 1.0), if i == 0 { p.bfg } else { p.fg });
    }
}
