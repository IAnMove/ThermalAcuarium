//! ThermalAcuarium - una pecera que es tu ordenador: cada proceso real es un pez.
//! RAM → tamaño · CPU → velocidad · escritura a disco → burbujas · temperatura → el agua hierve.
//! Ventana normal de Hyprland con la estética de Omarchy (paleta y fuente del tema activo).

mod draw;
mod gpu;
mod i18n;
mod office;
mod pixel;
mod reef;
mod rivers;
mod sonar;
mod volcano;
mod weather;
mod sim;
mod sys;
mod theme;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::cairo;
use gtk::{gdk, gio, glib};

use draw::View;
use i18n::{tr, trf};
use sim::{Pick, Tank};
use sys::{DiskSampler, Group, ProcSampler, Sensors, SysSampler, comm_of, fmt_bytes};
use theme::{Palette, Rgb, Style, current_font, to_hex};

const APP_ID: &str = "dev.ina.thermalacuarium";

// ---------------------------------------------------------------- argumentos

fn arg_val(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned())
}

fn has_flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

/// Temperatura forzada (`--temp 95` o THERMALACUARIUM_DEMO) para ver hervir el agua sin calentar el equipo.
fn demo_temp() -> Option<f64> {
    arg_val("--temp").or_else(|| std::env::var("THERMALACUARIUM_DEMO").ok()).and_then(|s| s.parse().ok())
}

/// Estilo inicial: `--style real|stylized` o el último que se eligió.
fn start_style() -> Style {
    arg_val("--style").and_then(|s| Style::parse(&s)).unwrap_or_else(Style::load)
}

fn seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9E3779B97F4A7C15, |d| d.as_nanos() as u64 | 1)
}

fn ncpu() -> f64 {
    std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64)
}

// ---------------------------------------------------------------- estado

struct Model {
    tank: Tank,
    pal: Palette,
    font: String,
    sensors: Sensors,
    sys: SysSampler,
    disks: DiskSampler,
    hover: Option<Pick>,
    cursor: Option<(f64, f64)>,
    names: u8,
    paused: bool,
    last_frame: Option<i64>,
    first_sync: bool,
    gpu: gpu::GpuCache,
    style: Style,
    /// Última muestra de procesos (para la tabla y para cerrar grupos).
    groups: Vec<Group>,
    office: bool,
}

struct Chips {
    temp: gtk::Label,
    cpu: gtk::Label,
    ram: gtk::Label,
    disk: gtk::Label,
    fish: gtk::Label,
}

struct Ui {
    m: Rc<RefCell<Model>>,
    win: adw::ApplicationWindow,
    area: gpu::TankWidget,
    toasts: adw::ToastOverlay,
    provider: gtk::CssProvider,
    chips: Chips,
    style_btn: gtk::Button,
    mode_btn: gtk::Button,
    pages: gtk::Stack,
    office: Rc<office::Office>,
    /// Pestañas del modo Arcade (pecera, volcán).
    arcade_tabs: Vec<gtk::ToggleButton>,
    volcano: Rc<RefCell<volcano::Volcano>>,
    rivers: Rc<RefCell<rivers::Rivers>>,
    weather: Rc<RefCell<weather::Weather>>,
    vpal: Rc<RefCell<Palette>>,
}

fn chip(pal: &Palette, key: &str, val: &str, vcol: Option<Rgb>) -> String {
    format!(
        "<span foreground=\"{}\" size=\"small\" weight=\"bold\">{}</span>  <span foreground=\"{}\" weight=\"bold\">{}</span>",
        to_hex(pal.dfg),
        glib::markup_escape_text(key),
        to_hex(vcol.unwrap_or(pal.bfg)),
        glib::markup_escape_text(val)
    )
}

// ---------------------------------------------------------------- muestreo

fn update_sensors(ui: &Rc<Ui>) {
    let mut guard = ui.m.borrow_mut();
    let m = &mut *guard;
    let r = m.sensors.read();
    let s = m.sys.sample();
    let disks = m.disks.sample();
    ui.rivers.borrow_mut().set_disks(&disks);
    ui.office.refresh_disks(&disks);
    let temp = demo_temp().or(r.cpu);
    m.tank.set_env(temp, r.load / ncpu(), s.ram_used, s.ram_total, s.disk_w);
    ui.office.refresh_perf(&s, &r, temp);
    ui.office.refresh_sensors(&r, temp);
    ui.weather.borrow_mut().set_reading(&r, temp);
    ui.office.refresh_mem(&s);
    // THERMALACUARIUM_DEMO_MEM=0.95 fuerza el uso de RAM para ver el volcán en erupción.
    let demo = std::env::var("THERMALACUARIUM_DEMO_MEM").ok().and_then(|v| v.parse::<f64>().ok());
    let used = demo.map_or(s.ram_used, |f| f * s.ram_total);
    ui.volcano.borrow_mut().set_memory(used, s.ram_total, s.cached, demo.map_or(s.swap_used, |f| f * s.swap_total.max(1.0)), s.swap_total.max(if demo.is_some() { 1.0 } else { 0.0 }));

    let pal = &m.pal;
    let f = |v: Option<f64>| v.map_or("--".to_string(), |v| format!("{v:.0}°"));
    ui.chips.temp.set_markup(&chip(pal, "󰔏", &f(temp), temp.map(|t| pal.heat(t))));
    ui.chips.temp.set_tooltip_text(Some(&trf("sensors_tip", &[f(r.gpu), f(r.nvme)])));
    ui.chips.cpu.set_markup(&chip(pal, "CPU", &format!("{:.0}%", s.cpu_pct), None));
    ui.chips.ram.set_markup(&chip(
        pal,
        "RAM",
        &trf("ram_of", &[format!("{:.0}", s.ram_used / s.ram_total.max(1.0) * 100.0), fmt_bytes(s.ram_total)]),
        None,
    ));
    ui.chips.disk.set_markup(&chip(pal, tr("chip_disk"), &format!("{}/s", fmt_bytes(s.disk_w)), None));
    let n = m.tank.fish.iter().filter(|f| f.life == sim::Life::Alive).count();
    ui.chips.fish.set_markup(&chip(pal, "🐟", &trf("fish_n", &[n.to_string()]).replace("🐟", ""), None));
}

/// Aplica una muestra de procesos (viene del hilo de muestreo).
fn apply_procs(ui: &Rc<Ui>, groups: Vec<Group>) {
    let mut m = ui.m.borrow_mut();
    let first = m.first_sync;
    m.first_sync = false;
    m.tank.sync(&groups, first);
    let ram_total = m.tank.ram_total;
    m.groups = groups;
    ui.volcano.borrow_mut().set_groups(&m.groups);
    ui.rivers.borrow_mut().set_groups(&m.groups);
    ui.office.refresh_disk_top(&m.groups);
    if m.office {
        ui.office.refresh_procs(&m.groups, ram_total);
        ui.office.refresh_top(&m.groups, ram_total);
    }
}

fn mode_name(office: bool) -> &'static str {
    if office { tr("mode_arcade") } else { tr("mode_office") }
}

/// Cambia entre la pecera y el administrador de tareas.
fn toggle_mode(ui: &Rc<Ui>) {
    let office = {
        let mut m = ui.m.borrow_mut();
        m.office = !m.office;
        theme::save_office(m.office);
        m.office
    };
    ui.pages.set_visible_child_name(if office { "office" } else { "tank" });
    ui.mode_btn.set_label(mode_name(office));
    ui.style_btn.set_visible(!office);
    if office {
        let (groups, rt) = {
            let m = ui.m.borrow();
            (m.groups.clone(), m.tank.ram_total)
        };
        ui.office.refresh_procs(&groups, rt);
    }
}

/// Cerrar un grupo entero desde la tabla (con el mismo diálogo que el acuario).
fn confirm_kill_group(ui: &Rc<Ui>, key: &str) {
    let Some(g) = ui.m.borrow().groups.iter().find(|g| g.key == key).cloned() else { return };
    if g.protected {
        ui.toasts.add_toast(adw::Toast::new(&trf("protected_msg", std::slice::from_ref(&g.key))));
        return;
    }
    let pids: Vec<i32> = g.procs.iter().map(|p| p.pid).collect();
    let cpu: f64 = g.procs.iter().map(|p| p.cpu).sum();
    let mem: f64 = g.procs.iter().map(|p| p.mem).sum();
    let body = trf("kill_body", &[g.key.clone(), if pids.len() > 1 { format!("{}…", pids[0]) } else { pids[0].to_string() }, format!("{cpu:.0}"), fmt_bytes(mem)]);
    let d = adw::AlertDialog::new(Some(tr("kill_head")), Some(&body));
    d.add_response("cancel", tr("cancel"));
    d.add_response("all", &if pids.len() > 1 { trf("kill_all", &[pids.len().to_string()]) } else { tr("kill_proc").to_string() });
    d.set_response_appearance("all", adw::ResponseAppearance::Destructive);
    d.set_default_response(Some("cancel"));
    d.set_close_response("cancel");
    let (ui2, key) = (ui.clone(), g.key.clone());
    d.choose(Some(&ui.win), gio::Cancellable::NONE, move |resp| {
        if resp == "all" {
            kill_pids(&ui2, pids, key);
        }
    });
}

fn style_name(s: Style) -> &'static str {
    match s {
        Style::Cartoon => tr("style_toon"),
        Style::Pixel => tr("style_pixel"),
        Style::Sonar => tr("style_sonar"),
        Style::Realistic => tr("style_real"),
    }
}

/// Vigila la carpeta del tema de Omarchy: al ejecutar `omarchy theme set …` el acuario se
/// recolorea al instante (los monitores se vuelven a armar porque Omarchy reemplaza la carpeta
/// `theme` entera y el monitor de la antigua quedaría huérfano).
fn watch_theme(ui: &Rc<Ui>) {
    let monitors: Rc<RefCell<Vec<gio::FileMonitor>>> = Rc::new(RefCell::new(vec![]));
    let pending = Rc::new(std::cell::Cell::new(false));
    fn arm(ui: &Rc<Ui>, monitors: &Rc<RefCell<Vec<gio::FileMonitor>>>, pending: &Rc<std::cell::Cell<bool>>) {
        let cur = theme::omarchy_current_dir();
        let mut v = vec![];
        for dir in [cur.clone(), format!("{cur}/theme")] {
            let Ok(mon) = gio::File::for_path(&dir).monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) else { continue };
            let (u, mons, pend) = (ui.clone(), monitors.clone(), pending.clone());
            mon.connect_changed(move |_, _, _, _| {
                if pend.replace(true) {
                    return; // ya hay una recarga en camino: los eventos llegan en ráfaga
                }
                let (u, mons, pend) = (u.clone(), mons.clone(), pend.clone());
                glib::timeout_add_local_once(Duration::from_millis(400), move || {
                    pend.set(false);
                    apply_theme(&u, false);
                    arm(&u, &mons, &pend);
                });
            });
            v.push(mon);
        }
        *monitors.borrow_mut() = v;
    }
    arm(ui, &monitors, &pending);
    // La referencia vive mientras viva la aplicación.
    std::mem::forget(monitors);
}

/// Cambia entre el estilo estilizado y el realista y recuerda la elección.
fn toggle_style(ui: &Rc<Ui>) {
    let s = {
        let mut m = ui.m.borrow_mut();
        m.style = m.style.toggled();
        m.style.save();
        m.style
    };
    ui.style_btn.set_label(style_name(s));
    ui.toasts.add_toast(adw::Toast::new(&trf("style_toast", &[style_name(s).to_string()])));
    ui.area.queue_draw();
}

const X_URL: &str = "https://x.com/theinaog";

/// Diálogo "Acerca de" con el enlace a la cuenta de X del autor.
fn show_about(ui: &Rc<Ui>) {
    let d = adw::AboutDialog::builder()
        .application_name("ThermalAcuarium")
        .application_icon("utilities-system-monitor")
        .developer_name("@theinaog")
        .version(env!("CARGO_PKG_VERSION"))
        .comments(tr("about_comments"))
        .build();
    d.add_link("X · @theinaog", X_URL);
    d.set_developers(&[&format!("@theinaog {X_URL}")]);
    d.present(Some(&ui.win));
}

fn apply_theme(ui: &Rc<Ui>, force: bool) {
    if !Palette::available() {
        return; // Omarchy está en mitad de un cambio de tema
    }
    let new = Palette::load();
    if !force && ui.m.borrow().pal.raw == new.raw {
        return;
    }
    // Un tema nuevo puede traer otra fuente: se vuelve a preguntar solo cuando cambia.
    let font = if force { ui.m.borrow().font.clone() } else { current_font() };
    #[allow(deprecated)]
    ui.provider.load_from_data(&new.css(&font));
    ui.m.borrow_mut().font = font;
    adw::StyleManager::default().set_color_scheme(if new.dark {
        adw::ColorScheme::ForceDark
    } else {
        adw::ColorScheme::ForceLight
    });
    ui.office.set_palette(&new);
    *ui.vpal.borrow_mut() = new.clone();
    ui.m.borrow_mut().pal = new;
    ui.area.queue_draw();
}

// ---------------------------------------------------------------- interacción

fn refresh_hover(ui: &Rc<Ui>) {
    let (changed, over_fish) = {
        let mut m = ui.m.borrow_mut();
        let pick = m.cursor.and_then(|(x, y)| m.tank.pick(x, y));
        let changed = pick != m.hover;
        m.hover = pick;
        (changed, matches!(pick, Some(Pick::Fish(_))))
    };
    if changed {
        ui.area.set_cursor_from_name(Some(if over_fish { "pointer" } else { "default" }));
        ui.area.queue_draw();
    }
}

fn on_click(ui: &Rc<Ui>, x: f64, y: f64) {
    let pick = ui.m.borrow().tank.pick(x, y);
    match pick {
        Some(Pick::Fish(i)) => confirm_kill(ui, i),
        Some(Pick::Decor(i)) => {
            let name = ui.m.borrow().tank.decor.get(i).map(|d| d.key.clone());
            if let Some(n) = name {
                ui.toasts.add_toast(adw::Toast::new(&trf("protected_msg", &[n])));
            }
        }
        None => {}
    }
}

fn confirm_kill(ui: &Rc<Ui>, i: usize) {
    let (key, pid, cpu, mem, group) = {
        let m = ui.m.borrow();
        let Some(f) = m.tank.fish.get(i) else { return };
        (f.key.clone(), f.pid, f.cpu, f.mem, m.tank.pids_of(f.kh))
    };
    let body = trf("kill_body", &[key.clone(), pid.to_string(), format!("{cpu:.0}"), fmt_bytes(mem)]);
    let d = adw::AlertDialog::new(Some(tr("kill_head")), Some(&body));
    d.add_response("cancel", tr("cancel"));
    if group.len() > 1 {
        d.add_response("one", tr("kill_one"));
        d.add_response("all", &trf("kill_all", &[group.len().to_string()]));
        d.set_response_appearance("all", adw::ResponseAppearance::Destructive);
    } else {
        d.add_response("one", tr("kill_proc"));
    }
    d.set_response_appearance("one", adw::ResponseAppearance::Destructive);
    d.set_default_response(Some("cancel"));
    d.set_close_response("cancel");
    let ui2 = ui.clone();
    d.choose(Some(&ui.win), gio::Cancellable::NONE, move |resp| match resp.as_str() {
        "one" => kill_pids(&ui2, vec![pid], key),
        "all" => kill_pids(&ui2, group, key),
        _ => {}
    });
}

/// SIGTERM ya y, si a los 3 s sigue siendo el mismo proceso, SIGKILL. El pez se pone panza arriba.
fn kill_pids(ui: &Rc<Ui>, pids: Vec<i32>, key: String) {
    let mut targets = vec![];
    for &p in &pids {
        if p > 1
            && let Some(c) = comm_of(p) {
                targets.push((p, c));
                unsafe { libc::kill(p, libc::SIGTERM) };
            }
    }
    ui.m.borrow_mut().tank.mark_dying(&pids);
    ui.toasts.add_toast(adw::Toast::new(&trf("closing", &[key])));

    glib::timeout_add_local_once(Duration::from_secs(3), move || {
        for (p, c) in &targets {
            if comm_of(*p).as_ref() == Some(c) {
                unsafe { libc::kill(*p, libc::SIGKILL) };
            }
        }
    });
}

// ---------------------------------------------------------------- ventana

fn build_ui(app: &adw::Application) {
    if let Some(w) = app.active_window() {
        w.present();
        return;
    }
    let provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("sin display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let m = Rc::new(RefCell::new(Model {
        tank: Tank::new(1180.0, 680.0, seed()),
        pal: Palette::load(),
        font: current_font(),
        sensors: Sensors::new(),
        sys: SysSampler::default(),
        disks: DiskSampler::default(),
        hover: None,
        cursor: None,
        names: 0,
        paused: false,
        last_frame: None,
        first_sync: true,
        gpu: gpu::GpuCache::default(),
        style: start_style(),
        groups: vec![],
        office: has_flag("--office") || theme::load_office(),
    }));

    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title("ThermalAcuarium")
        .default_width(1180)
        .default_height(720)
        .build();
    win.add_css_class("ta");

    let area = gpu::TankWidget::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    // Modo Arcade: pestañas Procesos (pecera) y Memoria (volcán).
    let arcade_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let atabs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    atabs.add_css_class("tabs");
    atabs.set_margin_top(6);
    atabs.set_margin_start(12);
    atabs.set_margin_bottom(6);
    let arcade = gtk::Stack::new();
    arcade.set_vexpand(true);
    let mut arcade_tabs = vec![];
    for (i, key) in ["tab_procs", "tab_mem", "tab_disks", "tab_sensors"].iter().enumerate() {
        let b = gtk::ToggleButton::with_label(tr(key));
        if i == 0 {
            b.set_active(true);
        } else {
            b.set_group(Some(&arcade_tabs[0]));
        }
        atabs.append(&b);
        arcade_tabs.push(b);
    }
    arcade_box.append(&atabs);
    arcade_box.append(&arcade);
    arcade.add_named(&area, Some("tank"));
    let vpal = Rc::new(RefCell::new(m.borrow().pal.clone()));
    let vol = Rc::new(RefCell::new(volcano::Volcano::new()));
    let vol_area = volcano::widget(vol.clone(), vpal.clone(), m.borrow().font.clone());
    arcade.add_named(&vol_area, Some("volcano"));
    let riv = Rc::new(RefCell::new(rivers::Rivers::new()));
    let riv_area = rivers::widget(riv.clone(), vpal.clone(), m.borrow().font.clone());
    arcade.add_named(&riv_area, Some("rivers"));
    let wx = Rc::new(RefCell::new(weather::Weather::new(ncpu())));
    let wx_area = weather::widget(wx.clone(), vpal.clone(), m.borrow().font.clone());
    arcade.add_named(&wx_area, Some("weather"));
    for (i, t) in arcade_tabs.iter().enumerate() {
        let st = arcade.clone();
        t.connect_toggled(move |b| {
            if b.is_active() {
                st.set_visible_child_name(["tank", "volcano", "rivers", "weather"][i]);
            }
        });
    }
    let pages = gtk::Stack::new();
    pages.add_named(&arcade_box, Some("tank"));
    let office = office::Office::new(m.borrow().pal.clone());
    pages.add_named(&office.root, Some("office"));
    pages.set_visible_child_name(if m.borrow().office { "office" } else { "tank" });
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&pages));

    let mk = || {
        let l = gtk::Label::new(None);
        l.add_css_class("chip");
        l
    };
    let chips = Chips { temp: mk(), cpu: mk(), ram: mk(), disk: mk(), fish: mk() };
    let chip_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    for l in [&chips.temp, &chips.cpu, &chips.ram, &chips.disk, &chips.fish] {
        chip_box.append(l);
    }
    let title = gtk::Label::new(Some("THERMALACUARIUM"));
    title.add_css_class("title");
    let header = adw::HeaderBar::new();
    header.pack_start(&title);
    header.set_title_widget(Some(&chip_box));
    let style_btn = gtk::Button::with_label(style_name(m.borrow().style));
    style_btn.add_css_class("flat");
    style_btn.add_css_class("stylebtn");
    style_btn.set_tooltip_text(Some(tr("style_tip")));
    let about_btn = gtk::Button::from_icon_name("help-about-symbolic");
    about_btn.add_css_class("flat");
    about_btn.set_tooltip_text(Some(tr("about")));
    let mode_btn = gtk::Button::with_label(mode_name(m.borrow().office));
    mode_btn.add_css_class("flat");
    mode_btn.add_css_class("stylebtn");
    mode_btn.set_tooltip_text(Some(tr("mode_tip")));
    style_btn.set_visible(!m.borrow().office);
    header.pack_end(&about_btn);
    header.pack_end(&style_btn);
    header.pack_end(&mode_btn);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&toasts));
    win.set_content(Some(&tv));

    let ui = Rc::new(Ui { m: m.clone(), win: win.clone(), area: area.clone(), toasts, provider, chips, style_btn: style_btn.clone(), mode_btn: mode_btn.clone(), pages, office: office.clone(), arcade_tabs, volcano: vol, rivers: riv, weather: wx, vpal });
    let u = ui.clone();
    mode_btn.connect_clicked(move |_| toggle_mode(&u));
    let u = ui.clone();
    *office.on_close.borrow_mut() = Some(Rc::new(move |k: &str| confirm_kill_group(&u, k)));
    let u = ui.clone();
    style_btn.connect_clicked(move |_| toggle_style(&u));
    let u = ui.clone();
    about_btn.connect_clicked(move |_| show_about(&u));

    // Dibujo: el árbol de nodos GSK de cada fotograma (la GPU compone, ver gpu.rs).
    let md = m.clone();
    area.set_render(move |w, snap| {
        let mut guard = md.borrow_mut();
        let m = &mut *guard;
        m.tank.resize(WidgetExt::width(w) as f64, WidgetExt::height(w) as f64);
        let v = View { tank: &m.tank, pal: &m.pal, font: &m.font, hover: m.hover, names: m.names, paused: m.paused, style: m.style };
        gpu::render(w.upcast_ref(), snap, &v, &mut m.gpu);
    });

    // Reloj de animación: sincronizado con el frame clock y limitado a ~30 FPS para no gastar CPU.
    let u = ui.clone();
    area.add_tick_callback(move |_, clock| {
        let now = clock.frame_time();
        {
            let mut m = u.m.borrow_mut();
            if m.paused {
                m.last_frame = None;
                return glib::ControlFlow::Continue;
            }
            let dt = m.last_frame.map_or(0.0, |l| (now - l) as f64 / 1e6);
            if m.last_frame.is_some() && dt < 1.0 / 31.0 {
                return glib::ControlFlow::Continue;
            }
            m.last_frame = Some(now);
            m.tank.step(dt);
        }
        refresh_hover(&u);
        u.area.queue_draw();
        glib::ControlFlow::Continue
    });

    let motion = gtk::EventControllerMotion::new();
    let u = ui.clone();
    motion.connect_motion(move |_, x, y| {
        u.m.borrow_mut().cursor = Some((x, y));
        refresh_hover(&u);
    });
    let u = ui.clone();
    motion.connect_leave(move |_| {
        u.m.borrow_mut().cursor = None;
        refresh_hover(&u);
    });
    area.add_controller(motion);

    let click = gtk::GestureClick::new();
    click.set_button(1);
    let u = ui.clone();
    click.connect_pressed(move |_, _, x, y| on_click(&u, x, y));
    area.add_controller(click);

    // L: nombres · Espacio: pausa
    let keys = gtk::EventControllerKey::new();
    let u = ui.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        match key {
            gdk::Key::l | gdk::Key::L => {
                let mut m = u.m.borrow_mut();
                m.names = (m.names + 1) % 3;
            }
            gdk::Key::t | gdk::Key::T => toggle_style(&u),
            gdk::Key::o | gdk::Key::O => toggle_mode(&u),
            gdk::Key::_1 | gdk::Key::_2 | gdk::Key::_3 | gdk::Key::_4 | gdk::Key::_5 => {
                let i = match key {
                    gdk::Key::_1 => 0,
                    gdk::Key::_2 => 1,
                    gdk::Key::_3 => 2,
                    gdk::Key::_4 => 3,
                    _ => 4,
                };
                if u.m.borrow().office {
                    u.office.show_tab(i);
                } else if let Some(b) = u.arcade_tabs.get(i) {
                    b.set_active(true);
                }
            }
            gdk::Key::space => {
                let mut m = u.m.borrow_mut();
                m.paused = !m.paused;
            }
            _ => return glib::Propagation::Proceed,
        }
        u.area.queue_draw();
        glib::Propagation::Stop
    });
    win.add_controller(keys);

    apply_theme(&ui, true);
    update_sensors(&ui);
    // El muestreo de /proc va en su propio hilo (lee cientos de archivos): la interfaz no se
    // bloquea nunca. La primera muestra solo sirve de referencia para medir la CPU.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<Group>>();
    std::thread::Builder::new()
        .name("procs".into())
        .spawn(move || {
            let mut sampler = ProcSampler::new();
            sampler.sample();
            std::thread::sleep(Duration::from_millis(700));
            loop {
                if tx.send(sampler.sample()).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .expect("hilo de procesos");
    let u = ui.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        let mut last = None;
        while let Ok(g) = rx.try_recv() {
            last = Some(g);
        }
        if let Some(g) = last {
            apply_procs(&u, g);
        }
        glib::ControlFlow::Continue
    });
    let u = ui.clone();
    glib::timeout_add_seconds_local(1, move || {
        update_sensors(&u);
        glib::ControlFlow::Continue
    });
    // Respaldo por si el sistema de archivos no avisa (p. ej. temas editados desde fuera).
    watch_theme(&ui);
    let u = ui.clone();
    glib::timeout_add_seconds_local(10, move || {
        apply_theme(&u, false);
        glib::ControlFlow::Continue
    });

    win.present();
}

// ---------------------------------------------------------------- captura sin ventana

/// `--snapshot out.png [--size 1180x680] [--seconds 8] [--temp 95] [--demo] [--dying N] [--hover x,y] [--names 0|1|2]`
/// Renderiza la pecera real a un PNG sin abrir ventana: sirve para probar el aspecto.
fn snapshot(path: &str) {
    let (w, h) = arg_val("--size")
        .and_then(|s| {
            let (a, b) = s.split_once('x')?;
            Some((a.parse::<i32>().ok()?, b.parse::<i32>().ok()?))
        })
        .unwrap_or((1180, 680));
    let secs: f64 = arg_val("--seconds").and_then(|s| s.parse().ok()).unwrap_or(8.0);
    let pal = Palette::load();
    let font = current_font();
    let style = start_style();
    if has_flag("--gallery") {
        let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).expect("surface");
        {
            let cr = cairo::Context::new(&surf).expect("cairo");
            let len: f64 = arg_val("--len").and_then(|s| s.parse().ok()).unwrap_or(150.0);
            if style == Style::Pixel {
                pixel::gallery(&cr, w as f64, h as f64, len, &font, &pal);
            } else {
                reef::scene::gallery(&cr, w as f64, h as f64, len, &font, &pal);
            }
        }
        let mut file = std::fs::File::create(path).expect("no se pudo crear el PNG");
        surf.write_to_png(&mut file).expect("no se pudo escribir el PNG");
        return;
    }
    let mut tank = Tank::new(w as f64, h as f64, 0xC0FFEE);
    let sensors = Sensors::new();
    let mut sys = SysSampler::default();
    let mut procs = ProcSampler::new();
    procs.sample();
    sys.sample();
    std::thread::sleep(Duration::from_millis(600));
    let s = sys.sample();
    let r = sensors.read();
    tank.set_env(demo_temp().or(r.cpu), r.load / ncpu(), s.ram_used, s.ram_total, s.disk_w);
    tank.sync(&procs.sample(), true);
    if has_flag("--demo") {
        tank.demo_randomize();
    }
    if let Some(n) = arg_val("--dying").and_then(|s| s.parse::<usize>().ok()) {
        // Solo visual: marca los n peces más grandes como cerrados, sin tocar ningún proceso.
        let mut by_size: Vec<(f64, i32)> = tank.fish.iter().map(|f| (f.len, f.pid)).collect();
        by_size.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        tank.mark_dying(&by_size.iter().take(n).map(|x| x.1).collect::<Vec<_>>());
    }
    for _ in 0..(secs * 30.0) as usize {
        tank.step(1.0 / 30.0);
    }
    let hover = arg_val("--hover").and_then(|s| {
        let (a, b) = s.split_once(',')?;
        tank.pick(a.parse().ok()?, b.parse().ok()?)
    });
    let names = arg_val("--names").and_then(|s| s.parse().ok()).unwrap_or(0);

    let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).expect("surface");
    let cr = cairo::Context::new(&surf).expect("cairo");
    let view = View { tank: &tank, pal: &pal, font: &font, hover, names, paused: false, style };
    if has_flag("--bench") {
        // Fotograma a fotograma como en la app (paso + dibujo), para ver el coste de calentar la caché.
        let mut times = vec![];
        let mut bench = Tank::new(w as f64, h as f64, 0xC0FFEE);
        bench.set_env(demo_temp().or(r.cpu), r.load / ncpu(), s.ram_used, s.ram_total, s.disk_w);
        bench.sync(&procs.sample(), true);
        bench.demo_randomize();
        for _ in 0..180 {
            bench.step(1.0 / 30.0);
            let v = View { tank: &bench, pal: &pal, font: &font, hover: None, names: 0, paused: false, style };
            let t = std::time::Instant::now();
            draw::draw_tank(&cr, &v);
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let avg = |r: &[f64]| r.iter().sum::<f64>() / r.len() as f64;
        eprintln!(
            "bench: primeros 30 fotogramas {:.1} ms · últimos 90 {:.1} ms (máx {:.1}) · máx total {:.1} ms",
            avg(&times[..30]),
            avg(&times[90..]),
            times[90..].iter().cloned().fold(0.0, f64::max),
            times.iter().cloned().fold(0.0, f64::max)
        );
    }
    let t0 = std::time::Instant::now();
    let reps = 20;
    for _ in 0..reps {
        draw::draw_tank(&cr, &view);
    }
    eprintln!(
        "{} peces · {} burbujas · hervor {:.2} · {:.1} ms/fotograma",
        tank.fish.len(),
        tank.bubbles.len(),
        tank.boil,
        t0.elapsed().as_secs_f64() * 1000.0 / reps as f64
    );
    drop(cr);
    let mut file = std::fs::File::create(path).expect("no se pudo crear el PNG");
    surf.write_to_png(&mut file).expect("no se pudo escribir el PNG");
}

/// `--bench-scene volcano|rivers|weather`: dibuja 40 fotogramas de esa pestaña a 1180×680 sin ventana
/// y dice cuánto cuesta cada uno (para optimizar sin adivinar).
fn bench_scene(which: &str) {
    let pal = Palette::load();
    let font = current_font();
    let (w, h) = (1180, 680);
    let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).expect("surface");
    let cr = cairo::Context::new(&surf).expect("cairo");
    let mut procs = ProcSampler::new();
    procs.sample();
    std::thread::sleep(Duration::from_millis(400));
    let groups = procs.sample();
    let mut sys = SysSampler::default();
    let s = sys.sample();
    let r = Sensors::new().read();
    let disks = DiskSampler::default().sample();
    let mut draw: Box<dyn FnMut(&cairo::Context, f64)> = match which {
        "volcano" => {
            let mut v = volcano::Volcano::new();
            v.resize(w as f64, h as f64);
            v.set_memory(s.ram_total * 0.8, s.ram_total, s.cached, s.swap_used, s.swap_total);
            v.set_groups(&groups);
            Box::new(move |cr, dt| {
                v.step(dt);
                v.draw(cr, &pal, &font);
            })
        }
        "rivers" => {
            let mut v = rivers::Rivers::new();
            v.resize(w as f64, h as f64);
            v.set_disks(&disks);
            v.set_groups(&groups);
            Box::new(move |cr, dt| {
                v.step(dt);
                v.draw(cr, &pal, &font);
            })
        }
        _ => {
            let mut v = weather::Weather::new(ncpu());
            v.resize(w as f64, h as f64);
            v.set_reading(&r, Some(84.0));
            Box::new(move |cr, dt| {
                v.step(dt);
                v.draw(cr, &pal, &font);
            })
        }
    };
    draw(&cr, 0.0); // hornea las capas estáticas
    let t0 = std::time::Instant::now();
    for _ in 0..40 {
        draw(&cr, 1.0 / 30.0);
    }
    eprintln!("{which}: {:.1} ms/fotograma", t0.elapsed().as_secs_f64() * 1000.0 / 40.0);
}

fn main() -> glib::ExitCode {
    if let Some(which) = arg_val("--bench-scene") {
        bench_scene(&which);
        return glib::ExitCode::SUCCESS;
    }
    if let Some(p) = arg_val("--snapshot") {
        snapshot(&p);
        return glib::ExitCode::SUCCESS;
    }
    // `--new-instance`: no registrarse en el bus, para poder abrir otra ventana (o probar) sin
    // activar la que ya esté abierta.
    let flags = if has_flag("--new-instance") { gio::ApplicationFlags::NON_UNIQUE } else { gio::ApplicationFlags::default() };
    let app = adw::Application::builder().application_id(APP_ID).flags(flags).build();
    app.connect_activate(build_ui);
    app.run_with_args::<String>(&[])
}
