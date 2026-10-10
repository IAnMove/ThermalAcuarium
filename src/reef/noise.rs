//! Ruido procedural para dar textura al estilo realista: piel de los peces, arena, rocas y las
//! redes de luz (cáusticas) que el sol dibuja en el fondo. Todo es determinista y se genera una vez.

use gtk::cairo;

pub fn hash2(x: i32, y: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA6B) ^ seed.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^ (h >> 15)
}

/// Número estable en [0, 1) para la celda (x, y).
pub fn rnd(x: i32, y: i32, seed: u32) -> f64 {
    hash2(x, y, seed) as f64 / 4_294_967_296.0
}

fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// Ruido de valor suave en [0, 1); si `period` > 0 el patrón se repite cada `period` celdas.
pub fn vnoise(x: f64, y: f64, seed: u32, period: i32) -> f64 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (smooth(x - x0), smooth(y - y0));
    let wrap = |v: i32| if period > 0 { v.rem_euclid(period) } else { v };
    let (ix, iy) = (x0 as i32, y0 as i32);
    let c = |dx: i32, dy: i32| rnd(wrap(ix + dx), wrap(iy + dy), seed);
    let top = c(0, 0) + (c(1, 0) - c(0, 0)) * fx;
    let bot = c(0, 1) + (c(1, 1) - c(0, 1)) * fx;
    top + (bot - top) * fy
}

/// Ruido fractal en [0, 1). `period` es el de la primera octava (cada octava lo duplica).
pub fn fbm(x: f64, y: f64, octaves: u32, seed: u32, period: i32) -> f64 {
    let (mut sum, mut amp, mut norm, mut f) = (0.0, 1.0, 0.0, 1.0);
    for o in 0..octaves {
        let p = if period > 0 { period * f as i32 } else { 0 };
        sum += amp * vnoise(x * f, y * f, seed.wrapping_add(o * 977), p);
        norm += amp;
        amp *= 0.5;
        f *= 2.0;
    }
    sum / norm
}

/// Ruido 1D (para perfiles: dunas, siluetas de roca).
pub fn fbm1(x: f64, octaves: u32, seed: u32) -> f64 {
    fbm(x, 0.37, octaves, seed, 0)
}

fn put(data: &mut [u8], stride: usize, x: usize, y: usize, rgba_premul: [u8; 4]) {
    let i = y * stride + x * 4;
    data[i..i + 4].copy_from_slice(&[rgba_premul[2], rgba_premul[1], rgba_premul[0], rgba_premul[3]]);
}

/// Baldosa de ruido gris, opaca y repetible, para mezclar con `SoftLight`/`Overlay` sobre texturas.
pub fn grey_tile(size: i32, cells: i32, octaves: u32, seed: u32, contrast: f64) -> cairo::ImageSurface {
    let mut surf = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size).expect("noise");
    let stride = surf.stride() as usize;
    {
        let mut data = surf.data().expect("noise data");
        for y in 0..size as usize {
            for x in 0..size as usize {
                let n = fbm(x as f64 / size as f64 * cells as f64, y as f64 / size as f64 * cells as f64, octaves, seed, cells);
                let v = (0.5 + (n - 0.5) * contrast).clamp(0.0, 1.0);
                let b = (v * 255.0) as u8;
                put(&mut data, stride, x, y, [b, b, b, 255]);
            }
        }
    }
    surf
}

/// Distancia a los dos puntos de rejilla más cercanos (ruido celular repetible).
fn worley(x: f64, y: f64, cells: i32, seed: u32) -> (f64, f64) {
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let (mut d1, mut d2) = (f64::MAX, f64::MAX);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (cx, cy) = (ix + dx, iy + dy);
            let (wx, wy) = (cx.rem_euclid(cells), cy.rem_euclid(cells));
            let px = cx as f64 + rnd(wx, wy, seed);
            let py = cy as f64 + rnd(wx, wy, seed ^ 0x55AA);
            let d = ((px - x).powi(2) + (py - y).powi(2)).sqrt();
            if d < d1 {
                d2 = d1;
                d1 = d;
            } else if d < d2 {
                d2 = d;
            }
        }
    }
    (d1, d2)
}

/// Baldosa de cáusticas repetible: la red de líneas brillantes que la luz dibuja al atravesar la
/// superficie ondulada. Blanco azulado con alfa (premultiplicado).
pub fn caustic_tile(size: i32, seed: u32) -> cairo::ImageSurface {
    let mut surf = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size).expect("caustics");
    let stride = surf.stride() as usize;
    {
        let mut data = surf.data().expect("caustics data");
        for y in 0..size as usize {
            for x in 0..size as usize {
                let (fx, fy) = (x as f64 / size as f64, y as f64 / size as f64);
                // Dos octavas de celdas: las aristas de las celdas son las líneas de luz.
                let mut b = 0.0;
                for (cells, wgt, s) in [(5, 0.75, seed), (9, 0.55, seed ^ 0x1234)] {
                    // Ligera deformación para que no parezca una rejilla de Voronoi.
                    let wx = fx * cells as f64 + 0.18 * (vnoise(fx * 6.0, fy * 6.0, s + 7, 6) - 0.5);
                    let wy = fy * cells as f64 + 0.18 * (vnoise(fx * 6.0 + 9.0, fy * 6.0, s + 8, 6) - 0.5);
                    let (d1, d2) = worley(wx, wy, cells, s);
                    let e = (d2 - d1).max(0.0);
                    b += wgt * (1.0 - (e / 0.085).min(1.0)).powf(2.6);
                }
                let a = (b * 0.9).clamp(0.0, 1.0);
                let (r, g, bl) = (0.80, 0.96, 1.0);
                put(&mut data, stride, x, y, [(r * a * 255.0) as u8, (g * a * 255.0) as u8, (bl * a * 255.0) as u8, (a * 255.0) as u8]);
            }
        }
    }
    surf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fbm1_varies_along_x() {
        let vals: Vec<f64> = (0..20).map(|i| fbm1(i as f64 * 0.3 + 11.0, 4, 11)).collect();
        let (mn, mx) = vals.iter().fold((1.0f64, 0.0f64), |(a, b), &v| (a.min(v), b.max(v)));
        eprintln!("fbm1 min {mn:.3} max {mx:.3} {vals:?}");
        assert!(mx - mn > 0.15, "fbm1 casi constante: {mn}..{mx}");
    }

    #[test]
    fn vnoise_in_unit_range() {
        for i in 0..200 {
            let v = vnoise(i as f64 * 0.37, 0.37, 5, 0);
            assert!((0.0..1.0).contains(&v), "{v}");
        }
    }
}

/// Mancha blanda y blanca de forma irregular (para humo, bruma, resplandores): alfa que cae
/// suavemente del centro al borde, deformada por ruido para que no se vea un círculo.
pub fn blob_tile(size: i32, seed: u32) -> cairo::ImageSurface {
    let mut surf = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size).expect("blob");
    let stride = surf.stride() as usize;
    {
        let mut data = surf.data().expect("blob data");
        let half = size as f64 / 2.0;
        for y in 0..size as usize {
            for x in 0..size as usize {
                let (dx, dy) = ((x as f64 - half) / half, (y as f64 - half) / half);
                let ang = dy.atan2(dx);
                let wobble = 0.72 + 0.28 * fbm(ang.cos() * 1.5 + 2.0, ang.sin() * 1.5 + 2.0, 3, seed, 0);
                let d = (dx * dx + dy * dy).sqrt() / wobble;
                let a = (1.0 - d).clamp(0.0, 1.0).powf(1.6) * (0.75 + 0.25 * fbm(dx * 3.0 + 5.0, dy * 3.0 + 5.0, 2, seed + 9, 0));
                let b = (a * 255.0) as u8;
                put(&mut data, stride, x, y, [b, b, b, b]);
            }
        }
    }
    surf
}
