//! Hilo de rasterizado de sprites. Pintar un pez con Cairo cuesta de 1 a 10 ms según el estilo; si
//! se hace en el hilo de la interfaz cada vez que aparece una variante nueva (tamaño, fase de la
//! cola), el fotograma da un tirón. Aquí la interfaz solo encola peticiones y, mientras llega el
//! sprite, pinta el más parecido que ya tenga. Los resultados vuelven como bytes BGRA premultiplicados
//! (las superficies de Cairo no viajan entre hilos) y la interfaz los convierte en texturas.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use crate::draw::{SKey, render_sprite};
use crate::sim::Look;
use crate::theme::Palette;

pub struct Job {
    pub key: SKey,
    pub look: Look,
    pub len: f64,
    pub blen: f64,
    /// Generación de la caché que lo pidió: si cambió (tema, estilo, escala) el resultado se tira.
    pub era: u32,
}

pub struct Done {
    pub key: SKey,
    pub era: u32,
    pub w: i32,
    pub h: i32,
    pub stride: i32,
    pub data: Vec<u8>,
    pub ox: f64,
    pub oy: f64,
    pub len: f64,
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
            let spr = render_sprite(&j.look, j.len, &j.key, j.blen, &pal, ds);
            let mut surf = spr.surf;
            surf.flush();
            let (w, h, stride) = (surf.width(), surf.height(), surf.stride());
            let Ok(data) = surf.data() else { continue };
            let d = Done { key: j.key, era: j.era, w, h, stride, data: data.to_vec(), ox: spr.ox, oy: spr.oy, len: spr.len };
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
