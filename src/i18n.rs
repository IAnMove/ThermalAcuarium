//! Idioma del sistema (THERMALACUARIUM_LANG, LANGUAGE, LC_ALL, LC_MESSAGES, LANG, /etc/locale.conf).

use std::fs;

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    En,
    Es,
    Fr,
    De,
    Pt,
    It,
}

fn lang() -> Lang {
    static L: std::sync::OnceLock<Lang> = std::sync::OnceLock::new();
    *L.get_or_init(|| {
        let mut cands: Vec<String> = ["THERMALACUARIUM_LANG", "LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .filter(|v| !v.is_empty())
            .collect();
        if let Ok(c) = fs::read_to_string("/etc/locale.conf") {
            for l in c.lines() {
                if let Some(v) = l.strip_prefix("LANG=") {
                    cands.push(v.trim().trim_matches('"').to_string());
                }
            }
        }
        for c in cands {
            for part in c.split(':') {
                let code = part.to_lowercase();
                if code == "c" || code.starts_with("c.") || code == "posix" || code.len() < 2 {
                    continue;
                }
                match code.get(..2) {
                    Some("es") => return Lang::Es,
                    Some("fr") => return Lang::Fr,
                    Some("de") => return Lang::De,
                    Some("pt") => return Lang::Pt,
                    Some("it") => return Lang::It,
                    Some("en") => return Lang::En,
                    _ => {}
                }
            }
        }
        Lang::En
    })
}

// clave -> [en, es, fr, de, pt, it]
const STR: &[(&str, [&str; 6])] = &[
    ("about", ["About", "Acerca de", "À propos", "Info", "Sobre", "Informazioni"]),
    ("about_comments", [
        "An aquarium that is your computer: every process is a fish",
        "Un acuario que es tu ordenador: cada proceso es un pez",
        "Un aquarium qui est votre ordinateur : chaque processus est un poisson",
        "Ein Aquarium, das dein Computer ist: Jeder Prozess ist ein Fisch",
        "Um aquário que é o seu computador: cada processo é um peixe",
        "Un acquario che è il tuo computer: ogni processo è un pesce",
    ]),
    ("style_real", ["REALISTIC", "REALISTA", "RÉALISTE", "REALISTISCH", "REALISTA", "REALISTICO"]),
    ("mode_office", ["OFFICE", "OFICINA", "BUREAU", "BÜRO", "ESCRITÓRIO", "UFFICIO"]),
    ("mode_arcade", ["ARCADE", "ARCADE", "ARCADE", "ARCADE", "ARCADE", "ARCADE"]),
    ("mode_tip", ["Arcade / office mode (O)", "Modo arcade / oficina (O)", "Mode arcade / bureau (O)", "Arcade-/Büromodus (O)", "Modo arcade / escritório (O)", "Modalità arcade / ufficio (O)"]),
    ("tab_procs", ["PROCESSES", "PROCESOS", "PROCESSUS", "PROZESSE", "PROCESSOS", "PROCESSI"]),
    ("tab_sensors", ["SENSORS", "SENSORES", "CAPTEURS", "SENSOREN", "SENSORES", "SENSORI"]),
    ("sens_cpu", ["CPU TEMPERATURE", "TEMPERATURA CPU", "TEMPÉRATURE CPU", "CPU-TEMPERATUR", "TEMPERATURA CPU", "TEMPERATURA CPU"]),
    ("sens_gpu", ["GPU TEMPERATURE", "TEMPERATURA GPU", "TEMPÉRATURE GPU", "GPU-TEMPERATUR", "TEMPERATURA GPU", "TEMPERATURA GPU"]),
    ("sens_nvme", ["NVME TEMPERATURE", "TEMPERATURA NVME", "TEMPÉRATURE NVME", "NVME-TEMPERATUR", "TEMPERATURA NVME", "TEMPERATURA NVME"]),
    ("sens_fan", ["FAN", "VENTILADOR", "VENTILATEUR", "LÜFTER", "VENTOINHA", "VENTOLA"]),
    ("sens_load", ["LOAD AVERAGE", "CARGA MEDIA", "CHARGE MOYENNE", "DURCHSCHNITTSLAST", "CARGA MÉDIA", "CARICO MEDIO"]),
    ("wx_cool", ["CLEAR · COOL", "DESPEJADO · FRESCO", "DÉGAGÉ · FRAIS", "KLAR · KÜHL", "LIMPO · FRESCO", "SERENO · FRESCO"]),
    ("wx_warm", ["HAZE · WARM", "CALIMA · TEMPLADO", "BRUME · TIÈDE", "DUNST · WARM", "NÉVOA · MORNO", "FOSCHIA · TIEPIDO"]),
    ("wx_hot", ["HEAT WAVE · HOT", "OLA DE CALOR · CALIENTE", "CANICULE · CHAUD", "HITZEWELLE · HEISS", "ONDA DE CALOR · QUENTE", "ONDATA DI CALORE · CALDO"]),
    ("wx_crit", ["FIRESTORM · CRITICAL", "TORMENTA DE FUEGO · CRÍTICO", "TEMPÊTE DE FEU · CRITIQUE", "FEUERSTURM · KRITISCH", "TEMPESTADE DE FOGO · CRÍTICO", "TEMPESTA DI FUOCO · CRITICO"]),
    ("wx_wind", ["wind {} rpm", "viento {} rpm", "vent {} rpm", "Wind {} rpm", "vento {} rpm", "vento {} rpm"]),
    ("wx_clouds", ["clouds: load {}", "nubes: carga {}", "nuages : charge {}", "Wolken: Last {}", "nuvens: carga {}", "nuvole: carico {}"]),
    ("tab_disks", ["DISKS", "DISCOS", "DISQUES", "DATENTRÄGER", "DISCOS", "DISCHI"]),
    ("disk_free", ["FREE", "LIBRE", "LIBRE", "FREI", "LIVRE", "LIBERO"]),
    ("disk_used", ["USED", "USADO", "UTILISÉ", "BELEGT", "USADO", "USATO"]),
    ("disk_read", ["READ", "LECTURA", "LECTURE", "LESEN", "LEITURA", "LETTURA"]),
    ("disk_write_t", ["WRITE", "ESCRITURA", "ÉCRITURE", "SCHREIBEN", "ESCRITA", "SCRITTURA"]),
    ("disk_top", ["TOP DISK ACTIVITY", "MÁS ACTIVIDAD DE DISCO", "PLUS D'ACTIVITÉ DISQUE", "MEISTE DISK-AKTIVITÄT", "MAIS ATIVIDADE DE DISCO", "PIÙ ATTIVITÀ DISCO"]),
    ("tab_mem", ["MEMORY", "MEMORIA", "MÉMOIRE", "SPEICHER", "MEMÓRIA", "MEMORIA"]),
    ("mem_cached", ["cache {}", "caché {}", "cache {}", "Cache {}", "cache {}", "cache {}"]),
    ("mem_swap", ["swap {} of {}", "swap {} de {}", "swap {} sur {}", "Swap {} von {}", "swap {} de {}", "swap {} di {}"]),
    ("mem_used", ["IN USE", "EN USO", "UTILISÉE", "BELEGT", "EM USO", "IN USO"]),
    ("mem_cache", ["CACHE & BUFFERS", "CACHÉ Y BÚFERES", "CACHE ET TAMPONS", "CACHE & PUFFER", "CACHE E BUFFERS", "CACHE E BUFFER"]),
    ("mem_free", ["AVAILABLE", "DISPONIBLE", "DISPONIBLE", "VERFÜGBAR", "DISPONÍVEL", "DISPONIBILE"]),
    ("mem_swap_t", ["SWAP", "SWAP", "SWAP", "SWAP", "SWAP", "SWAP"]),
    ("mem_top", ["TOP MEMORY", "MÁS MEMORIA", "PLUS DE MÉMOIRE", "MEISTER SPEICHER", "MAIS MEMÓRIA", "PIÙ MEMORIA"]),
    ("tab_perf", ["PERFORMANCE", "RENDIMIENTO", "PERFORMANCES", "LEISTUNG", "DESEMPENHO", "PRESTAZIONI"]),
    ("search", ["Filter by name…", "Filtrar por nombre…", "Filtrer par nom…", "Nach Name filtern…", "Filtrar por nome…", "Filtra per nome…"]),
    ("col_name", ["NAME", "NOMBRE", "NOM", "NAME", "NOME", "NOME"]),
    ("col_n", ["PROCS", "PROCS", "PROCS", "PROZ.", "PROCS", "PROC."]),
    ("col_pid", ["PID", "PID", "PID", "PID", "PID", "PID"]),
    ("col_cpu", ["CPU", "CPU", "CPU", "CPU", "CPU", "CPU"]),
    ("col_ram", ["RAM", "RAM", "RAM", "RAM", "RAM", "RAM"]),
    ("col_write", ["WRITE", "ESCRITURA", "ÉCRITURE", "SCHREIBEN", "ESCRITA", "SCRITTURA"]),
    ("close_caps", ["CLOSE", "CERRAR", "FERMER", "SCHLIESSEN", "FECHAR", "CHIUDI"]),
    ("disk_w", ["DISK WRITE", "DISCO ESC.", "DISQUE ÉCR.", "DISK SCHR.", "DISCO ESC.", "DISCO SCR."]),
    ("temp", ["TEMPERATURE", "TEMPERATURA", "TEMPÉRATURE", "TEMPERATUR", "TEMPERATURA", "TEMPERATURA"]),
    ("load", ["load {}", "carga {}", "charge {}", "Last {}", "carga {}", "carico {}"]),
    ("read", ["read {}/s", "lectura {}/s", "lecture {}/s", "Lesen {}/s", "leitura {}/s", "lettura {}/s"]),
    ("style_sonar", ["SONAR", "SONAR", "SONAR", "SONAR", "SONAR", "SONAR"]),
    ("style_pixel", ["PIXEL", "PÍXEL", "PIXEL", "PIXEL", "PIXEL", "PIXEL"]),
    ("style_toon", ["STYLIZED", "ESTILIZADO", "STYLISÉ", "STILISIERT", "ESTILIZADO", "STILIZZATO"]),
    ("style_tip", ["Switch visual style (T)", "Cambiar de estilo visual (T)", "Changer de style visuel (T)", "Darstellung wechseln (T)", "Mudar o estilo visual (T)", "Cambia stile visivo (T)"]),
    ("style_toast", ["Style: {}", "Estilo: {}", "Style : {}", "Stil: {}", "Estilo: {}", "Stile: {}"]),
    ("paused", ["PAUSED", "PAUSA", "PAUSE", "PAUSE", "PAUSADO", "PAUSA"]),
    ("chip_disk", ["DISK W", "DISCO E", "DISQUE É", "DISK S", "DISCO E", "DISCO S"]),
    ("ram_of", ["{}% of {}", "{}% de {}", "{}% sur {}", "{}% von {}", "{}% de {}", "{}% di {}"]),
    ("fish_n", ["{} fish", "{} peces", "{} poissons", "{} Fische", "{} peixes", "{} pesci"]),
    ("hidden", ["+{} not shown", "+{} sin mostrar", "+{} masqués", "+{} ausgeblendet", "+{} ocultos", "+{} nascosti"]),
    ("shoal", ["shoal of {}", "banco de {}", "banc de {}", "Schwarm aus {}", "cardume de {}", "banco di {}"]),
    ("write_s", ["write {}/s", "escribe {}/s", "écrit {}/s", "schreibt {}/s", "grava {}/s", "scrive {}/s"]),
    ("protected_tag", ["protected", "protegido", "protégé", "geschützt", "protegido", "protetto"]),
    ("sensors_tip", ["GPU {} · NVMe {}", "GPU {} · NVMe {}", "GPU {} · NVMe {}", "GPU {} · NVMe {}", "GPU {} · NVMe {}", "GPU {} · NVMe {}"]),
    ("kill_head", [
        "Close this process?",
        "¿Cerrar este proceso?",
        "Fermer ce processus ?",
        "Prozess beenden?",
        "Fechar este processo?",
        "Chiudere questo processo?",
    ]),
    ("kill_body", [
        "{}\nPID {} · {}% CPU · {}\n\nA close request will be sent; if it doesn't respond within 3 s, it will be forced.",
        "{}\nPID {} · {}% CPU · {}\n\nSe enviará una orden de cierre y, si no responde en 3 s, se forzará.",
        "{}\nPID {} · {}% CPU · {}\n\nUne demande de fermeture sera envoyée ; sans réponse sous 3 s, l'arrêt sera forcé.",
        "{}\nPID {} · {}% CPU · {}\n\nEs wird eine Beenden-Anfrage gesendet; ohne Antwort innerhalb von 3 s wird erzwungen.",
        "{}\nPID {} · {}% CPU · {}\n\nSerá enviado um pedido de encerramento e, se não responder em 3 s, será forçado.",
        "{}\nPID {} · {}% CPU · {}\n\nVerrà inviata una richiesta di chiusura e, se non risponde entro 3 s, verrà forzata.",
    ]),
    ("kill_one", ["Close this one", "Cerrar solo este", "Fermer celui-ci", "Nur diesen beenden", "Fechar só este", "Chiudi solo questo"]),
    ("kill_proc", ["Close process", "Cerrar proceso", "Fermer le processus", "Prozess beenden", "Fechar processo", "Chiudi processo"]),
    ("kill_all", ["Close all {}", "Cerrar los {}", "Fermer les {}", "Alle {} beenden", "Fechar os {}", "Chiudi tutti i {}"]),
    ("cancel", ["Cancel", "Cancelar", "Annuler", "Abbrechen", "Cancelar", "Annulla"]),
    ("closing", ["Closing {}…", "Cerrando {}…", "Fermeture de {}…", "{} wird beendet…", "Fechando {}…", "Chiusura di {}…"]),
    ("protected_msg", [
        "{} is part of your desktop session: it lives in the tank as decoration and can't be closed from here",
        "{} forma parte de tu sesión de escritorio: vive en la pecera como decoración y no se puede cerrar desde aquí",
        "{} fait partie de votre session de bureau : il sert de décor et ne peut pas être fermé d'ici",
        "{} gehört zu deiner Desktop-Sitzung: Es dient als Dekoration und kann hier nicht beendet werden",
        "{} faz parte da sua sessão de desktop: serve de decoração e não pode ser fechado daqui",
        "{} fa parte della sessione desktop: è solo decorazione e non può essere chiuso da qui",
    ]),
];

fn idx() -> usize {
    match lang() {
        Lang::En => 0,
        Lang::Es => 1,
        Lang::Fr => 2,
        Lang::De => 3,
        Lang::Pt => 4,
        Lang::It => 5,
    }
}

pub fn tr(k: &str) -> &'static str {
    STR.iter().find(|(key, _)| *key == k).map_or("?", |(_, v)| v[idx()])
}

/// Traduce y sustituye cada `{}` por el siguiente argumento.
pub fn trf(k: &str, args: &[String]) -> String {
    let mut s = tr(k).to_string();
    for a in args {
        if let Some(i) = s.find("{}") {
            s.replace_range(i..i + 2, a);
        }
    }
    s
}
