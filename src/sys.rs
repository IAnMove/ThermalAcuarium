//! Telemetría real: sensores (hwmon), CPU/RAM/disco globales y procesos (/proc).

use std::collections::HashMap;
use std::fs;
use std::time::Instant;

/// Procesos de la sesión de escritorio: salen en la pecera como decoración y no se pueden cerrar.
pub const PROTECTED: &[&str] = &[
    "Hyprland", "hyprland", "systemd", "(sd-pam)", "quickshell", "pipewire", "wireplumber",
    "dbus-broker", "dbus-broker-lau", "Xwayland", "pipewire-pulse", "gnome-keyring-d",
    "xdg-desktop-por", "hyprlock", "hypridle", "mako", "waybar",
];

fn rd_int(path: &str) -> Option<i64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

// ---------------------------------------------------------------- sensores

#[derive(Default, Clone)]
pub struct Reading {
    pub cpu: Option<f64>,
    pub gpu: Option<f64>,
    pub nvme: Option<f64>,
    pub load: f64,
}

pub struct Sensors {
    by_name: HashMap<String, Vec<String>>,
}

impl Sensors {
    pub fn new() -> Self {
        let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
        if let Ok(rd) = fs::read_dir("/sys/class/hwmon") {
            for e in rd.flatten() {
                let p = e.path();
                if let Ok(n) = fs::read_to_string(p.join("name")) {
                    by_name.entry(n.trim().to_string()).or_default().push(p.to_string_lossy().into_owned());
                }
            }
        }
        Sensors { by_name }
    }

    fn temp(&self, name: &str) -> Option<f64> {
        self.by_name
            .get(name)?
            .iter()
            .find_map(|p| rd_int(&format!("{p}/temp1_input")))
            .map(|v| v as f64 / 1000.0)
    }

    pub fn read(&self) -> Reading {
        let cpu = self.temp("k10temp").or_else(|| self.temp("coretemp")).or_else(|| self.temp("acpitz"));
        let load = fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|x| x.parse().ok()))
            .unwrap_or(0.0);
        Reading { cpu, gpu: self.temp("amdgpu"), nvme: self.temp("nvme"), load }
    }
}

// ---------------------------------------------------------------- sistema

/// CPU total, RAM y disco (lectura/escritura) del sistema.
#[derive(Default, Clone)]
pub struct SysReading {
    pub cpu_pct: f64,
    pub ram_used: f64,
    pub ram_total: f64,
    pub disk_w: f64,
    pub disk_r: f64,
}

#[derive(Default)]
pub struct SysSampler {
    prev_cpu: Option<(u64, u64)>,
    prev_disk: Option<(u64, u64, Instant)>,
}

fn is_whole_disk(n: &str) -> bool {
    if n.starts_with("nvme") || n.starts_with("mmcblk") {
        !n[4..].contains('p')
    } else if n.starts_with("sd") || n.starts_with("vd") || n.starts_with("hd") || n.starts_with("xvd") {
        !n.chars().last().is_none_or(|c| c.is_ascii_digit())
    } else {
        false
    }
}

impl SysSampler {
    pub fn sample(&mut self) -> SysReading {
        let mut r = SysReading::default();
        if let Ok(s) = fs::read_to_string("/proc/stat")
            && let Some(l) = s.lines().next() {
                let v: Vec<u64> = l.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
                if v.len() >= 8 {
                    let idle = v[3] + v[4];
                    let total: u64 = v[..8].iter().sum();
                    if let Some((pi, pt)) = self.prev_cpu {
                        let dt = total.saturating_sub(pt) as f64;
                        if dt > 0.0 {
                            r.cpu_pct = (1.0 - idle.saturating_sub(pi) as f64 / dt) * 100.0;
                        }
                    }
                    self.prev_cpu = Some((idle, total));
                }
            }
        if let Ok(s) = fs::read_to_string("/proc/meminfo") {
            let (mut total, mut avail) = (0.0, 0.0);
            for l in s.lines() {
                let mut it = l.split_whitespace();
                match it.next() {
                    Some("MemTotal:") => total = it.next().and_then(|x| x.parse().ok()).unwrap_or(0.0),
                    Some("MemAvailable:") => avail = it.next().and_then(|x| x.parse().ok()).unwrap_or(0.0),
                    _ => {}
                }
            }
            r.ram_total = total * 1024.0;
            r.ram_used = (total - avail) * 1024.0;
        }
        if let Ok(s) = fs::read_to_string("/proc/diskstats") {
            let (mut w, mut rd) = (0u64, 0u64);
            for l in s.lines() {
                let p: Vec<&str> = l.split_whitespace().collect();
                if p.len() > 9 && is_whole_disk(p[2]) {
                    rd += p[5].parse::<u64>().unwrap_or(0) * 512;
                    w += p[9].parse::<u64>().unwrap_or(0) * 512;
                }
            }
            let now = Instant::now();
            if let Some((pr, pw, pt)) = self.prev_disk {
                let dt = now.duration_since(pt).as_secs_f64();
                if dt > 0.0 {
                    r.disk_w = w.saturating_sub(pw) as f64 / dt;
                    r.disk_r = rd.saturating_sub(pr) as f64 / dt;
                }
            }
            self.prev_disk = Some((rd, w, now));
        }
        r
    }
}

// ---------------------------------------------------------------- procesos

/// Un proceso real: un pez. `start` (starttime del kernel) distingue PIDs reutilizados.
#[derive(Clone)]
pub struct Proc {
    pub pid: i32,
    pub start: u64,
    pub cpu: f64,
    pub mem: f64,
    pub wbps: f64,
}

/// Procesos con el mismo nombre: un banco.
#[derive(Clone)]
pub struct Group {
    pub key: String,
    pub procs: Vec<Proc>,
    pub protected: bool,
}

pub struct ProcSampler {
    prev: HashMap<i32, (u64, u64, u64)>, // pid -> (start, ticks, bytes escritos)
    prev_t: Option<Instant>,
    clk: f64,
    page: f64,
    uid: u32,
    self_pid: i32,
}

fn read_write_bytes(pid: i32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/io"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("write_bytes:").and_then(|v| v.trim().parse().ok()))
        })
        .unwrap_or(0)
}

impl ProcSampler {
    pub fn new() -> Self {
        ProcSampler {
            prev: HashMap::new(),
            prev_t: None,
            clk: unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64,
            page: unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as f64,
            uid: unsafe { libc::getuid() },
            self_pid: std::process::id() as i32,
        }
    }

    fn key(comm: &str, pid: i32) -> String {
        if (comm.starts_with("chrom") || comm.starts_with("brave"))
            && let Ok(c) = fs::read(format!("/proc/{pid}/cmdline"))
                && c.windows(8).any(|w| w == b"headless") {
                    return "chromium headless".into();
                }
        comm.to_string()
    }

    /// Procesos del usuario agrupados por nombre; CPU y escritura son medias desde la muestra anterior.
    pub fn sample(&mut self) -> Vec<Group> {
        use std::os::unix::fs::MetadataExt;
        let now = Instant::now();
        let dt = self.prev_t.map(|t| now.duration_since(t).as_secs_f64());
        let mut cur = HashMap::new();
        let mut groups: HashMap<String, Group> = HashMap::new();
        if let Ok(rd) = fs::read_dir("/proc") {
            for e in rd.flatten() {
                let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
                if pid == self.self_pid {
                    continue;
                }
                let Ok(md) = fs::metadata(format!("/proc/{pid}")) else { continue };
                if md.uid() != self.uid {
                    continue;
                }
                let Ok(s) = fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
                let (Some(a), Some(b)) = (s.find('('), s.rfind(')')) else { continue };
                let comm = &s[a + 1..b];
                let rest: Vec<&str> = s[b + 2..].split_whitespace().collect();
                let (Some(u), Some(k), Some(st), Some(rss)) = (rest.get(11), rest.get(12), rest.get(19), rest.get(21))
                else {
                    continue;
                };
                let (Ok(u), Ok(k), Ok(start), Ok(rss)) =
                    (u.parse::<u64>(), k.parse::<u64>(), st.parse::<u64>(), rss.parse::<f64>())
                else {
                    continue;
                };
                let ticks = u + k;
                let prev = self.prev.get(&pid).filter(|p| p.0 == start).copied();
                let prev_tick = prev.map_or(ticks, |p| p.1);
                // Solo se mira el I/O si el proceso corrió o tiene memoria apreciable.
                let wb = if ticks > prev_tick || rss > 2500.0 {
                    read_write_bytes(pid)
                } else {
                    prev.map_or(0, |p| p.2)
                };
                cur.insert(pid, (start, ticks, wb));
                let mut p = Proc { pid, start, cpu: 0.0, mem: rss.max(0.0) * self.page, wbps: 0.0 };
                if let (Some(dt), Some((_, pt, pw))) = (dt, prev)
                    && dt > 0.0 {
                        p.cpu = ticks.saturating_sub(pt) as f64 / self.clk / dt * 100.0;
                        p.wbps = wb.saturating_sub(pw) as f64 / dt;
                    }
                let key = Self::key(comm, pid);
                groups
                    .entry(key.clone())
                    .or_insert_with(|| Group { protected: PROTECTED.contains(&comm), key, procs: vec![] })
                    .procs
                    .push(p);
            }
        }
        self.prev = cur;
        self.prev_t = Some(now);
        groups.into_values().collect()
    }
}

pub fn comm_of(pid: i32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/comm")).ok()
}

pub fn fmt_bytes(b: f64) -> String {
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", b / 1e3)
    }
}
