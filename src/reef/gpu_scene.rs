//! Compositor de GPU del estilo realista (ver `gpu.rs`). Las capas estáticas se hornean con Cairo a
//! texturas una vez; cada fotograma solo se arma el árbol de nodos: agua, capas, cáusticas
//! (baldosa repetida con máscara de alfa), decoración que se mece (mezcla de dos fases), peces,
//! burbujas con textura, atenuación, superficie y cristal.

use std::collections::HashMap;
use std::time::Instant;

use gtk::prelude::*;
use gtk::{gdk, graphene, gsk};

use super::decor::{self, DKey};
use super::scene::{self, Layer};
use crate::draw::View;
use crate::gpu::{GpuCache, col, pt, rect, stops, texture_of};
use crate::sim::{BubbleKind, Pick};

struct LayerTex {
    tex: gdk::MemoryTexture,
    /// Rectángulo lógico que cubre.
    r: (f64, f64, f64, f64),
}

struct DecorTex {
    tex: gdk::MemoryTexture,
    w: f32,
    h: f32,
    ox: f32,
    oy: f32,
    size: f64,
}

#[derive(Default)]
pub struct RealGpu {
    /// (ancho, alto, ds*100) con el que se hornearon las capas.
    key: (i32, i32, i32),
    /// Tamaño al que se quiere re-hornear y desde cuándo (para no hornear mientras se redimensiona).
    pending: Option<((i32, i32, i32), Instant)>,
    far: Option<LayerTex>,
    rays: Option<LayerTex>,
    rocks: Option<LayerTex>,
    sand: Option<LayerTex>,
    glass: Option<LayerTex>,
    caustic: Option<gdk::MemoryTexture>,
    decor: HashMap<DKey, DecorTex>,
    bubbles: Vec<Option<(gdk::MemoryTexture, f32)>>,
}

impl RealGpu {
    pub fn clear(&mut self) {
        *self = RealGpu::default();
    }
}

fn wrap(l: Layer) -> Option<LayerTex> {
    let r = (l.x, l.y, l.w, l.h);
    texture_of(l.surf).map(|tex| LayerTex { tex, r })
}

fn bake_all(g: &mut RealGpu, v: &View, ds: f64) {
    let ly = scene::bake(v.tank, ds, &scene::Water::from(v.pal));
    g.far = wrap(ly.far);
    g.rays = wrap(ly.rays);
    g.rocks = wrap(ly.rocks);
    g.sand = wrap(ly.sand);
    g.glass = wrap(ly.glass);
    g.caustic = texture_of(ly.caustic);
}

fn put(snap: &gtk::Snapshot, l: &Option<LayerTex>, stretch: (f64, f64)) {
    if let Some(l) = l {
        // Si las capas son de otro tamaño (se está redimensionando) se estiran un momento.
        let (x, y, w, h) = l.r;
        snap.append_texture(&l.tex, &rect(x * stretch.0, y * stretch.1, w * stretch.0, h * stretch.1));
    }
}

pub fn render(widget: &gtk::Widget, snap: &gtk::Snapshot, v: &View, cache: &mut GpuCache, ds: f64) {
    let t = v.tank;
    let (w, h) = (t.w, t.h);
    let b = t.boil;
    let want = (w as i32, h as i32, (ds * 100.0) as i32);

    // Hornear (o re-hornear tras 300 ms sin cambiar de tamaño).
    {
        let g = &mut cache.real;
        if g.far.is_none() {
            bake_all(g, v, ds);
            g.key = want;
            g.pending = None;
        } else if g.key != want {
            match g.pending {
                Some((k, since)) if k == want => {
                    if since.elapsed().as_millis() > 300 {
                        bake_all(g, v, ds);
                        g.key = want;
                        g.pending = None;
                        g.decor.clear();
                    }
                }
                _ => g.pending = Some((want, Instant::now())),
            }
        }
    }
    let stretch = (w / cache.real.key.0.max(1) as f64, h / cache.real.key.1.max(1) as f64);
    let full = rect(0.0, 0.0, w, h);

    // 1. Agua.
    let wt = scene::Water::from(v.pal);
    let ws: Vec<_> = wt.stops(b).iter().map(|&(o, c)| (o as f32, c, 1.0)).collect();
    snap.append_linear_gradient(&full, &pt(0.0, 0.0), &pt(0.0, h), &stops(&ws));

    // 2. Arrecife lejano y rayos de luz.
    put(snap, &cache.real.far, stretch);
    snap.save();
    snap.translate(&pt((t.t * 0.15).sin() * w * 0.03, 0.0));
    snap.push_opacity((1.0 - 0.5 * b) * (0.85 + 0.15 * (t.t * 0.5).sin()));
    put(snap, &cache.real.rays, stretch);
    snap.pop();
    snap.restore();

    // 3. Nieve marina.
    for m in &t.motes {
        let tw = 0.6 + 0.4 * ((t.t * 1.3 + m.x * 0.05).sin() * 0.5 + 0.5);
        let r = 0.6 + 1.5 * m.z;
        snap.append_color(&col((0.80, 0.93, 1.0), (0.05 + 0.20 * m.z) * tw), &rect(m.x - r, m.y - r, 2.0 * r, 2.0 * r));
    }

    // 4. Roca, resplandor del fondo, arena y cáusticas.
    put(snap, &cache.real.rocks, stretch);
    if b > 0.01 {
        let fy = h * 0.915;
        let gh = h * 0.55;
        let glow = (1.0, 0.55, 0.16);
        snap.append_linear_gradient(&rect(0.0, fy - gh, w, gh + h * 0.1), &pt(0.0, fy), &pt(0.0, fy - gh), &stops(&[(0.0, glow, 0.62 * b), (1.0, glow, 0.0)]));
    }
    put(snap, &cache.real.sand, stretch);
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
                &stops(&[(0.0, (1.0, 0.80, 0.40), 0.5 * b), (1.0, (1.0, 0.55, 0.15), 0.0)]),
            );
        }
    }
    caustics(snap, cache, v, stretch);

    // 5. Decoración.
    decor_pass(snap, cache, v, ds);

    // Los compañeros de banco del pez señalado se iluminan suavemente.
    if let Some(Pick::Fish(hi)) = v.hover
        && let Some(hf) = t.fish.get(hi) {
            for f in t.fish.iter().filter(|f| f.kh == hf.kh && f.life == crate::sim::Life::Alive) {
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

    // 6. Sombras sobre la arena, peces y burbujas.
    for f in t.fish.iter().filter(|f| f.life == crate::sim::Life::Alive) {
        let fy = t.floor_y(f.x);
        let k = (1.0 - (fy - f.y) / (h * 0.7)).clamp(0.0, 1.0);
        if k < 0.05 {
            continue;
        }
        let (rx, ry) = (f.len * 0.45, f.len * 0.09 + 2.0);
        snap.append_radial_gradient(
            &rect(f.x - rx, fy + 2.0 - ry, 2.0 * rx, 2.0 * ry),
            &pt(f.x, fy + 2.0),
            rx as f32,
            ry as f32,
            0.0,
            1.0,
            &stops(&[(0.0, (0.0, 0.0, 0.02), 0.32 * k * f.alpha), (1.0, (0.0, 0.0, 0.02), 0.0)]),
        );
    }
    for &i in &t.order {
        crate::gpu::fish(snap, cache, v, i, ds);
    }
    bubbles(snap, cache, v, ds);

    // 7. Atenuación con la profundidad.
    snap.append_linear_gradient(
        &full,
        &pt(0.0, 0.0),
        &pt(0.0, h),
        &stops(&[(0.0, wt.top, 0.0), (0.6, wt.bot, 0.10), (1.0, wt.bot, 0.36)]),
    );

    // 8. Superficie (franja fina que se repinta cada fotograma).
    {
        let strip = t.surface_y() + 34.0 * t.scale();
        let cr = snap.append_cairo(&rect(0.0, 0.0, w, strip));
        scene::surface(&cr, t, &wt);
    }

    // 9. Turbidez, calor y cristal.
    let k = ((t.ram_frac - 0.5) / 0.5).clamp(0.0, 1.0);
    if k > 0.01 {
        snap.append_color(&col((0.18, 0.30, 0.30), 0.34 * k * k), &rect(0.0, t.surface_y(), w, h));
    }
    if b > 0.3 {
        let hot = (1.0, 0.45, 0.12);
        let r = w.max(h) * 0.85;
        snap.append_radial_gradient(
            &full,
            &pt(w / 2.0, h * 1.05),
            r as f32,
            r as f32,
            (h * 0.3 / r) as f32,
            1.0,
            &stops(&[(0.0, hot, 0.0), (1.0, hot, 0.26 * b)]),
        );
    }
    put(snap, &cache.real.glass, stretch);

    crate::gpu::labels(snap, widget, v, cache);
}

/// Cáusticas: la baldosa se repite a dos escalas y rumbos, y una máscara de alfa (roca y arena, con
/// un fundido vertical) hace que solo se vean sobre superficies, como en la realidad.
fn caustics(snap: &gtk::Snapshot, cache: &GpuCache, v: &View, stretch: (f64, f64)) {
    let (Some(tile), Some(_)) = (&cache.real.caustic, &cache.real.sand) else { return };
    let t = v.tank;
    let (w, h) = (t.w, t.h);
    let top = h * 0.40;
    snap.push_clip(&rect(0.0, top, w, h - top));
    snap.push_mask(gsk::MaskMode::Alpha);
    // Máscara: roca y arena con un fundido (más luz hacia el fondo).
    snap.push_mask(gsk::MaskMode::Alpha);
    snap.append_linear_gradient(&rect(0.0, top, w, h - top), &pt(0.0, top), &pt(0.0, h), &stops(&[(0.0, (0.0, 0.0, 0.0), 0.35), (1.0, (0.0, 0.0, 0.0), 1.0)]));
    snap.pop();
    put(snap, &cache.real.rocks, stretch);
    put(snap, &cache.real.sand, stretch);
    snap.pop(); // fin de la máscara interior (roca y arena × fundido)
    snap.pop(); // fin de la máscara exterior: lo que sigue es la fuente
    // Fuente: dos copias de la baldosa a distinta escala y rumbo.
    let tile_px = 256.0;
    for (scale, vx, vy, a) in [(2.8, 14.0, 6.0, 0.52), (2.0, -9.0, 10.0, 0.40)] {
        let size = tile_px * scale;
        let ox = (t.t * vx).rem_euclid(size);
        let oy = (t.t * vy).rem_euclid(size);
        snap.push_opacity(a * (0.85 + 0.15 * (t.t * 1.7).sin()));
        snap.save();
        snap.translate(&pt(-ox - size, -oy - size));
        snap.scale(scale as f32, scale as f32);
        let bounds = rect(0.0, 0.0, (w / scale) + 3.0 * tile_px, (h / scale) + 3.0 * tile_px);
        let child = rect(0.0, 0.0, tile_px, tile_px);
        snap.push_repeat(&bounds, Some(&child));
        snap.append_texture(tile, &child);
        snap.pop();
        snap.restore();
        snap.pop();
    }
    snap.pop(); // fin de la fuente: se aplica la máscara
    snap.pop(); // fin del recorte
}

/// Asegura que el sprite de `k` está en la caché; false si no se pudo crear.
fn ensure_decor(cache: &mut GpuCache, k: &DKey, size: f64, ds: f64) -> bool {
    if cache.real.decor.contains_key(k) {
        return true;
    }
    let spr = decor::render(k, size, ds);
    let (wp, hp) = (spr.surf.width(), spr.surf.height());
    let Some(tex) = texture_of(spr.surf) else { return false };
    cache.real.decor.insert(*k, DecorTex { tex, w: (wp as f64 / ds) as f32, h: (hp as f64 / ds) as f32, ox: spr.ox as f32, oy: spr.oy as f32, size: spr.size });
    true
}

fn decor_pass(snap: &gtk::Snapshot, cache: &mut GpuCache, v: &View, ds: f64) {
    let t = v.tank;
    for (i, d) in t.decor.iter().enumerate() {
        let kind = decor::kind_of(d);
        let np = decor::phases(kind);
        let agit = t.boil > 0.35;
        let x = d.xf * t.w;
        let base = t.floor_y(x) + 5.0;
        let f = (t.t * (0.16 + 0.12 * t.boil)).rem_euclid(1.0) * np as f64;
        let (i0, mixp) = (f.floor() as usize % np, f - f.floor());
        let (k0, bs) = decor::key(d, i0, agit);
        // Sprite de la fase siguiente para mezclar (solo las piezas blandas).
        let k1 = (np > 1).then(|| decor::key(d, (i0 + 1) % np, agit).0);
        if !ensure_decor(cache, &k0, bs, ds) {
            continue;
        }
        let a = {
            let s = &cache.real.decor[&k0];
            (s.w, s.h, s.ox, s.oy, s.size)
        };
        let sc = (d.size / a.4) as f32;
        snap.save();
        snap.translate(&pt(x, base));
        snap.scale(sc, sc);
        let r = graphene::Rect::new(-a.2, -a.3, a.0, a.1);
        match k1 {
            Some(k1) => {
                snap.push_cross_fade(mixp);
                if let Some(s) = cache.real.decor.get(&k0) {
                    snap.append_texture(&s.tex, &r);
                }
                snap.pop();
                if ensure_decor(cache, &k1, bs, ds) {
                    snap.append_texture(&cache.real.decor[&k1].tex, &r);
                }
                snap.pop();
            }
            None => {
                if let Some(s) = cache.real.decor.get(&k0) {
                    snap.append_texture(&s.tex, &r);
                }
            }
        }
        snap.restore();
        if v.hover == Some(Pick::Decor(i)) {
            crate::gpu::outline(snap, &rect(x - d.size * 0.4, base - d.size, d.size * 0.8, d.size + 6.0), &col(v.pal.accent, 0.6));
        }
    }
}

fn bubbles(snap: &gtk::Snapshot, cache: &mut GpuCache, v: &View, ds: f64) {
    let g = &mut cache.real;
    if g.bubbles.len() < 35 {
        g.bubbles.resize_with(35, || None);
    }
    let dot = col((0.85, 0.96, 1.0), 0.45);
    for b in &v.tank.bubbles {
        match b.kind {
            BubbleKind::Air => {
                if b.r < 1.7 {
                    let r = b.r.max(0.7);
                    snap.append_color(&dot, &rect(b.x - r, b.y - r, 2.0 * r, 2.0 * r));
                    continue;
                }
                let k = scene::bubble_bucket(b.r);
                if g.bubbles[k].is_none() {
                    let surf = scene::bubble_sprite(k, ds);
                    let side = (surf.width() as f64 / ds) as f32;
                    g.bubbles[k] = texture_of(surf).map(|t| (t, side));
                }
                if let Some((tex, side)) = &g.bubbles[k] {
                    let off = *side as f64 / 2.0;
                    snap.append_texture(tex, &rect(b.x - off, b.y - off, *side as f64, *side as f64));
                }
            }
            BubbleKind::Foam => {
                let a = (b.life / b.max_life).clamp(0.0, 1.0).sqrt();
                crate::gpu::disc(snap, &rect(b.x - b.r, b.y - b.r, 2.0 * b.r, 2.0 * b.r), b.r as f32, &col((0.92, 0.98, 1.0), 0.38 * a));
            }
        }
    }
}
