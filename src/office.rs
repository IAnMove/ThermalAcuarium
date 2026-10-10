//! Modo Oficina: el administrador de tareas serio. Dos pestañas: Procesos (tabla ordenable con
//! filtro y botón de cerrar) y Rendimiento (CPU, RAM, disco y temperatura con historial).
//! Mismo tema de Omarchy que el acuario: fuente mono, bordes rectos, nada de adornos.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use adw::prelude::*;
use gtk::cairo;

use crate::i18n::{tr, trf};
use crate::sys::{Disk, Group, Reading, SysReading, fmt_bytes};
use crate::theme::{Palette, Rgb};

pub const HIST: usize = 120;

/// Valores de una fila de la tabla (para ordenar y filtrar sin leer los widgets).
#[derive(Clone)]
struct Vals {
    key: String,
    n: usize,
    pid: i32,
    cpu: f64,
    mem: f64,
    wbps: f64,
}

struct Row {
    row: gtk::ListBoxRow,
    name: gtk::Label,
    n: gtk::Label,
    pid: gtk::Label,
    cpu: gtk::Label,
    mem: gtk::Label,
    wbps: gtk::Label,
    bar: gtk::ProgressBar,
}

struct Card {
    value: gtk::Label,
    sub: gtk::Label,
    area: gtk::DrawingArea,
    hist: Rc<RefCell<VecDeque<f64>>>,
}

pub struct Office {
    pub root: gtk::Box,
    stack: gtk::Stack,
    tabs: Vec<gtk::ToggleButton>,
    list: gtk::ListBox,
    search: gtk::SearchEntry,
    rows: RefCell<HashMap<String, Row>>,
    vals: Rc<RefCell<HashMap<String, Vals>>>,
    /// Columna de orden (0 nombre, 1 nº, 2 pid, 3 cpu, 4 ram, 5 escritura) y sentido.
    sort: Rc<Cell<(usize, bool)>>,
    heads: Vec<gtk::Button>,
    cpu: Card,
    ram: Card,
    disk: Card,
    temp: Card,
    /// Pestaña Memoria: tarjetas (valor, barra) y lista de los que más usan.
    mem_cards: Vec<(gtk::Label, gtk::Label, gtk::ProgressBar)>,
    top_list: gtk::Box,
    top_rows: RefCell<Vec<(gtk::Label, gtk::Label, gtk::ProgressBar)>>,
    /// Pestaña Discos: una tarjeta por disco y la lista de los procesos con más E/S.
    disk_box: gtk::Box,
    disk_cards: RefCell<Vec<(String, gtk::Label, gtk::Label, gtk::Label, gtk::ProgressBar)>>,
    dtop_list: gtk::Box,
    dtop_rows: RefCell<Vec<(gtk::Label, gtk::Label, gtk::ProgressBar)>>,
    /// Pestaña Sensores: CPU, GPU, NVMe (con historial), ventilador y carga.
    sens: Vec<Card>,
    pal: Rc<RefCell<Palette>>,
    pub on_close: RefCell<Option<Rc<dyn Fn(&str)>>>,
}

const COLS: [(&str, f32, i32); 6] = [
    ("col_name", 0.0, 220),
    ("col_n", 1.0, 60),
    ("col_pid", 1.0, 80),
    ("col_cpu", 1.0, 70),
    ("col_ram", 1.0, 90),
    ("col_write", 1.0, 100),
];

fn cell(xalign: f32, width: i32) -> gtk::Label {
    gtk::Label::builder().xalign(xalign).width_request(width).ellipsize(gtk::pango::EllipsizeMode::End).build()
}

fn card(title: &str, pal: &Rc<RefCell<Palette>>, hist: Rc<RefCell<VecDeque<f64>>>, color: fn(&Palette) -> Rgb, max: Option<f64>) -> (gtk::Box, Card) {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    b.add_css_class("card");
    b.set_hexpand(true);
    b.set_vexpand(true);
    let t = gtk::Label::builder().label(title).xalign(0.0).build();
    t.add_css_class("cardlabel");
    let value = gtk::Label::builder().label("--").xalign(0.0).build();
    value.add_css_class("cardvalue");
    let sub = gtk::Label::builder().label(" ").xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    sub.add_css_class("cardsub");
    let area = gtk::DrawingArea::new();
    area.set_vexpand(true);
    area.set_content_height(60);
    let c = Card { value, sub, area: area.clone(), hist: hist.clone() };
    let p2 = pal.clone();
    area.set_draw_func(move |_, cr, w, h| sparkline(cr, w as f64, h as f64, &hist.borrow(), color(&p2.borrow()), &p2.borrow(), max));
    for w in [&t, &c.value, &c.sub] {
        b.append(w);
    }
    b.append(&area);
    (b, c)
}

/// Gráfica de historial: área rellena y línea, con la rejilla de fondo.
fn sparkline(cr: &cairo::Context, w: f64, h: f64, hist: &VecDeque<f64>, col: Rgb, pal: &Palette, max: Option<f64>) {
    cr.set_source_rgba(pal.dbg.0, pal.dbg.1, pal.dbg.2, 1.0);
    cr.rectangle(0.0, 0.0, w, h);
    let _ = cr.fill();
    cr.set_source_rgba(pal.dfg.0, pal.dfg.1, pal.dfg.2, 0.25);
    cr.set_line_width(1.0);
    for i in 1..4 {
        let y = (h * i as f64 / 4.0).round() + 0.5;
        cr.move_to(0.0, y);
        cr.line_to(w, y);
    }
    let _ = cr.stroke();
    if hist.len() < 2 {
        return;
    }
    let top = max.unwrap_or_else(|| hist.iter().cloned().fold(1e-9, f64::max) * 1.15).max(1e-9);
    let n = HIST as f64;
    let pt = |i: usize, v: f64| ((w * (i as f64 + (n - hist.len() as f64)) / (n - 1.0)), h - (v / top).clamp(0.0, 1.0) * (h - 2.0) - 1.0);
    for (i, &v) in hist.iter().enumerate() {
        let (x, y) = pt(i, v);
        if i == 0 {
            cr.move_to(x, y);
        } else {
            cr.line_to(x, y);
        }
    }
    cr.set_source_rgba(col.0, col.1, col.2, 1.0);
    cr.set_line_width(1.5);
    cr.set_line_join(cairo::LineJoin::Round);
    let _ = cr.stroke_preserve();
    let (xl, _) = pt(hist.len() - 1, 0.0);
    cr.line_to(xl, h);
    cr.line_to(pt(0, 0.0).0, h);
    cr.close_path();
    cr.set_source_rgba(col.0, col.1, col.2, 0.18);
    let _ = cr.fill();
}

impl Office {
    pub fn new(pal: Palette) -> Rc<Office> {
        let pal = Rc::new(RefCell::new(pal));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Pestañas.
        let tabbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tabbar.add_css_class("tabs");
        tabbar.set_margin_top(8);
        tabbar.set_margin_start(12);
        tabbar.set_margin_end(12);
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        let mut tabs = vec![];
        for (i, key) in ["tab_procs", "tab_mem", "tab_disks", "tab_sensors", "tab_perf"].iter().enumerate() {
            let b = gtk::ToggleButton::with_label(tr(key));
            if i == 0 {
                b.set_active(true);
            } else {
                b.set_group(Some(&tabs[0]));
            }
            tabbar.append(&b);
            tabs.push(b);
        }
        root.append(&tabbar);
        root.append(&stack);

        // --- Procesos
        let procs = gtk::Box::new(gtk::Orientation::Vertical, 6);
        procs.set_margin_top(8);
        procs.set_margin_bottom(12);
        procs.set_margin_start(12);
        procs.set_margin_end(12);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(tr("search")));
        procs.append(&search);
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.add_css_class("tablehead");
        let sort = Rc::new(Cell::new((3usize, true)));
        let mut heads = vec![];
        for (i, (key, xalign, width)) in COLS.iter().enumerate() {
            let b = gtk::Button::with_label(tr(key));
            b.add_css_class("flat");
            b.add_css_class("colhead");
            b.set_width_request(*width);
            if i == 0 {
                b.set_hexpand(true);
            }
            if let Some(l) = b.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
                l.set_xalign(*xalign);
            }
            head.append(&b);
            heads.push(b);
        }
        head.append(&gtk::Label::builder().width_request(70).build());
        procs.append(&head);
        let list = gtk::ListBox::new();
        list.add_css_class("proclist");
        list.set_selection_mode(gtk::SelectionMode::None);
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&list).build();
        procs.append(&scroll);
        stack.add_named(&procs, Some("procs"));

        // --- Rendimiento
        let perf = gtk::Grid::new();
        perf.set_row_spacing(8);
        perf.set_column_spacing(8);
        perf.set_row_homogeneous(true);
        perf.set_column_homogeneous(true);
        perf.set_margin_top(8);
        perf.set_margin_bottom(12);
        perf.set_margin_start(12);
        perf.set_margin_end(12);
        let mk_hist = || Rc::new(RefCell::new(VecDeque::with_capacity(HIST)));
        let (bc, cpu) = card("CPU", &pal, mk_hist(), |p| p.accent, Some(100.0));
        let (br, ram) = card("RAM", &pal, mk_hist(), |p| p.green, Some(100.0));
        let (bd, disk) = card(tr("disk_w"), &pal, mk_hist(), |p| p.cyan, None);
        let (bt, temp) = card(tr("temp"), &pal, mk_hist(), |p| p.orange, Some(105.0));
        perf.attach(&bc, 0, 0, 1, 1);
        perf.attach(&br, 1, 0, 1, 1);
        perf.attach(&bd, 0, 1, 1, 1);
        perf.attach(&bt, 1, 1, 1, 1);
        stack.add_named(&perf, Some("perf"));

        // --- Memoria
        let memv = gtk::Box::new(gtk::Orientation::Vertical, 8);
        memv.set_margin_top(8);
        memv.set_margin_bottom(12);
        memv.set_margin_start(12);
        memv.set_margin_end(12);
        let memg = gtk::Grid::new();
        memg.set_column_spacing(8);
        memg.set_column_homogeneous(true);
        let mut mem_cards = vec![];
        for (i, key) in ["mem_used", "mem_cache", "mem_free", "mem_swap_t"].iter().enumerate() {
            let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
            b.add_css_class("card");
            let t = gtk::Label::builder().label(tr(key)).xalign(0.0).build();
            t.add_css_class("cardlabel");
            let v = gtk::Label::builder().label("--").xalign(0.0).build();
            v.add_css_class("cardvalue");
            let sb = gtk::Label::builder().label(" ").xalign(0.0).build();
            sb.add_css_class("cardsub");
            let bar = gtk::ProgressBar::new();
            for w in [&t, &v, &sb] {
                b.append(w);
            }
            b.append(&bar);
            memg.attach(&b, i as i32, 0, 1, 1);
            mem_cards.push((v, sb, bar));
        }
        memv.append(&memg);
        let top_t = gtk::Label::builder().label(tr("mem_top")).xalign(0.0).build();
        top_t.add_css_class("cardlabel");
        top_t.set_margin_top(6);
        memv.append(&top_t);
        let top_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&top_list).build();
        memv.append(&scroll);
        stack.add_named(&memv, Some("mem"));

        // --- Discos
        let dv = gtk::Box::new(gtk::Orientation::Vertical, 8);
        dv.set_margin_top(8);
        dv.set_margin_bottom(12);
        dv.set_margin_start(12);
        dv.set_margin_end(12);
        let disk_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        disk_box.set_homogeneous(true);
        dv.append(&disk_box);
        let dt = gtk::Label::builder().label(tr("disk_top")).xalign(0.0).build();
        dt.add_css_class("cardlabel");
        dt.set_margin_top(6);
        dv.append(&dt);
        let dtop_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&dtop_list).build();
        dv.append(&scroll);
        stack.add_named(&dv, Some("disks"));

        // --- Sensores
        let sg = gtk::Grid::new();
        sg.set_row_spacing(8);
        sg.set_column_spacing(8);
        sg.set_row_homogeneous(true);
        sg.set_column_homogeneous(true);
        sg.set_margin_top(8);
        sg.set_margin_bottom(12);
        sg.set_margin_start(12);
        sg.set_margin_end(12);
        let mut sens = vec![];
        for (i, (key, col, max)) in [
            ("sens_cpu", (|p: &Palette| p.orange) as fn(&Palette) -> Rgb, Some(105.0)),
            ("sens_gpu", |p: &Palette| p.yellow, Some(105.0)),
            ("sens_nvme", |p: &Palette| p.cyan, Some(105.0)),
            ("sens_fan", |p: &Palette| p.accent, None),
            ("sens_load", |p: &Palette| p.green, None),
        ]
        .into_iter()
        .enumerate()
        {
            let (b, c) = card(tr(key), &pal, mk_hist(), col, max);
            sg.attach(&b, (i % 3) as i32, (i / 3) as i32, 1, 1);
            sens.push(c);
        }
        stack.add_named(&sg, Some("sensors"));

        let o = Rc::new(Office {
            root,
            stack,
            tabs,
            list,
            search,
            rows: RefCell::new(HashMap::new()),
            vals: Rc::new(RefCell::new(HashMap::new())),
            sort,
            heads,
            cpu,
            ram,
            disk,
            temp,
            mem_cards,
            top_list,
            top_rows: RefCell::new(vec![]),
            disk_box,
            disk_cards: RefCell::new(vec![]),
            dtop_list,
            dtop_rows: RefCell::new(vec![]),
            sens,
            pal,
            on_close: RefCell::new(None),
        });

        for (i, t) in o.tabs.iter().enumerate() {
            let o2 = o.clone();
            t.connect_toggled(move |b| {
                if b.is_active() {
                    o2.stack.set_visible_child_name(["procs", "mem", "disks", "sensors", "perf"][i]);
                }
            });
        }
        for (i, h) in o.heads.iter().enumerate() {
            let o2 = o.clone();
            h.connect_clicked(move |_| {
                let (c, desc) = o2.sort.get();
                o2.sort.set(if c == i { (i, !desc) } else { (i, i != 0) });
                o2.mark_sort();
                o2.list.invalidate_sort();
            });
        }
        let vals = o.vals.clone();
        let sort = o.sort.clone();
        o.list.set_sort_func(move |a, b| {
            let v = vals.borrow();
            let (Some(a), Some(b)) = (v.get(a.widget_name().as_str()), v.get(b.widget_name().as_str())) else { return gtk::Ordering::Equal };
            let (c, desc) = sort.get();
            let ord = match c {
                0 => a.key.to_lowercase().cmp(&b.key.to_lowercase()),
                1 => a.n.cmp(&b.n),
                2 => a.pid.cmp(&b.pid),
                3 => a.cpu.partial_cmp(&b.cpu).unwrap_or(std::cmp::Ordering::Equal),
                4 => a.mem.partial_cmp(&b.mem).unwrap_or(std::cmp::Ordering::Equal),
                _ => a.wbps.partial_cmp(&b.wbps).unwrap_or(std::cmp::Ordering::Equal),
            };
            (if desc { ord.reverse() } else { ord }).into()
        });
        let vals = o.vals.clone();
        let search = o.search.clone();
        o.list.set_filter_func(move |r| {
            let q = search.text().to_lowercase();
            q.is_empty() || vals.borrow().get(r.widget_name().as_str()).is_some_and(|v| v.key.to_lowercase().contains(&q))
        });
        let o2 = o.clone();
        o.search.connect_search_changed(move |_| o2.list.invalidate_filter());
        o.mark_sort();
        o
    }

    /// Marca la columna de orden en la cabecera (▲/▼).
    fn mark_sort(&self) {
        let (c, desc) = self.sort.get();
        for (i, (key, _, _)) in COLS.iter().enumerate() {
            let t = if i == c { format!("{} {}", tr(key), if desc { "▼" } else { "▲" }) } else { tr(key).to_string() };
            self.heads[i].set_label(&t);
        }
    }

    pub fn show_tab(&self, i: usize) {
        if let Some(t) = self.tabs.get(i) {
            t.set_active(true);
        }
    }

    /// Tarjetas de la pestaña Memoria.
    pub fn refresh_mem(&self, s: &SysReading) {
        let total = s.ram_total.max(1.0);
        let free = (s.ram_total - s.ram_used).max(0.0);
        let vals = [
            (s.ram_used, total, format!("{:.0}%", s.ram_used / total * 100.0)),
            (s.cached, total, format!("{:.0}%", s.cached / total * 100.0)),
            (free, total, trf("ram_of", &[format!("{:.0}", free / total * 100.0), fmt_bytes(s.ram_total)])),
            (s.swap_used, s.swap_total.max(1.0), if s.swap_total > 0.0 { trf("mem_swap", &[fmt_bytes(s.swap_used), fmt_bytes(s.swap_total)]) } else { "–".into() }),
        ];
        for ((v, sb, bar), (val, max, sub)) in self.mem_cards.iter().zip(vals.iter()) {
            v.set_text(&fmt_bytes(*val));
            sb.set_text(sub);
            bar.set_fraction((val / max).clamp(0.0, 1.0));
        }
    }

    /// Lista de los programas que más RAM usan.
    pub fn refresh_top(&self, groups: &[Group], ram_total: f64) {
        let mut v: Vec<(String, f64)> = groups.iter().map(|g| (g.key.clone(), g.procs.iter().map(|p| p.mem).sum())).collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(15);
        let mut rows = self.top_rows.borrow_mut();
        while rows.len() < v.len() {
            let b = gtk::Box::new(gtk::Orientation::Vertical, 3);
            b.add_css_class("procrow");
            let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let name = cell(0.0, 200);
            name.add_css_class("procname");
            name.set_hexpand(true);
            let mem = cell(1.0, 90);
            mem.add_css_class("procmeta");
            top.append(&name);
            top.append(&mem);
            let bar = gtk::ProgressBar::new();
            b.append(&top);
            b.append(&bar);
            self.top_list.append(&b);
            rows.push((name, mem, bar));
        }
        for (i, (name, mem, bar)) in rows.iter().enumerate() {
            let Some(p) = name.parent().and_then(|t| t.parent()) else { continue };
            if let Some((k, m)) = v.get(i) {
                p.set_visible(true);
                name.set_text(k);
                mem.set_text(&fmt_bytes(*m));
                bar.set_fraction((m / ram_total.max(1.0)).clamp(0.0, 1.0));
            } else {
                p.set_visible(false);
            }
        }
    }

    /// Tarjetas de la pestaña Sensores.
    pub fn refresh_sensors(&self, r: &Reading, temp: Option<f64>) {
        let push = |c: &Card, v: f64| {
            let mut h = c.hist.borrow_mut();
            if h.len() == HIST {
                h.pop_front();
            }
            h.push_back(v);
            c.area.queue_draw();
        };
        let deg = |c: &Card, v: Option<f64>, zone: bool| match v {
            Some(t) => {
                push(c, t);
                c.value.set_text(&format!("{t:.0}°"));
                if zone {
                    c.sub.set_text(if t >= crate::weather::CRIT {
                        tr("wx_crit")
                    } else if t >= crate::weather::HOT {
                        tr("wx_hot")
                    } else if t >= crate::weather::WARM {
                        tr("wx_warm")
                    } else {
                        tr("wx_cool")
                    });
                }
            }
            None => c.value.set_text("--"),
        };
        deg(&self.sens[0], temp, true);
        deg(&self.sens[1], r.gpu, false);
        deg(&self.sens[2], r.nvme, false);
        match r.fan {
            Some(f) => {
                push(&self.sens[3], f as f64);
                self.sens[3].value.set_text(&format!("{f} rpm"));
                if let Some(m) = r.fan_max {
                    self.sens[3].sub.set_text(&format!("{:.0}% · max {m} rpm", f as f64 / m.max(1) as f64 * 100.0));
                }
            }
            None => self.sens[3].value.set_text("--"),
        }
        push(&self.sens[4], r.load);
        self.sens[4].value.set_text(&format!("{:.2}", r.load));
    }

    /// Tarjetas de la pestaña Discos (una por disco físico).
    pub fn refresh_disks(&self, disks: &[Disk]) {
        let mut cards = self.disk_cards.borrow_mut();
        if cards.len() != disks.len() || cards.iter().zip(disks).any(|(c, d)| c.0 != d.name) {
            while let Some(c) = self.disk_box.first_child() {
                self.disk_box.remove(&c);
            }
            cards.clear();
            for d in disks {
                let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
                b.add_css_class("card");
                let t = gtk::Label::builder().label(&d.name).xalign(0.0).build();
                t.add_css_class("cardlabel");
                let v = gtk::Label::builder().label("--").xalign(0.0).build();
                v.add_css_class("cardvalue");
                let io = gtk::Label::builder().label(" ").xalign(0.0).build();
                io.add_css_class("cardsub");
                let mp = gtk::Label::builder().label(" ").xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
                mp.add_css_class("cardsub");
                let bar = gtk::ProgressBar::new();
                for w in [&t, &v, &io, &mp] {
                    b.append(w);
                }
                b.append(&bar);
                self.disk_box.append(&b);
                cards.push((d.name.clone(), v, io, mp, bar));
            }
        }
        for ((_, v, io, mp, bar), d) in cards.iter().zip(disks) {
            let (u, t) = (d.used(), d.total().max(1.0));
            v.set_text(&format!("{} / {}", fmt_bytes(u), fmt_bytes(t)));
            io.set_text(&format!("{} {}/s   {} {}/s", tr("disk_read"), fmt_bytes(d.rbps), tr("disk_write_t"), fmt_bytes(d.wbps)));
            mp.set_text(&d.mounts.iter().map(|m| m.0.as_str()).collect::<Vec<_>>().join("  "));
            bar.set_fraction((u / t).clamp(0.0, 1.0));
        }
    }

    /// Procesos con más E/S de disco.
    pub fn refresh_disk_top(&self, groups: &[Group]) {
        let mut v: Vec<(String, f64, f64)> = groups
            .iter()
            .map(|g| (g.key.clone(), g.procs.iter().map(|p| p.rbps).sum::<f64>(), g.procs.iter().map(|p| p.wbps).sum::<f64>()))
            .filter(|(_, r, w)| r.max(*w) >= 1e3)
            .collect();
        v.sort_by(|a, b| (b.1 + b.2).partial_cmp(&(a.1 + a.2)).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(12);
        let top = v.first().map_or(1.0, |x| (x.1 + x.2).max(1.0));
        let mut rows = self.dtop_rows.borrow_mut();
        while rows.len() < v.len() {
            let b = gtk::Box::new(gtk::Orientation::Vertical, 3);
            b.add_css_class("procrow");
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let name = cell(0.0, 200);
            name.add_css_class("procname");
            name.set_hexpand(true);
            let io = cell(1.0, 200);
            io.add_css_class("procmeta");
            line.append(&name);
            line.append(&io);
            let bar = gtk::ProgressBar::new();
            b.append(&line);
            b.append(&bar);
            self.dtop_list.append(&b);
            rows.push((name, io, bar));
        }
        for (i, (name, io, bar)) in rows.iter().enumerate() {
            let Some(p) = name.parent().and_then(|t| t.parent()) else { continue };
            if let Some((k, r, w)) = v.get(i) {
                p.set_visible(true);
                name.set_text(k);
                io.set_text(&format!("↓ {}/s  ↑ {}/s", fmt_bytes(*r), fmt_bytes(*w)));
                bar.set_fraction(((r + w) / top).clamp(0.0, 1.0));
            } else {
                p.set_visible(false);
            }
        }
    }

    pub fn set_palette(&self, pal: &Palette) {
        *self.pal.borrow_mut() = pal.clone();
        for c in [&self.cpu, &self.ram, &self.disk, &self.temp] {
            c.area.queue_draw();
        }
    }

    /// Reconstruye la tabla con los grupos de la última muestra (reutiliza las filas que ya existen).
    pub fn refresh_procs(self: &Rc<Self>, groups: &[Group], ram_total: f64) {
        // Primero los valores (la función de orden los lee al añadir filas, así que no se puede
        // tener el préstamo abierto mientras se toca la lista).
        let mut new_vals = HashMap::new();
        for g in groups {
            let n = g.procs.len();
            new_vals.insert(
                g.key.clone(),
                Vals {
                    key: g.key.clone(),
                    n,
                    pid: g.procs.iter().map(|p| p.pid).min().unwrap_or(0),
                    cpu: g.procs.iter().map(|p| p.cpu).sum(),
                    mem: g.procs.iter().map(|p| p.mem).sum(),
                    wbps: g.procs.iter().map(|p| p.wbps).sum(),
                },
            );
        }
        *self.vals.borrow_mut() = new_vals;
        let mut rows = self.rows.borrow_mut();
        let mut seen = std::collections::HashSet::new();
        for g in groups {
            seen.insert(g.key.clone());
            let v = self.vals.borrow().get(&g.key).cloned();
            let Some(v) = v else { continue };
            let r = rows.entry(g.key.clone()).or_insert_with(|| self.build_row(&g.key, g.protected));
            r.name.set_text(&g.key);
            r.n.set_text(&v.n.to_string());
            r.pid.set_text(&if v.n > 1 { format!("{}…", v.pid) } else { v.pid.to_string() });
            r.cpu.set_text(&format!("{:.0}%", v.cpu));
            r.mem.set_text(&fmt_bytes(v.mem));
            r.wbps.set_text(&if v.wbps < 1e3 { "–".to_string() } else { format!("{}/s", fmt_bytes(v.wbps)) });
            r.bar.set_fraction((v.mem / ram_total.max(1.0)).clamp(0.0, 1.0));
            if r.row.parent().is_none() {
                r.row.set_widget_name(&g.key);
                self.list.append(&r.row);
            }
        }
        let gone: Vec<String> = rows.keys().filter(|k| !seen.contains(*k)).cloned().collect();
        for k in gone {
            if let Some(r) = rows.remove(&k) {
                self.list.remove(&r.row);
            }
        }
        drop(rows);
        self.list.invalidate_sort();
        self.list.invalidate_filter();
    }

    fn build_row(self: &Rc<Self>, key: &str, protected: bool) -> Row {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        b.add_css_class("procrow");
        let name = cell(0.0, 220);
        name.add_css_class("procname");
        name.set_hexpand(true);
        let n = cell(1.0, 60);
        let pid = cell(1.0, 80);
        let cpu = cell(1.0, 70);
        let mem = cell(1.0, 90);
        let wbps = cell(1.0, 100);
        for l in [&n, &pid, &cpu, &mem, &wbps] {
            l.add_css_class("procmeta");
        }
        let close = gtk::Button::with_label(tr("close_caps"));
        close.add_css_class("closebtn");
        close.set_width_request(70);
        close.set_valign(gtk::Align::Center);
        if protected {
            close.set_sensitive(false);
            close.set_tooltip_text(Some(tr("protected_tag")));
        }
        let o = self.clone();
        let k = key.to_string();
        close.connect_clicked(move |_| {
            if let Some(f) = o.on_close.borrow().as_ref() {
                f(&k);
            }
        });
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        for w in [&name, &n, &pid, &cpu, &mem, &wbps] {
            top.append(w);
        }
        top.append(&close);
        let bar = gtk::ProgressBar::new();
        bar.set_hexpand(true);
        let v = gtk::Box::new(gtk::Orientation::Vertical, 3);
        v.append(&top);
        v.append(&bar);
        b.append(&v);
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&b));
        row.set_activatable(false);
        Row { row, name, n, pid, cpu, mem, wbps, bar }
    }

    /// Actualiza las tarjetas de rendimiento con la última lectura.
    pub fn refresh_perf(&self, s: &SysReading, r: &Reading, temp: Option<f64>) {
        let push = |c: &Card, v: f64| {
            let mut h = c.hist.borrow_mut();
            if h.len() == HIST {
                h.pop_front();
            }
            h.push_back(v);
            c.area.queue_draw();
        };
        push(&self.cpu, s.cpu_pct);
        self.cpu.value.set_text(&format!("{:.0}%", s.cpu_pct));
        self.cpu.sub.set_text(&trf("load", &[format!("{:.2}", r.load)]));
        let pct = s.ram_used / s.ram_total.max(1.0) * 100.0;
        push(&self.ram, pct);
        self.ram.value.set_text(&fmt_bytes(s.ram_used));
        self.ram.sub.set_text(&trf("ram_of", &[format!("{pct:.0}"), fmt_bytes(s.ram_total)]));
        push(&self.disk, s.disk_w);
        self.disk.value.set_text(&format!("{}/s", fmt_bytes(s.disk_w)));
        self.disk.sub.set_text(&trf("read", &[fmt_bytes(s.disk_r)]));
        if let Some(t) = temp {
            push(&self.temp, t);
            self.temp.value.set_text(&format!("{t:.0}°"));
        }
        let f = |v: Option<f64>| v.map_or("--".to_string(), |v| format!("{v:.0}°"));
        self.temp.sub.set_text(&trf("sensors_tip", &[f(r.gpu), f(r.nvme)]));
    }

}
