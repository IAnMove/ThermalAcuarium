//! Hilo de rasterizado de sprites. Pintar un pez con Cairo cuesta de 1 a 10 ms según el estilo; si
//! se hace en el hilo de la interfaz cada vez que aparece una variante nueva (tamaño, fase de la
//! cola), el fotograma da un tirón. Aquí la interfaz solo encola peticiones y, mientras llega el
//! sprite, pinta el más parecido que ya tenga. Los resultados vuelven como bytes BGRA premultiplicados
//! (las superficies de Cairo no viajan entre hilos) y la interfaz los convierte en texturas.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use gtk::cairo;

use crate::draw::{DKey, SKey, render_decor, render_sprite};
use crate::sim::{Decor, Look};
use crate::theme::{Palette, Style};

pub enum Job {
    Fish { key: SKey, look: Look, len: f64, blen: f64, era: u32 },
    Decor { key: DKey, d: Box<Decor>, size: f64, style: Style, cell: f64, era: u32 },
}

/// Un sprite terminado: píxeles BGRA premultiplicados y su geometría.
pub struct Pixels {
    pub w: i32,
    pub h: i32,
    pub stride: i32,
    pub data: Vec<u8>,
}

pub enum Done {
    Fish { key: SKey, era: u32, px: Pixels, ox: f64, oy: f64, len: f64 },
    Decor { key: DKey, era: u32, px: Pixels, w: f64, h: f64, ox: f64, oy: f64, size: f64 },
}

fn pixels(mut surf: cairo::ImageSurface) -> Option<Pixels> {
    surf.flush();
    let (w, h, stride) = (surf.width(), surf.height(), surf.stride());
    let data = surf.data().ok()?.to_vec();
    Some(Pixels { w, h, stride, data })
}

enum Msg {
    Job(Job),
    /// Nueva paleta y escala de dispositivo para los trabajos siguientes.
    Env(Box<Palette>, f64),
}

pub struct Worker {
    tx: Sender<Msg>,
    rx: Receiver<Done>,
}

impl Worker {
    pub fn spawn(pal: &Palette, ds: f64) -> Worker {
        let (tx, jobs) = channel::<Msg>();
        let (done_tx, rx) = channel::<Done>();
        let pal0 = pal.clone();
        std::thread::Builder::new()
            .name("sprites".into())
            .spawn(move || run(jobs, done_tx, pal0, ds))
            .expect("hilo de sprites");
        Worker { tx, rx }
    }

    pub fn set_env(&self, pal: &Palette, ds: f64) {
        let _ = self.tx.send(Msg::Env(Box::new(pal.clone()), ds));
    }

    pub fn request(&self, job: Job) {
        let _ = self.tx.send(Msg::Job(job));
    }

    /// Resultados que ya están listos (sin bloquear).
    pub fn poll(&self) -> Vec<Done> {
        let mut v = vec![];
        loop {
            match self.rx.try_recv() {
                Ok(d) => v.push(d),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return v,
            }
        }
    }
}

fn run(jobs: Receiver<Msg>, done: Sender<Done>, mut pal: Palette, mut ds: f64) {
    // Los trabajos se atienden del más reciente al más antiguo: lo que se pidió en el último
    // fotograma es lo que se está viendo ahora.
    let mut queue: Vec<Job> = vec![];
    loop {
        // Espera al menos un mensaje y recoge todos los que haya.
        let first = match jobs.recv() {
            Ok(m) => m,
            Err(_) => return,
        };
        let mut msgs = vec![first];
        while let Ok(m) = jobs.try_recv() {
            msgs.push(m);
        }
        for m in msgs {
            match m {
                Msg::Env(p, d) => {
                    pal = *p;
                    ds = d;
                    queue.clear();
                }
                Msg::Job(j) => queue.push(j),
            }
        }
        while let Some(j) = queue.pop() {
            let d = match j {
                Job::Fish { key, look, len, blen, era } => {
                    let spr = render_sprite(&look, len, &key, blen, &pal, ds);
                    let Some(px) = pixels(spr.surf) else { continue };
                    Done::Fish { key, era, px, ox: spr.ox, oy: spr.oy, len: spr.len }
                }
                Job::Decor { key, d, size, style, cell, era } => {
                    let Some(ds_) = render_decor(&d, &key, size, style, cell, &pal, ds) else { continue };
                    let Some(px) = pixels(ds_.surf) else { continue };
                    Done::Decor { key, era, px, w: ds_.w, h: ds_.h, ox: ds_.ox, oy: ds_.oy, size: ds_.size }
                }
            };
            if done.send(d).is_err() {
                return;
            }
            // Si entre tanto llegaron peticiones nuevas, pasan por delante.
            while let Ok(m) = jobs.try_recv() {
                match m {
                    Msg::Env(p, d) => {
                        pal = *p;
                        ds = d;
                        queue.clear();
                    }
                    Msg::Job(j) => queue.push(j),
                }
            }
            if queue.len() > 400 {
                // Demasiado atraso (p. ej. cambio de estilo con cientos de peces): se descartan
                // las peticiones más antiguas, que la interfaz volverá a pedir si aún las necesita.
                queue.drain(..queue.len() - 200);
            }
        }
    }
}
