//! Simulación de la pecera. Cada proceso real es un pez (los del mismo nombre nadan en banco),
//! los procesos protegidos de la sesión son decoración, y el hardware pone el agua:
//! la temperatura la hace hervir, la RAM la enturbia, la carga mueve la corriente y el
//! disco suelta burbujas.

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

use crate::sys::{Group, Proc};

pub const SPECIES: usize = 6;
/// Especies del estilo realista (ver `reef/species.rs`).
pub const REAL_SPECIES: usize = 15;
pub const DECOR_KINDS: usize = 4;
const MAX_FISH: usize = 180;
const MAX_BUBBLES: usize = 800;
/// Desde esta temperatura el agua empieza a burbujear; a `BOIL_FULL` hierve a borbotones.
const BOIL_FROM: f64 = 82.0;
const BOIL_FULL: f64 = 100.0;

// ---------------------------------------------------------------- utilidades

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    pub fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.next()
    }
    pub fn chance(&mut self, p: f64) -> bool {
        self.next() < p
    }
    pub fn sign(&mut self) -> f64 {
        if self.next() < 0.5 { -1.0 } else { 1.0 }
    }
    /// Aproximación a una normal centrada en 0 (desviación ~0.8).
    pub fn gauss(&mut self) -> f64 {
        (self.next() + self.next() + self.next() - 1.5) * 1.6
    }
}

pub fn hash_str(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

/// Número pseudoaleatorio estable en [0, 1) a partir de una semilla.
pub fn unit(x: u64) -> f64 {
    (mix64(x) >> 11) as f64 / (1u64 << 53) as f64
}

fn smooth(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Longitud en px de un pez con `mem` bytes de RAM. La escala es logarítmica: de 1 MB a 10 GB
/// hay cuatro órdenes de magnitud y en lineal solo se vería una ballena y nada más.
pub fn size_of(mem: f64, ram_total: f64) -> f64 {
    let lo = 1.5e6;
    let hi = (ram_total * 0.5).max(2e9);
    let t = ((mem.max(lo) / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0);
    22.0 + 170.0 * t.powf(1.5)
}

// ---------------------------------------------------------------- entidades

#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum Pattern {
    Plain,
    Bands,
    Spots,
    Stripe,
}

/// Aspecto de un pez, derivado del nombre del proceso (la especie y los colores son los
/// mismos para todo el banco; solo varía un poco el tono por PID).
#[derive(Clone, Copy)]
pub struct Look {
    pub species: usize,
    /// Especie de arrecife (estilo realista).
    pub real: usize,
    pub pattern: Pattern,
    pub c1: usize,
    pub c2: usize,
    pub tint: f64,
    pub seed: u64,
}

impl Look {
    fn new(kh: u64, pid: i32) -> Look {
        let s = mix64(kh);
        let c1 = ((s >> 8) % 8) as usize;
        let mut c2 = ((s >> 16) % 8) as usize;
        if c2 == c1 {
            c2 = (c2 + 3) % 8;
        }
        Look {
            species: (s % SPECIES as u64) as usize,
            real: (mix64(s ^ 0xA5A5_5A5A) % REAL_SPECIES as u64) as usize,
            pattern: match (s >> 24) % 10 {
                0..=2 => Pattern::Plain,
                3..=4 => Pattern::Bands,
                5..=6 => Pattern::Spots,
                _ => Pattern::Stripe,
            },
            c1,
            c2,
            tint: unit(kh ^ (pid as u64).wrapping_mul(0x9E3779B97F4A7C15)) * 2.0 - 1.0,
            seed: s,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Life {
    Alive,
    /// Cerrado por el usuario: panza arriba y a flote (segundos transcurridos).
    Dying(f64),
    /// El proceso terminó solo: el pez se disuelve en burbujas.
    Leaving,
}

pub struct Fish {
    pub pid: i32,
    pub start: u64,
    pub key: String,
    pub kh: u64,
    pub look: Look,
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
    /// Sentido de nado (±1), ángulo de cabeceo y volteo: `face` pasa suavemente de -1 a 1 al girar.
    pub dir: f64,
    pub face: f64,
    pub pitch: f64,
    pub flipv: f64,
    pub len: f64,
    base_len: f64,
    pub mem: f64,
    pub cpu: f64,
    pub wbps: f64,
    mem_goal: f64,
    cpu_goal: f64,
    wbps_goal: f64,
    pub phase: f64,
    pub amp: f64,
    phase0: f64,
    yoff: f64,
    /// Puesto dentro del banco: la formación (una espiral de girasol, alargada en el sentido de
    /// nado) sale de él, así que un banco grande es una nube ancha y no una columna.
    slot: usize,
    speed_var: f64,
    pub life: Life,
    pub alpha: f64,
    gone: bool,
    bub_acc: f64,
    dead: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum BubbleKind {
    Air,
    Foam,
}

pub struct Bubble {
    pub x: f64,
    pub y: f64,
    pub r: f64,
    vy: f64,
    wob: f64,
    wf: f64,
    wa: f64,
    pub life: f64,
    pub max_life: f64,
    pub kind: BubbleKind,
}

/// Proceso protegido: decoración fija en el fondo.
#[derive(Clone)]
pub struct Decor {
    pub pid: i32,
    pub start: u64,
    pub key: String,
    pub kind: usize,
    pub xf: f64,
    pub mem: f64,
    pub size: f64,
    pub seed: u64,
}

pub struct Mote {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Clone, Copy)]
struct GroupState {
    dir: f64,
    yf: f64,
    timer: f64,
    /// Tiempo que falta para que el banco pueda volver a girar al tocar una pared.
    cool: f64,
    cx: f64,
    n: usize,
    sx: f64,
    sy: f64,
    minx: f64,
    maxx: f64,
    lsum: f64,
}

impl GroupState {
    fn blank(dir: f64, yf: f64, timer: f64, cx: f64) -> Self {
        GroupState { dir, yf, timer, cool: 0.0, cx, n: 0, sx: 0.0, sy: 0.0, minx: 0.0, maxx: 0.0, lsum: 0.0 }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Pick {
    Fish(usize),
    Decor(usize),
}

// ---------------------------------------------------------------- pecera

pub struct Tank {
    pub w: f64,
    pub h: f64,
    pub t: f64,
    pub fish: Vec<Fish>,
    /// Índices de `fish` en orden de dibujo (de atrás a delante).
    pub order: Vec<usize>,
    pub decor: Vec<Decor>,
    pub bubbles: Vec<Bubble>,
    pub ripples: Vec<(f64, f64)>,
    pub motes: Vec<Mote>,
    pub temp: f64,
    temp_goal: f64,
    have_temp: bool,
    pub boil: f64,
    pub load: f64,
    pub ram_frac: f64,
    pub ram_total: f64,
    pub disk_w: f64,
    pub hidden: usize,
    fit: f64,
    groups: HashMap<u64, GroupState>,
    boil_acc: f64,
    foam_acc: f64,
    air_acc: f64,
    rng: Rng,
}

impl Tank {
    pub fn new(w: f64, h: f64, seed: u64) -> Tank {
        let mut rng = Rng::new(seed);
        let motes = (0..70)
            .map(|_| Mote { x: rng.range(0.0, w), y: rng.range(0.0, h * 0.9), z: rng.range(0.2, 1.0) })
            .collect();
        Tank {
            w,
            h,
            t: 0.0,
            fish: vec![],
            order: vec![],
            decor: vec![],
            bubbles: vec![],
            ripples: vec![],
            motes,
            temp: 40.0,
            temp_goal: 40.0,
            have_temp: false,
            boil: 0.0,
            load: 0.0,
            ram_frac: 0.0,
            ram_total: 16e9,
            disk_w: 0.0,
            hidden: 0,
            fit: 1.0,
            groups: HashMap::new(),
            boil_acc: 0.0,
            foam_acc: 0.0,
            air_acc: 0.0,
            rng,
        }
    }

    // --- geometría

    pub fn scale(&self) -> f64 {
        ((self.w * self.h).sqrt() / 820.0).clamp(0.55, 1.6)
    }

    pub fn surface_y(&self) -> f64 {
        self.h * 0.045
    }

    /// Altura de la arena en `x` (dunas suaves).
    pub fn floor_y(&self, x: f64) -> f64 {
        self.h * 0.915 + (x * 0.011).sin() * self.h * 0.007 + (x * 0.027 + 1.3).sin() * self.h * 0.004
    }

    /// Franja vertical por la que nadan los peces.
    fn band(&self) -> (f64, f64) {
        let sc = self.scale();
        (self.surface_y() + 16.0 * sc, self.h * 0.915 - self.h * 0.011 - 12.0 * sc)
    }

    pub fn vent_x(&self, i: usize) -> f64 {
        self.w * (0.12 + 0.19 * i as f64 + 0.03 * (self.t * 0.07 + i as f64).sin())
    }

    pub fn resize(&mut self, w: f64, h: f64) {
        if w < 2.0 || h < 2.0 || ((w - self.w).abs() < 0.5 && (h - self.h).abs() < 0.5) {
            return;
        }
        let (kx, ky) = (w / self.w, h / self.h);
        for f in &mut self.fish {
            f.x *= kx;
            f.y *= ky;
        }
        for b in &mut self.bubbles {
            b.x *= kx;
            b.y *= ky;
        }
        for m in &mut self.motes {
            m.x *= kx;
            m.y *= ky;
        }
        self.w = w;
        self.h = h;
    }

    /// Datos del hardware que ponen el agua. `temp` es la temperatura de la CPU.
    pub fn set_env(&mut self, temp: Option<f64>, load: f64, ram_used: f64, ram_total: f64, disk_w: f64) {
        if let Some(t) = temp {
            if !self.have_temp {
                self.temp = t;
            }
            self.temp_goal = t;
            self.have_temp = true;
        }
        self.load = load;
        self.ram_total = ram_total.max(1.0);
        self.ram_frac = (ram_used / self.ram_total).clamp(0.0, 1.0);
        self.disk_w = disk_w;
    }

    // --- sincronización con los procesos reales

    /// Reconcilia los peces con la lista real de procesos: crea los nuevos, actualiza los
    /// existentes y despide a los que ya no están. `first` coloca los peces ya dentro del agua.
    pub fn sync(&mut self, groups: &[Group], first: bool) {
        let mut fish_procs: Vec<(u64, &str, &Proc)> = vec![];
        let mut deco_procs: Vec<(u64, &Proc)> = vec![];
        for g in groups {
            let kh = hash_str(&g.key);
            for p in &g.procs {
                if g.protected {
                    deco_procs.push((kh, p));
                } else {
                    fish_procs.push((kh, g.key.as_str(), p));
                }
            }
        }
        fish_procs.sort_by(|a, b| b.2.mem.partial_cmp(&a.2.mem).unwrap_or(std::cmp::Ordering::Equal));
        self.hidden = fish_procs.len().saturating_sub(MAX_FISH);
        fish_procs.truncate(MAX_FISH);

        let known: HashMap<(i32, u64), usize> =
            self.fish.iter().enumerate().map(|(i, f)| ((f.pid, f.start), i)).collect();
        let mut seen = vec![false; self.fish.len()];
        let mut shoal: HashMap<u64, usize> = HashMap::new();
        for (kh, _, _) in &fish_procs {
            *shoal.entry(*kh).or_default() += 1;
        }
        for (kh, key, p) in fish_procs {
            if let Some(&i) = known.get(&(p.pid, p.start)) {
                seen[i] = true;
                let f = &mut self.fish[i];
                f.mem_goal = p.mem;
                f.cpu_goal = p.cpu;
                f.wbps_goal = p.wbps;
                f.gone = false;
                if f.life == Life::Leaving {
                    f.life = Life::Alive;
                }
            } else {
                self.spawn_fish(kh, key, p, first, shoal[&kh]);
            }
        }
        for (i, f) in self.fish.iter_mut().enumerate() {
            if i < seen.len() && !seen[i] {
                f.gone = true;
                if f.life == Life::Alive {
                    f.life = Life::Leaving;
                }
            }
        }

        let mut deco_seen = HashSet::new();
        for (kh, p) in deco_procs {
            deco_seen.insert((p.pid, p.start));
            if let Some(d) = self.decor.iter_mut().find(|d| d.pid == p.pid && d.start == p.start) {
                d.mem = p.mem;
            } else {
                let h = kh ^ (p.pid as u64).wrapping_mul(0x9E3779B97F4A7C15);
                self.decor.push(Decor {
                    pid: p.pid,
                    start: p.start,
                    key: groups.iter().find(|g| hash_str(&g.key) == kh).map_or(String::new(), |g| g.key.clone()),
                    kind: ((mix64(kh) >> 40) % DECOR_KINDS as u64) as usize,
                    xf: 0.05 + 0.9 * unit(h),
                    mem: p.mem,
                    size: 0.0,
                    seed: mix64(h),
                });
            }
        }
        self.decor.retain(|d| deco_seen.contains(&(d.pid, d.start)));
        let (rt, sc) = (self.ram_total, self.scale());
        for d in &mut self.decor {
            d.size = decor_size(d.mem, rt) * sc;
        }
        self.reorder();
    }

    fn spawn_fish(&mut self, kh: u64, key: &str, p: &Proc, first: bool, shoal: usize) {
        let sc = self.scale();
        let (top, bot) = self.band();
        if !self.groups.contains_key(&kh) {
            let g = GroupState::blank(
                self.rng.sign(),
                self.rng.range(0.1, 0.9),
                self.rng.range(3.0, 10.0),
                self.rng.range(self.w * 0.15, self.w * 0.85),
            );
            self.groups.insert(kh, g);
        }
        let g = self.groups[&kh];
        let base_len = size_of(p.mem, self.ram_total) * sc;
        let len = base_len * self.fit;
        let (x, y, alpha, dir) = if first {
            // Un banco grande arranca repartido en una nube más ancha para que no nazca apelotonado.
            let spread = len * (0.9 + 0.55 * (shoal as f64).sqrt());
            (
                (g.cx + self.rng.gauss() * spread).clamp(len, (self.w - len).max(len)),
                (top + (bot - top) * g.yf + self.rng.gauss() * spread * 0.55).clamp(top, bot),
                1.0,
                g.dir,
            )
        } else {
            let from_left = self.rng.chance(0.5);
            (
                if from_left { -len * 0.6 } else { self.w + len * 0.6 },
                self.rng.range(top, bot),
                0.0,
                if from_left { 1.0 } else { -1.0 },
            )
        };
        let phase0 = self.rng.range(0.0, 2.0 * PI);
        let slot = {
            let mut used: Vec<usize> = self.fish.iter().filter(|f| f.kh == kh).map(|f| f.slot).collect();
            used.sort_unstable();
            used.iter().enumerate().find(|(i, s)| *i != **s).map_or(used.len(), |(i, _)| i)
        };
        self.fish.push(Fish {
            pid: p.pid,
            start: p.start,
            key: key.to_string(),
            kh,
            look: Look::new(kh, p.pid),
            x,
            y,
            vx: dir * len * 0.4,
            vy: 0.0,
            dir,
            face: dir,
            pitch: 0.0,
            flipv: 1.0,
            len,
            base_len,
            mem: p.mem,
            cpu: p.cpu,
            wbps: p.wbps,
            mem_goal: p.mem,
            cpu_goal: p.cpu,
            wbps_goal: p.wbps,
            phase: phase0,
            amp: 0.5,
            phase0,
            yoff: self.rng.gauss() * len * 0.25,
            slot,
            speed_var: self.rng.range(0.85, 1.2),
            life: Life::Alive,
            alpha,
            gone: false,
            bub_acc: 0.0,
            dead: false,
        });
    }

    /// Marca como cerrados (panza arriba) los peces con esos PIDs.
    pub fn mark_dying(&mut self, pids: &[i32]) {
        for f in &mut self.fish {
            if pids.contains(&f.pid) && f.life == Life::Alive {
                f.life = Life::Dying(0.0);
                f.gone = false;
            }
        }
    }

    /// PIDs vivos de un banco.
    pub fn pids_of(&self, kh: u64) -> Vec<i32> {
        self.fish.iter().filter(|f| f.kh == kh && f.life == Life::Alive).map(|f| f.pid).collect()
    }

    /// Para capturas y pruebas: reparte CPU y escritura de forma variada.
    pub fn demo_randomize(&mut self) {
        for f in &mut self.fish {
            let c = if self.rng.chance(0.25) { self.rng.range(20.0, 160.0) } else { self.rng.range(0.0, 6.0) };
            f.cpu = c;
            f.cpu_goal = c;
            let w = if self.rng.chance(0.25) { 10f64.powf(self.rng.range(4.5, 7.5)) } else { 0.0 };
            f.wbps = w;
            f.wbps_goal = w;
        }
    }

    fn reorder(&mut self) {
        let key = |f: &Fish| f.len + if matches!(f.life, Life::Dying(_)) { 10_000.0 } else { 0.0 };
        let mut o: Vec<usize> = (0..self.fish.len()).collect();
        o.sort_by(|&a, &b| key(&self.fish[a]).partial_cmp(&key(&self.fish[b])).unwrap_or(std::cmp::Ordering::Equal));
        self.order = o;
    }

    // --- interacción

    /// Qué hay bajo el cursor (de delante hacia atrás).
    pub fn pick(&self, px: f64, py: f64) -> Option<Pick> {
        for &i in self.order.iter().rev() {
            let f = &self.fish[i];
            if f.life != Life::Alive || f.alpha < 0.3 {
                continue;
            }
            let lx = (px - f.x) * f.dir;
            let ly = py - f.y;
            let a = (f.len * 0.52).max(12.0);
            let b = (f.len * 0.30).max(10.0);
            if (lx / a).powi(2) + (ly / b).powi(2) <= 1.0 {
                return Some(Pick::Fish(i));
            }
        }
        for (i, d) in self.decor.iter().enumerate() {
            let x = d.xf * self.w;
            let base = self.floor_y(x);
            if (px - x).abs() < d.size * 0.4 + 6.0 && py > base - d.size && py < base + 8.0 {
                return Some(Pick::Decor(i));
            }
        }
        None
    }

    // --- paso de simulación

    pub fn step(&mut self, dt: f64) {
        let dt = dt.clamp(0.0, 0.1);
        self.t += dt;
        let k = |r: f64| 1.0 - (-dt * r).exp();
        let (w, sc) = (self.w, self.scale());
        let sy = self.surface_y();
        let (top, bot) = self.band();

        // El agua: temperatura suavizada y hervor.
        if self.have_temp {
            self.temp += (self.temp_goal - self.temp) * k(1.5);
        }
        let btarget = if self.have_temp { smooth((self.temp - BOIL_FROM) / (BOIL_FULL - BOIL_FROM)) } else { 0.0 };
        self.boil += (btarget - self.boil) * k(1.2);
        let b = self.boil;

        let mut fish = std::mem::take(&mut self.fish);

        // Valores suavizados y tamaño. Si hay demasiada biomasa, todos se encogen para caber.
        let mut area = 0.0;
        for f in &mut fish {
            f.cpu += (f.cpu_goal - f.cpu) * k(2.5);
            f.mem += (f.mem_goal - f.mem) * k(1.5);
            f.wbps += (f.wbps_goal - f.wbps) * k(2.5);
            f.base_len = size_of(f.mem, self.ram_total) * sc;
            area += f.base_len * f.base_len * 0.2;
        }
        let fit_goal = (0.26 * w * (bot - top) / area.max(1.0)).sqrt().clamp(0.45, 1.0);
        self.fit += (fit_goal - self.fit) * k(0.8);
        for d in &mut self.decor {
            d.size = decor_size(d.mem, self.ram_total) * sc;
        }

        // Bancos: centro, cambios de rumbo y de profundidad.
        for g in self.groups.values_mut() {
            g.n = 0;
            g.sx = 0.0;
            g.sy = 0.0;
            g.minx = f64::MAX;
            g.maxx = f64::MIN;
            g.lsum = 0.0;
        }
        for f in &fish {
            if f.life == Life::Alive
                && let Some(g) = self.groups.get_mut(&f.kh) {
                    g.n += 1;
                    g.sx += f.x;
                    g.sy += f.y;
                    g.minx = g.minx.min(f.x);
                    g.maxx = g.maxx.max(f.x);
                    g.lsum += f.len;
                }
        }
        self.groups.retain(|_, g| g.n > 0);
        for g in self.groups.values_mut() {
            g.cx = g.sx / g.n as f64;
            g.timer -= dt;
            g.cool -= dt;
            if g.timer <= 0.0 {
                g.yf = self.rng.range(0.08, 0.9);
                g.timer = self.rng.range(5.0, 14.0) / (1.0 + b);
                if g.cool <= 0.0 && self.rng.chance(0.2) {
                    g.dir = -g.dir;
                    g.cool = 3.0;
                }
            }
            // El banco gira cuando su pez de cabeza llega a la pared (con pausa para no vibrar
            // cuando el banco es tan grande que toca las dos).
            let wall = 0.5 * g.lsum / g.n as f64 + 14.0;
            if g.cool <= 0.0 {
                if g.dir < 0.0 && g.minx < wall {
                    g.dir = 1.0;
                    g.cool = 4.0;
                } else if g.dir > 0.0 && g.maxx > w - wall {
                    g.dir = -1.0;
                    g.cool = 4.0;
                }
            }
        }

        let snap: Vec<(u64, f64, f64, f64)> = fish.iter().map(|f| (f.kh, f.x, f.y, f.len)).collect();
        for f in &mut fish {
            let g = self.groups.get(&f.kh).copied().unwrap_or(GroupState::blank(f.dir, 0.5, 1.0, f.x));
            f.len += (f.base_len * self.fit - f.len) * k(1.5);
            let cpu_t = 1.0 - (-f.cpu / 40.0).exp();

            match f.life {
                Life::Alive | Life::Leaving => {
                    // Más CPU, más rápido; con el agua hirviendo todos se agitan.
                    let speed = ((0.28 + 2.4 * cpu_t) * f.len * f.speed_var * (1.0 + 1.1 * b)).clamp(10.0, 480.0 * sc);
                    // Cada pez rebota por su cuenta antes de tocar la pared, aunque el banco aún no haya girado.
                    let wm = f.len * 0.6 + 30.0;
                    let dirx = if f.x < wm && g.dir < 0.0 {
                        1.0
                    } else if f.x > w - wm && g.dir > 0.0 {
                        -1.0
                    } else {
                        g.dir
                    };
                    let mut dvx = dirx * speed;
                    // Puesto en la formación del banco: espiral de girasol, estirada a lo largo.
                    let (ox, oy) = if g.n > 1 {
                        let lavg = g.lsum / g.n as f64;
                        let r = 0.55 * lavg * (f.slot as f64).sqrt();
                        let th = f.slot as f64 * 2.399_963;
                        (r * th.cos() * 1.8, r * th.sin() * 0.7)
                    } else {
                        (0.0, 0.0)
                    };
                    if g.n > 1 {
                        let lim = 0.6 * speed + 20.0;
                        dvx += ((g.cx + ox - f.x) * 0.5).clamp(-lim, lim);
                    }
                    let mut dvy = 0.0;
                    for &(kh, x, y, l) in &snap {
                        let (dx, dy) = (f.x - x, f.y - y);
                        let d2 = dx * dx + dy * dy;
                        // Los del mismo banco se mantienen separados; los de otros bancos solo se rozan.
                        let (r, k) = if kh == f.kh { (0.40 * (f.len + l) + 6.0, 1.3) } else { (0.20 * (f.len + l), 0.5) };
                        if d2 > 1e-3 && d2 < r * r {
                            let d = d2.sqrt();
                            let p = (r - d) / r;
                            // Fuerza absoluta (no proporcional a la velocidad): los peces ociosos también se apartan.
                            let push = p * (speed + 0.9 * f.len + 20.0) * k;
                            dvx += dx / d * push;
                            dvy += dy / d * push * 0.8;
                        }
                    }
                    let lm = f.len * 0.5 + 8.0;
                    dvx += (lm - f.x).max(0.0) * 2.5 - (f.x - (w - lm)).max(0.0) * 2.5;
                    let yf = g.yf + (0.14 - g.yf) * 0.35 * b;
                    let ty = (top + (bot - top) * yf + oy + f.yoff).clamp(top, bot);
                    let lim = 0.5 * speed + 10.0;
                    dvy += ((ty - f.y) * 0.9).clamp(-lim, lim);
                    dvy += (self.t * 0.8 + f.phase0).sin() * (6.0 + 28.0 * b)
                        + (self.t * 11.0 + f.phase0 * 5.0).sin() * b * 22.0;
                    let kv = k(2.4);
                    f.vx += (dvx - f.vx) * kv;
                    f.vy += (dvy - f.vy) * kv;
                    f.x = (f.x + f.vx * dt).clamp(-1.5 * f.len, w + 1.5 * f.len);
                    f.y = (f.y + f.vy * dt).clamp(top, bot);

                    let th = 0.15 * speed + 2.0;
                    if f.vx > th {
                        f.dir = 1.0;
                    } else if f.vx < -th {
                        f.dir = -1.0;
                    }
                    f.face += (f.dir - f.face) * k(7.0);
                    f.flipv += (1.0 - f.flipv) * k(4.0);
                    let pt = (-f.vy).atan2(f.vx.abs().max(20.0)).clamp(-0.55, 0.55);
                    f.pitch += (pt - f.pitch) * k(4.0);
                    let freq = (0.8 + 1.5 * f.vx.hypot(f.vy) / f.len.max(10.0)).clamp(0.8, 7.5);
                    f.phase += 2.0 * PI * freq * dt;
                    f.amp += ((0.5 + 0.35 * cpu_t + 0.15 * b) - f.amp) * k(3.0);

                    if f.life == Life::Leaving {
                        f.alpha -= dt / 0.8;
                        f.bub_acc += 30.0 * dt;
                        if f.alpha <= 0.0 {
                            f.dead = true;
                        }
                    } else {
                        f.alpha = (f.alpha + dt / 0.7).min(1.0);
                        // La escritura a disco sale por la boca en forma de burbujas.
                        f.bub_acc += 8.0 * ((1.0 + f.wbps / 2e4).log10() / 3.5).clamp(0.0, 1.0) * dt;
                    }
                }
                Life::Dying(t) => {
                    let t2 = t + dt;
                    f.life = Life::Dying(t2);
                    f.flipv += (-1.0 - f.flipv) * k(6.0);
                    f.amp += (0.0 - f.amp) * k(4.0);
                    f.pitch += (0.0 - f.pitch) * k(3.0);
                    f.vx *= (-dt * 2.5).exp();
                    let float_y = sy + f.len * 0.22 + 6.0 + (self.t * 1.6 + f.phase0).sin() * 2.0;
                    let tvy = if f.y > float_y { -(40.0 * sc + 0.35 * f.len) } else { 0.0 };
                    f.vy += (tvy - f.vy) * k(3.0);
                    f.x += f.vx * dt;
                    f.y += f.vy * dt;
                    if f.y < float_y {
                        f.y = float_y;
                        f.vy = 0.0;
                    }
                    if t2 > 3.5 && f.gone {
                        f.alpha -= dt / 1.2;
                        if f.alpha <= 0.0 {
                            f.dead = true;
                        }
                    }
                    // Si el proceso sobrevive al SIGKILL, el pez revive.
                    if t2 > 10.0 && !f.gone {
                        f.life = Life::Alive;
                    }
                }
            }

            while f.bub_acc >= 1.0 {
                f.bub_acc -= 1.0;
                if self.bubbles.len() < MAX_BUBBLES {
                    let r = (1.2 + f.len * 0.018) * self.rng.range(0.6, 1.2);
                    self.bubbles.push(Bubble {
                        x: f.x + f.dir * f.len * 0.47 + self.rng.gauss() * 2.0,
                        y: f.y - f.len * 0.02,
                        r,
                        vy: 45.0 + r * 14.0,
                        wob: self.rng.range(0.0, 2.0 * PI),
                        wf: self.rng.range(3.0, 6.0),
                        wa: self.rng.range(3.0, 9.0),
                        life: 1e9,
                        max_life: 1e9,
                        kind: BubbleKind::Air,
                    });
                }
            }
        }
        fish.retain(|f| !f.dead);
        self.fish = fish;
        self.reorder();

        // Hervor: burbujas desde el fondo (más densas cuanto más calor) y espuma en la superficie.
        if b > 0.02 {
            self.boil_acc += (2.0 + 240.0 * b.powf(1.6)) * (w / 1000.0) * dt;
            while self.boil_acc >= 1.0 {
                self.boil_acc -= 1.0;
                if self.bubbles.len() >= MAX_BUBBLES {
                    continue;
                }
                let x = if self.rng.chance(0.6) {
                    let vi = self.rng.range(0.0, 5.0) as usize;
                    self.vent_x(vi) + self.rng.gauss() * 30.0 * sc
                } else {
                    self.rng.range(0.0, w)
                };
                let r = self.rng.range(1.5, 3.5 + 6.0 * b) * sc.max(0.8);
                self.bubbles.push(Bubble {
                    x,
                    y: self.floor_y(x) + 2.0,
                    r,
                    vy: (60.0 + r * 18.0) * (1.0 + 0.9 * b),
                    wob: self.rng.range(0.0, 2.0 * PI),
                    wf: self.rng.range(3.0, 7.0),
                    wa: self.rng.range(4.0, 14.0) * (0.5 + b),
                    life: 1e9,
                    max_life: 1e9,
                    kind: BubbleKind::Air,
                });
            }
        }
        if b > 0.3 {
            self.foam_acc += 40.0 * b * (w / 1000.0) * dt;
            while self.foam_acc >= 1.0 {
                self.foam_acc -= 1.0;
                if self.bubbles.len() >= MAX_BUBBLES {
                    continue;
                }
                let life = self.rng.range(0.8, 1.8);
                self.bubbles.push(Bubble {
                    x: self.rng.range(0.0, w),
                    y: sy + self.rng.range(0.0, 5.0),
                    r: self.rng.range(2.0, 6.0) * sc,
                    vy: 0.0,
                    wob: self.rng.range(0.0, 6.0),
                    wf: 1.0,
                    wa: 0.0,
                    life,
                    max_life: life,
                    kind: BubbleKind::Foam,
                });
            }
        }
        // Piedra de aire: su burbujeo sigue la escritura total a disco.
        self.air_acc += (1.0 + 9.0 * ((1.0 + self.disk_w / 1e5).log10() / 3.0).clamp(0.0, 1.0)) * dt;
        while self.air_acc >= 1.0 {
            self.air_acc -= 1.0;
            if self.bubbles.len() >= MAX_BUBBLES {
                continue;
            }
            let x = w * 0.92;
            let r = self.rng.range(1.5, 3.2) * sc.max(0.8);
            self.bubbles.push(Bubble {
                x: x + self.rng.gauss() * 2.0,
                y: self.floor_y(x) - 6.0,
                r,
                vy: 70.0 + r * 16.0,
                wob: self.rng.range(0.0, 2.0 * PI),
                wf: self.rng.range(3.0, 6.0),
                wa: self.rng.range(2.0, 6.0),
                life: 1e9,
                max_life: 1e9,
                kind: BubbleKind::Air,
            });
        }

        let mut pops = vec![];
        for bu in &mut self.bubbles {
            match bu.kind {
                BubbleKind::Air => {
                    bu.y -= bu.vy * dt;
                    bu.wob += bu.wf * dt;
                    bu.x += bu.wob.sin() * bu.wa * dt;
                    bu.r *= 1.0 + 0.08 * dt;
                    if bu.y < sy + bu.r * 0.4 {
                        bu.life = 0.0;
                        pops.push(bu.x);
                    }
                }
                BubbleKind::Foam => {
                    bu.life -= dt;
                    bu.wob += dt;
                    bu.x += bu.wob.sin() * 3.0 * dt;
                }
            }
        }
        self.bubbles.retain(|bu| bu.life > 0.0);
        for x in pops {
            if b > 0.15 && self.ripples.len() < 40 && self.rng.chance(0.4) {
                self.ripples.push((x, 0.0));
            }
        }
        for r in &mut self.ripples {
            r.1 += dt;
        }
        self.ripples.retain(|r| r.1 < 0.9);

        // Nieve marina: la corriente sigue la carga del sistema.
        let drift = 5.0 + 38.0 * self.load.min(2.0);
        for m in &mut self.motes {
            m.x += drift * (0.3 + m.z) * dt;
            m.y += (self.t * 0.3 + m.x * 0.01).sin() * 3.0 * dt;
            if m.x > w {
                m.x -= w;
            }
            m.y = m.y.clamp(sy, self.h * 0.9);
        }
    }
}

/// Tamaño de una pieza de decoración según la RAM del proceso protegido.
fn decor_size(mem: f64, ram_total: f64) -> f64 {
    let lo = 1.5e6;
    let hi = (ram_total * 0.5).max(2e9);
    let t = ((mem.max(lo) / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0);
    26.0 + 140.0 * t.powf(1.3)
}
