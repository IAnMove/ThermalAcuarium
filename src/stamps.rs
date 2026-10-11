//! Sellos: manchas blandas e irregulares (humo, nubes, espuma, resplandores) que se pintan con el
//! color actual. Cada sello se escala una sola vez por tamaño (cubos del 12 %) y aspecto a una
//! máscara A8, de modo que pintar una mancha es una composición sin transformar: la ruta rápida de
//! pixman. Con una matriz de escala en el patrón, Cairo iría por la ruta general (unas 10× más lenta).

use std::cell::RefCell;

use gtk::cairo::{self, Context, ImageSurface};

use crate::reef::noise;

pub struct Stamps {
    tiles: Vec<ImageSurface>,
    cache: RefCell<Vec<((i32, i32, i32), ImageSurface)>>,
}

impl Stamps {
    /// `n` variantes de mancha de 96 px, con semillas a partir de `seed`.
    pub fn new(n: usize, seed: u32) -> Stamps {
        Stamps { tiles: (0..n).map(|i| noise::blob_tile(96, seed + i as u32)).collect(), cache: RefCell::new(vec![]) }
    }

    /// Pinta la mancha `k` con el color actual, centrada en (x, y) y de semiejes (rx, ry).
    pub fn paint(&self, cr: &Context, k: usize, x: f64, y: f64, rx: f64, ry: f64) {
        let k = k % self.tiles.len();
        let rx = rx.max(1.0);
        let bucket = (rx.ln() / 1.12f64.ln()).round() as i32;
        let brx = 1.12f64.powi(bucket);
        let aspect = ((ry / rx) * 10.0).round().max(1.0) as i32;
        let key = (k as i32, bucket, aspect);
        let mut cache = self.cache.borrow_mut();
        if !cache.iter().any(|(kk, _)| *kk == key) {
            let bry = brx * aspect as f64 / 10.0;
            let (wp, hp) = ((2.0 * brx).ceil().max(2.0) as i32, (2.0 * bry).ceil().max(2.0) as i32);
            let s = ImageSurface::create(cairo::Format::A8, wp, hp).expect("sello");
            {
                let c = Context::new(&s).expect("cairo");
                c.scale(wp as f64 / 96.0, hp as f64 / 96.0);
                let p = cairo::SurfacePattern::create(&self.tiles[k]);
                p.set_filter(cairo::Filter::Bilinear);
                let _ = c.set_source(&p);
                let _ = c.paint();
            }
            cache.push((key, s));
            if cache.len() > 200 {
                cache.remove(0);
            }
        }
        let (_, s) = cache.iter().find(|(kk, _)| *kk == key).expect("sello");
        let _ = cr.mask_surface(s, (x - s.width() as f64 / 2.0).round(), (y - s.height() as f64 / 2.0).round());
    }
}

/// Baldosa repetible pre-escalada: el patrón que se devuelve solo lleva una traslación entera
/// (ruta rápida). `scale` se redondea al 1 %.
pub struct ScaledTiles {
    tile: ImageSurface,
    cache: RefCell<Vec<(i32, ImageSurface)>>,
}

impl ScaledTiles {
    pub fn new(tile: ImageSurface) -> ScaledTiles {
        ScaledTiles { tile, cache: RefCell::new(vec![]) }
    }

    pub fn pattern(&self, scale: f64, dx: f64, dy: f64) -> cairo::SurfacePattern {
        let k = (scale * 100.0).round() as i32;
        let mut cache = self.cache.borrow_mut();
        if !cache.iter().any(|(kk, _)| *kk == k) {
            let n0 = self.tile.width() as f64;
            let n = (n0 * scale).round().max(8.0) as i32;
            let s = ImageSurface::create(cairo::Format::ARgb32, n, n).expect("baldosa");
            {
                let c = Context::new(&s).expect("cairo");
                c.scale(n as f64 / n0, n as f64 / n0);
                let p = cairo::SurfacePattern::create(&self.tile);
                p.set_extend(cairo::Extend::Repeat);
                p.set_filter(cairo::Filter::Bilinear);
                let _ = c.set_source(&p);
                let _ = c.paint();
            }
            cache.push((k, s));
        }
        let (_, s) = cache.iter().find(|(kk, _)| *kk == k).expect("baldosa");
        let pat = cairo::SurfacePattern::create(s);
        pat.set_extend(cairo::Extend::Repeat);
        pat.set_filter(cairo::Filter::Fast);
        let mut m = cairo::Matrix::identity();
        m.translate(-dx.round(), -dy.round());
        pat.set_matrix(m);
        pat
    }
}
