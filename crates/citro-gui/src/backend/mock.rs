//! An in-memory backend with fake data. It powers `citro-gui --demo` (screenshots, QA without touching a real data
//! directory) and the UI tests. Every host is a documentation address (203.0.113.x, *.example.net) and every
//! password is a fake one; nothing here touches the disk or the network.

use super::{
    BResult, Backend, BackendError, BackendInfo, BackendKind, Caps, HubStatus, KernelInfo, KernelStatus, Level,
    LogKind, LogLine, LogQuery, Member, OpenError, OpenOpts, ProxyCheck, Query, Role, Row, RunInfo, RunState, TeamInfo,
    redact,
};
use crate::query;
use crate::timefmt::now_ts;
use citro_core::csv::{self, CsvRow};
use citro_core::profile::{Group, Profile, ProxyConfig};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

const HOUR: i64 = 3600;
const MIN: i64 = 60;
const DAY: i64 = 86_400;

#[derive(Debug, Clone)]
struct Lock {
    user: String,
    device: String,
    since: i64,
}

#[derive(Debug)]
struct State {
    profiles: Vec<Profile>,
    groups: Vec<Group>,
    running: HashMap<String, (u32, i64)>,
    locks: HashMap<String, Lock>,
    /// Profiles whose browser "crashes" at start (the red state in the table).
    failing: Vec<String>,
    events: Vec<LogLine>,
    next_serial: u64,
    next_pid: u32,
    kernel: KernelStatus,
    hub: HubStatus,
    teams: Vec<TeamInfo>,
    /// The profiles that belong to the team hub (the others are private to this device).
    shared: HashSet<String>,
}

pub struct MockBackend {
    state: Mutex<State>,
    /// Simulated time a blocking call takes (open / close / proxy check), so busy states are visible.
    latency: Duration,
    kind: BackendKind,
}

struct Seed {
    name: &'static str,
    group: usize, // index into GROUPS + 1; 0 = Ungrouped
    remark: &'static str,
    proxy: Option<(&'static str, &'static str, u16, &'static str)>, // scheme, host, port, user
    country: &'static str,
    os: &'static str,
    opened_ago: i64,
    closed_ago: i64,
    state: SeedState,
}

#[derive(Clone, Copy, PartialEq)]
enum SeedState {
    Closed,
    Running,
    Failing,
    Locked(usize), // index into MATES
}

const GROUPS: [(&str, &str); 5] = [
    ("Vinted FR", "Resale accounts, France"),
    ("eBay DE", "Kleinanzeigen and eBay Germany"),
    ("Leboncoin", "Lyon and Paris sellers"),
    ("Wallapop ES", ""),
    ("Research", "No logins, price checks only"),
];

/// (user, device)
const MATES: [(&str, &str); 3] = [("lena", "LENA-PC"), ("omar", "OMAR-MBP"), ("jonas", "JONAS-LAPTOP")];

fn seeds() -> Vec<Seed> {
    use SeedState::*;
    let s = |name, group, remark, proxy, country, os, opened_ago, closed_ago, state| Seed {
        name,
        group,
        remark,
        proxy,
        country,
        os,
        opened_ago,
        closed_ago,
        state,
    };
    vec![
        s(
            "Vinted FR - Camille",
            1,
            "main seller, ships Mon and Thu",
            Some(("socks5", "203.0.113.21", 1080, "demo")),
            "fr",
            "linux",
            3 * MIN,
            2 * DAY,
            Running,
        ),
        s(
            "Vinted FR - Louis",
            1,
            "warming up, no listings yet",
            Some(("socks5", "203.0.113.22", 1080, "demo")),
            "fr",
            "macos",
            52 * MIN,
            41 * MIN,
            Closed,
        ),
        s(
            "Vinted FR - Manon",
            1,
            "sells shoes only",
            Some(("socks5", "203.0.113.23", 1080, "demo")),
            "fr",
            "linux",
            3 * HOUR,
            2 * HOUR,
            Closed,
        ),
        s(
            "eBay DE - Anna",
            2,
            "business account, VAT id on file",
            Some(("http", "proxy.example.net", 8080, "demo")),
            "de",
            "windows",
            20 * MIN,
            12 * MIN,
            Closed,
        ),
        s(
            "eBay DE - Jonas",
            2,
            "",
            Some(("http", "proxy.example.net", 8081, "demo")),
            "de",
            "linux",
            9 * MIN,
            DAY + 3 * HOUR,
            Running,
        ),
        s(
            "eBay DE - Mia",
            2,
            "needs a new proxy, login fails",
            Some(("socks5", "203.0.113.44", 1080, "demo")),
            "de",
            "macos",
            5 * HOUR,
            4 * HOUR + 50 * MIN,
            Failing,
        ),
        s(
            "Leboncoin - Sophie",
            3,
            "Lyon, electronics",
            Some(("socks5", "203.0.113.31", 1080, "demo")),
            "fr",
            "linux",
            DAY + 2 * HOUR,
            DAY + HOUR,
            Closed,
        ),
        s(
            "Leboncoin - Hugo",
            3,
            "Paris, furniture",
            Some(("socks5", "203.0.113.32", 1080, "demo")),
            "fr",
            "macos",
            6 * DAY,
            6 * DAY - 40 * MIN,
            Closed,
        ),
        s(
            "Leboncoin - Clara",
            3,
            "shared with the team",
            Some(("http", "proxy.example.net", 3128, "demo")),
            "fr",
            "linux",
            33 * MIN,
            0,
            Locked(0),
        ),
        s(
            "Wallapop ES - Carmen",
            4,
            "second hand bikes",
            Some(("socks5", "203.0.113.51", 1080, "demo")),
            "es",
            "linux",
            2 * DAY,
            2 * DAY - 25 * MIN,
            Closed,
        ),
        s(
            "Wallapop ES - Pablo",
            4,
            "kernel crash on first open",
            Some(("socks5", "203.0.113.52", 1080, "demo")),
            "es",
            "windows",
            0,
            0,
            Failing,
        ),
        s(
            "Price watch - Amazon IT",
            5,
            "no login, daily at 07:00",
            Some(("socks5", "203.0.113.61", 1080, "demo")),
            "it",
            "linux",
            14 * HOUR,
            13 * HOUR + 40 * MIN,
            Closed,
        ),
        s(
            "Price watch - Zalando NL",
            5,
            "",
            Some(("socks5", "203.0.113.62", 1080, "demo")),
            "nl",
            "macos",
            2 * HOUR,
            20 * MIN,
            Locked(1),
        ),
        s("Scratch profile", 0, "direct connection, throw away", None, "", "", 0, 0, Closed),
    ]
}

fn fake_id(i: usize) -> String {
    // deterministic, looks like the real `k` + 6 [a-z0-9] ids
    let mut x = 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(i as u64 + 1);
    let mut id = String::from("k");
    for _ in 0..6 {
        x ^= x >> 29;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        id.push(b"abcdefghijklmnopqrstuvwxyz0123456789"[(x >> 33) as usize % 36] as char);
    }
    id
}

impl MockBackend {
    /// ~14 realistic fake profiles, 5 groups, a fake team and a fake kernel, relative to `now`.
    pub fn demo_at(now: i64) -> MockBackend {
        let groups: Vec<Group> = GROUPS
            .iter()
            .enumerate()
            .map(|(i, (n, r))| Group { group_id: (i + 1).to_string(), group_name: (*n).into(), remark: (*r).into() })
            .collect();
        let mut profiles = Vec::new();
        let (mut running, mut locks, mut failing) = (HashMap::new(), HashMap::new(), Vec::new());
        for (i, s) in seeds().into_iter().enumerate() {
            let id = fake_id(i);
            let mut p = Profile {
                profile_id: id.clone(),
                serial_number: i as u64 + 1,
                name: s.name.into(),
                group_id: s.group.to_string(),
                remark: s.remark.into(),
                password: "demo-password".into(),
                cookie: String::new(),
                country: s.country.into(),
                persona_os: s.os.into(),
                created_time: now - (30 - i as i64) * DAY,
                last_open_time: if s.opened_ago > 0 { now - s.opened_ago } else { 0 },
                last_close_time: if s.closed_ago > 0 { now - s.closed_ago } else { 0 },
                tabs: if s.group == 1 { vec!["https://www.vinted.fr/member".into()] } else { Vec::new() },
                ..Default::default()
            };
            if let Some((scheme, host, port, user)) = s.proxy {
                p.user_proxy_config =
                    ProxyConfig::from_url(&format!("{scheme}://{user}:demo-pass@{host}:{port}")).unwrap_or_default();
                p.ip = host.to_owned();
            }
            match s.state {
                SeedState::Running => {
                    running.insert(id.clone(), (40_000 + i as u32 * 137, now - s.opened_ago));
                }
                SeedState::Locked(m) => {
                    locks.insert(
                        id.clone(),
                        Lock { user: MATES[m].0.into(), device: MATES[m].1.into(), since: now - s.opened_ago },
                    );
                }
                SeedState::Failing => failing.push(id.clone()),
                SeedState::Closed => {}
            }
            profiles.push(p);
        }
        let ev = |ago: i64, level, id: Option<usize>, msg: &str| LogLine {
            ts: now - ago,
            level,
            profile_id: id.map(fake_id),
            message: msg.into(),
        };
        let events = vec![
            ev(3 * HOUR + 12 * MIN, Level::Info, None, "citro-gui started (demo data, nothing is written to disk)"),
            ev(2 * HOUR + 58 * MIN, Level::Info, Some(2), "opened Vinted FR - Manon (pid 41122)"),
            ev(2 * HOUR + MIN, Level::Info, Some(2), "closed Vinted FR - Manon"),
            ev(
                HOUR + 47 * MIN,
                Level::Warn,
                None,
                "proxy check 203.0.113.44:1080: the proxy rejected the user name / password",
            ),
            ev(
                HOUR + 46 * MIN,
                Level::Error,
                Some(5),
                "could not open eBay DE - Mia: the kernel exited before opening its DevTools port (status Some(1))",
            ),
            ev(HOUR + 20 * MIN, Level::Info, Some(0), "saved profile Vinted FR - Camille"),
            ev(58 * MIN, Level::Info, None, "created group Research"),
            ev(57 * MIN, Level::Info, Some(11), "created profile Price watch - Amazon IT"),
            ev(52 * MIN, Level::Info, Some(1), "opened Vinted FR - Louis (pid 43870)"),
            ev(41 * MIN, Level::Info, Some(1), "closed Vinted FR - Louis"),
            ev(33 * MIN, Level::Info, None, "moved 2 profile(s) to Leboncoin"),
            ev(20 * MIN, Level::Info, Some(3), "opened eBay DE - Anna (pid 44901)"),
            ev(14 * MIN, Level::Info, None, "exported 14 profiles to ~/citro-profiles-20261116-0912.csv"),
            ev(12 * MIN, Level::Info, Some(3), "closed eBay DE - Anna"),
            ev(9 * MIN, Level::Info, Some(4), "opened eBay DE - Jonas (pid 45410)"),
            ev(7 * MIN, Level::Warn, None, "proxy check 203.0.113.62:1080: timed out"),
            ev(3 * MIN, Level::Info, Some(0), "opened Vinted FR - Camille (pid 46218)"),
        ];
        let shared: HashSet<String> = profiles
            .iter()
            .filter(|p| p.group_id == "1" || locks.contains_key(&p.profile_id))
            .map(|p| p.profile_id.clone())
            .collect();
        let me = "mia";
        let teams = vec![TeamInfo {
            id: "t1".into(),
            name: "Vinted Ops".into(),
            role: Role::Admin,
            members: vec![
                Member { name: me.into(), role: Role::Owner },
                Member { name: "jonas".into(), role: Role::Admin },
                Member { name: "lena".into(), role: Role::Member },
                Member { name: "omar".into(), role: Role::Member },
                Member { name: "sofia".into(), role: Role::Viewer },
            ],
            profile_count: shared.len(),
        }];
        let kernel = KernelStatus {
            installed: Some(KernelInfo {
                version: "151.0.7922.108".into(),
                path: "/home/demo/.local/share/citroenition/kernels/clearcote-151.0.7922.108/chrome".into(),
            }),
            latest: Some("152.0.7981.40".into()),
            all: vec![
                KernelInfo {
                    version: "151.0.7922.108".into(),
                    path: "/home/demo/.local/share/citroenition/kernels/clearcote-151.0.7922.108/chrome".into(),
                },
                KernelInfo {
                    version: "150.0.7871.64".into(),
                    path: "/home/demo/.local/share/citroenition/kernels/clearcote-150.0.7871.64/chrome".into(),
                },
            ],
            source: "newest installed kernel".into(),
            error: None,
        };
        MockBackend {
            state: Mutex::new(State {
                next_serial: profiles.len() as u64 + 1,
                profiles,
                groups,
                running,
                locks,
                failing,
                events,
                next_pid: 50_000,
                kernel,
                hub: HubStatus {
                    connected: true,
                    url: "https://hub.example.net".into(),
                    user: me.into(),
                    team: "Vinted Ops".into(),
                    device_name: "MIA-LAPTOP".into(),
                    error: None,
                },
                teams,
                shared,
            }),
            latency: Duration::from_millis(900),
            kind: BackendKind::Demo,
        }
    }

    pub fn demo() -> MockBackend {
        MockBackend::demo_at(now_ts())
    }

    pub fn with_latency(mut self, latency: Duration) -> MockBackend {
        self.latency = latency;
        self
    }

    /// Ids of the demo profiles whose browser fails to start.
    pub fn failing_ids(&self) -> Vec<String> {
        self.lock().failing.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn nap(&self, fraction: f64) {
        if !self.latency.is_zero() {
            std::thread::sleep(self.latency.mul_f64(fraction));
        }
    }

    fn log(st: &mut State, level: Level, id: Option<&str>, message: String) {
        st.events.push(LogLine { ts: now_ts(), level, profile_id: id.map(str::to_owned), message });
    }
}

fn find<'a>(st: &'a State, id: &str) -> BResult<&'a Profile> {
    st.profiles.iter().find(|p| p.profile_id == id).ok_or_else(|| BackendError::NotFound(format!("profile {id}")))
}

fn run_state(st: &State, id: &str) -> RunState {
    if let Some((pid, since)) = st.running.get(id) {
        RunState::Open { pid: *pid, since: *since }
    } else if let Some(l) = st.locks.get(id) {
        RunState::LockedElsewhere { user: l.user.clone(), device: l.device.clone(), since: l.since }
    } else {
        RunState::Closed
    }
}

const DEMO_BROWSER_LOG: &str = "\
[46218:46218:1116/093201.412:INFO:chrome_browser_main.cc(312)] Clearcote 151.0.7922.108 starting, persona linux
[46218:46218:1116/093201.455:INFO:fingerprint_seed.cc(41)] seed from profile id, 14 surfaces derived
[46218:46251:1116/093201.730:WARNING:gpu_process_host.cc(1021)] GPU process launch took 318 ms
[46218:46218:1116/093203.019:INFO:proxy_config_service.cc(88)] using socks5://demo:***@203.0.113.21:1080
[46218:46218:1116/093203.302:INFO:timezone_override.cc(27)] time zone Europe/Paris, languages fr-FR,fr
[46218:46262:1116/093204.870:ERROR:ssl_client_socket_impl.cc(985)] handshake failed; returned -1, SSL error code 1, net_error -100
[46218:46218:1116/093205.140:INFO:session_restore.cc(204)] restored 1 tab
[46218:46218:1116/093210.002:WARNING:permission_context.cc(319)] notification permission denied by policy";

impl Backend for MockBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            kind: self.kind,
            data_dir: "/home/demo/.local/share/citroenition".into(),
            caps: Caps { teams: true, kernel_install: true, proxy_check: true, group_edit: true },
        }
    }

    fn list_profiles(&self, q: &Query) -> BResult<Vec<Row>> {
        let st = self.lock();
        let rows: Vec<Row> = st
            .profiles
            .iter()
            .map(|p| Row {
                run: run_state(&st, &p.profile_id),
                team: st.shared.contains(&p.profile_id).then(|| "t1".to_owned()),
                profile: redact(p),
            })
            .collect();
        Ok(query::apply(rows, q, &st.groups))
    }

    fn profile(&self, id: &str) -> BResult<Profile> {
        find(&self.lock(), id).cloned()
    }

    fn save_profile(&self, mut p: Profile) -> BResult<Profile> {
        p.name = p.name.trim().to_owned();
        if p.name.is_empty() {
            return Err(BackendError::Invalid("give the profile a name".into()));
        }
        let mut st = self.lock();
        if p.group_id.is_empty() {
            p.group_id = "0".into();
        }
        if p.profile_id.is_empty() {
            p.serial_number = st.next_serial;
            st.next_serial += 1;
            p.profile_id = fake_id(1000 + p.serial_number as usize);
            p.created_time = now_ts();
            Self::log(&mut st, Level::Info, Some(&p.profile_id.clone()), format!("created profile {}", p.name));
            st.profiles.push(p.clone());
            return Ok(p);
        }
        let slot = st
            .profiles
            .iter_mut()
            .find(|x| x.profile_id == p.profile_id)
            .ok_or_else(|| BackendError::NotFound(format!("profile {}", p.profile_id)))?;
        p.serial_number = slot.serial_number;
        p.created_time = slot.created_time;
        p.last_open_time = slot.last_open_time;
        p.last_close_time = slot.last_close_time;
        *slot = p.clone();
        Self::log(&mut st, Level::Info, Some(&p.profile_id.clone()), format!("saved profile {}", p.name));
        Ok(p)
    }

    fn delete_profiles(&self, ids: &[String]) -> BResult<()> {
        let mut st = self.lock();
        let team: Vec<String> = st
            .profiles
            .iter()
            .filter(|p| ids.contains(&p.profile_id) && st.shared.contains(&p.profile_id))
            .map(|p| p.name.clone())
            .collect();
        if !team.is_empty() {
            return Err(BackendError::Invalid(format!(
                "{} belong{} to the team hub: deleting removes {} for the whole team, not only here",
                team.join(", "),
                if team.len() == 1 { "s" } else { "" },
                if team.len() == 1 { "it" } else { "them" },
            )));
        }
        let before = st.profiles.len();
        let names: Vec<String> =
            st.profiles.iter().filter(|p| ids.contains(&p.profile_id)).map(|p| p.name.clone()).collect();
        st.profiles.retain(|p| !ids.contains(&p.profile_id));
        for id in ids {
            st.running.remove(id);
            st.locks.remove(id);
        }
        if st.profiles.len() == before {
            return Err(BackendError::NotFound("none of the selected profiles exists any more".into()));
        }
        for n in names {
            Self::log(&mut st, Level::Info, None, format!("deleted profile {n}"));
        }
        Ok(())
    }

    fn set_remark(&self, id: &str, text: &str) -> BResult<()> {
        let mut st = self.lock();
        let p = st.profiles.iter_mut().find(|p| p.profile_id == id).ok_or_else(|| BackendError::NotFound(id.into()))?;
        p.remark = text.trim_end().to_owned();
        Ok(())
    }

    fn move_to_group(&self, ids: &[String], group: &str) -> BResult<()> {
        let mut st = self.lock();
        let group = if group.is_empty() { "0" } else { group };
        if group != "0" && !st.groups.iter().any(|g| g.group_id == group) {
            return Err(BackendError::Invalid(format!("group {group} does not exist")));
        }
        for p in st.profiles.iter_mut().filter(|p| ids.contains(&p.profile_id)) {
            p.group_id = group.to_owned();
        }
        let name = super::group_label(&st.groups, group);
        Self::log(&mut st, Level::Info, None, format!("moved {} profile(s) to {name}", ids.len()));
        Ok(())
    }

    fn duplicate_profile(&self, id: &str) -> BResult<Profile> {
        let mut copy = self.profile(id)?;
        copy.profile_id.clear();
        copy.name = format!("{} (copy)", copy.name);
        copy.last_open_time = 0;
        copy.last_close_time = 0;
        self.save_profile(copy)
    }

    fn clear_cache(&self, id: &str) -> BResult<()> {
        let mut st = self.lock();
        let name = find(&st, id)?.name.clone();
        if st.running.contains_key(id) {
            return Err(BackendError::Invalid(format!("close {name} before clearing its cache")));
        }
        Self::log(&mut st, Level::Info, Some(id), format!("cleared the cache of {name}"));
        Ok(())
    }

    fn check_proxy(&self, proxy: &ProxyConfig) -> BResult<ProxyCheck> {
        let Some(hop) = proxy.hop() else {
            return Err(BackendError::Invalid("no proxy is configured (direct connection)".into()));
        };
        self.nap(0.6);
        // the demo proxies on .44 reject their login, the rest answer
        if hop.host.ends_with(".44") {
            return Ok(ProxyCheck {
                ok: false,
                ip: String::new(),
                country: String::new(),
                latency_ms: 412,
                message: "the proxy rejected the user name / password".into(),
            });
        }
        Ok(ProxyCheck {
            ok: true,
            ip: hop.host.clone(),
            country: "de".into(),
            latency_ms: 187,
            message: format!("exit IP {}", hop.host),
        })
    }

    fn open(&self, id: &str, _opts: &OpenOpts) -> Result<RunInfo, OpenError> {
        {
            let st = self.lock();
            find(&st, id)?;
            if let Some(l) = st.locks.get(id) {
                return Err(OpenError::LockedElsewhere {
                    user: l.user.clone(),
                    device: l.device.clone(),
                    since: l.since,
                });
            }
            if let Some((pid, since)) = st.running.get(id) {
                return Ok(RunInfo { pid: *pid, since: *since });
            }
        }
        self.nap(1.0);
        let mut st = self.lock();
        let name = find(&st, id)?.name.clone();
        if st.failing.iter().any(|f| f == id) {
            let msg = "the kernel exited before opening its DevTools port (status Some(1)): \
                       [1:1:1116/093201.012:FATAL:gpu_data_manager_impl_private.cc(415)] GPU process isn't usable. Goodbye.";
            Self::log(
                &mut st,
                Level::Error,
                Some(id),
                format!("could not open {name}: the kernel exited before opening its DevTools port (status Some(1))"),
            );
            return Err(OpenError::Other(msg.into()));
        }
        let (pid, since) = (st.next_pid, now_ts());
        st.next_pid += 7;
        st.running.insert(id.to_owned(), (pid, since));
        if let Some(p) = st.profiles.iter_mut().find(|p| p.profile_id == id) {
            p.last_open_time = since;
        }
        Self::log(&mut st, Level::Info, Some(id), format!("opened {name} (pid {pid})"));
        Ok(RunInfo { pid, since })
    }

    fn close(&self, id: &str) -> BResult<()> {
        self.nap(0.4);
        let mut st = self.lock();
        let name = find(&st, id)?.name.clone();
        if st.running.remove(id).is_some() {
            if let Some(p) = st.profiles.iter_mut().find(|p| p.profile_id == id) {
                p.last_close_time = now_ts();
            }
            Self::log(&mut st, Level::Info, Some(id), format!("closed {name}"));
        }
        Ok(())
    }

    fn groups(&self) -> BResult<Vec<Group>> {
        Ok(self.lock().groups.clone())
    }

    fn save_group(&self, mut g: Group) -> BResult<Group> {
        g.group_name = g.group_name.trim().to_owned();
        g.remark = g.remark.trim().to_owned();
        if g.group_name.is_empty() {
            return Err(BackendError::Invalid("the group name must not be empty".into()));
        }
        let mut st = self.lock();
        if st.groups.iter().any(|x| x.group_id != g.group_id && x.group_name.eq_ignore_ascii_case(&g.group_name)) {
            return Err(BackendError::Invalid(format!("a group called `{}` already exists", g.group_name)));
        }
        if g.group_id.is_empty() {
            let next = st.groups.iter().filter_map(|x| x.group_id.parse::<u64>().ok()).max().unwrap_or(0) + 1;
            g.group_id = next.to_string();
            st.groups.push(g.clone());
            Self::log(&mut st, Level::Info, None, format!("created group {}", g.group_name));
        } else {
            let slot = st
                .groups
                .iter_mut()
                .find(|x| x.group_id == g.group_id)
                .ok_or_else(|| BackendError::NotFound(format!("group {}", g.group_id)))?;
            *slot = g.clone();
        }
        Ok(g)
    }

    fn delete_group(&self, id: &str) -> BResult<()> {
        if id.is_empty() || id == "0" {
            return Err(BackendError::Invalid("Ungrouped is built in and cannot be deleted".into()));
        }
        let mut st = self.lock();
        let before = st.groups.len();
        st.groups.retain(|g| g.group_id != id);
        if st.groups.len() == before {
            return Err(BackendError::NotFound(format!("group {id}")));
        }
        for p in st.profiles.iter_mut().filter(|p| p.group_id == id) {
            p.group_id = "0".into();
        }
        Ok(())
    }

    fn export_csv(&self, ids: Option<Vec<String>>, group: Option<String>, bom: bool) -> BResult<Vec<u8>> {
        let st = self.lock();
        let rows: Vec<CsvRow> = st
            .profiles
            .iter()
            .filter(|p| match (&ids, &group) {
                (Some(ids), _) => ids.contains(&p.profile_id),
                (None, Some(g)) => (if p.group_id.is_empty() { "0" } else { &p.group_id }) == g,
                (None, None) => true,
            })
            .map(|p| {
                let name = match super::group_label(&st.groups, &p.group_id).as_str() {
                    "Ungrouped" => String::new(),
                    n => n.to_owned(),
                };
                let mut row = CsvRow::from_profile(p, &name, st.running.contains_key(&p.profile_id));
                if let Some(l) = st.locks.get(&p.profile_id) {
                    row.locked_by = l.user.clone();
                    row.locked_device = l.device.clone();
                    row.status = "open".into();
                }
                row
            })
            .collect();
        Ok(csv::write(&rows, bom))
    }

    fn logs(&self, q: &LogQuery) -> BResult<Vec<LogLine>> {
        match q.kind {
            LogKind::Citro => {
                let needle = q.text.trim().to_lowercase();
                let st = self.lock();
                let mut out: Vec<LogLine> = st
                    .events
                    .iter()
                    .rev()
                    .filter(|l| l.level >= q.level_min)
                    .filter(|l| q.profile_id.as_deref().is_none_or(|p| l.profile_id.as_deref() == Some(p)))
                    .filter(|l| needle.is_empty() || l.message.to_lowercase().contains(&needle))
                    .take(if q.limit == 0 { usize::MAX } else { q.limit })
                    .cloned()
                    .collect();
                out.reverse();
                Ok(out)
            }
            LogKind::Browser => {
                let Some(id) = &q.profile_id else { return Ok(Vec::new()) };
                let needle = q.text.trim().to_lowercase();
                let mut out: Vec<LogLine> = DEMO_BROWSER_LOG
                    .lines()
                    .map(|l| LogLine {
                        ts: 0,
                        level: if l.contains(":ERROR:") || l.contains(":FATAL:") {
                            Level::Error
                        } else if l.contains(":WARNING:") {
                            Level::Warn
                        } else {
                            Level::Info
                        },
                        profile_id: Some(id.clone()),
                        message: l.to_owned(),
                    })
                    .filter(|l| {
                        l.level >= q.level_min && (needle.is_empty() || l.message.to_lowercase().contains(&needle))
                    })
                    .collect();
                if q.limit > 0 && out.len() > q.limit {
                    out.drain(..out.len() - q.limit);
                }
                Ok(out)
            }
        }
    }

    fn kernel_status(&self) -> BResult<KernelStatus> {
        Ok(self.lock().kernel.clone())
    }

    fn install_kernel(&self, progress: &dyn Fn(u64, u64)) -> BResult<KernelStatus> {
        const TOTAL: u64 = 118_400_000;
        for i in 0..=20u64 {
            progress(TOTAL / 20 * i, TOTAL);
            self.nap(0.04);
        }
        let mut st = self.lock();
        if let Some(latest) = st.kernel.latest.clone() {
            let info = KernelInfo {
                path: format!("/home/demo/.local/share/citroenition/kernels/clearcote-{latest}/chrome"),
                version: latest,
            };
            st.kernel.all.insert(0, info.clone());
            st.kernel.installed = Some(info.clone());
            Self::log(&mut st, Level::Info, None, format!("installed kernel {}", info.version));
        }
        Ok(st.kernel.clone())
    }

    fn teams(&self) -> BResult<Vec<TeamInfo>> {
        let st = self.lock();
        let mut teams = st.teams.clone();
        for t in &mut teams {
            t.profile_count = st.shared.len();
        }
        Ok(if st.hub.connected { teams } else { Vec::new() })
    }

    fn hub_status(&self) -> HubStatus {
        self.lock().hub.clone()
    }

    fn connect_hub(&self, url: &str, _token: &str, device_name: &str, team: &str) -> BResult<HubStatus> {
        self.nap(0.5);
        let mut st = self.lock();
        let team = if team.trim().is_empty() { st.teams[0].name.clone() } else { team.trim().to_owned() };
        st.hub = HubStatus {
            connected: true,
            url: url.into(),
            user: "mia".into(),
            team,
            device_name: device_name.into(),
            error: None,
        };
        Ok(st.hub.clone())
    }

    fn disconnect_hub(&self) -> BResult<HubStatus> {
        self.nap(0.3);
        let mut st = self.lock();
        if st.hub.connected {
            Self::log(&mut st, Level::Info, None, "disconnected from the team hub".into());
        }
        st.hub = HubStatus::default();
        Ok(st.hub.clone())
    }

    fn pull_team(&self) -> BResult<String> {
        self.nap(0.6);
        let mut st = self.lock();
        if !st.hub.connected {
            return Err(BackendError::Invalid("this device is not connected to a team hub: connect first".into()));
        }
        Self::log(&mut st, Level::Info, None, "team: the team's profiles are up to date".into());
        Ok("the team's profiles are up to date".into())
    }

    fn share_profiles(&self, ids: &[String]) -> BResult<String> {
        self.nap(0.8);
        let mut st = self.lock();
        if !st.hub.connected {
            return Err(BackendError::Invalid("this device is not connected to a team hub: connect first".into()));
        }
        let mut done = 0usize;
        for id in ids {
            let name = find(&st, id)?.name.clone();
            if st.shared.insert(id.clone()) {
                done += 1;
                Self::log(&mut st, Level::Info, Some(id), format!("shared {name} with the team"));
            }
        }
        let team = st.hub.team.clone();
        Ok(format!("Shared {done} profile{} with {team}", if done == 1 { "" } else { "s" }))
    }

    fn delete_profiles_everywhere(&self, ids: &[String]) -> BResult<()> {
        self.nap(0.5);
        let mut st = self.lock();
        let names: Vec<String> =
            st.profiles.iter().filter(|p| ids.contains(&p.profile_id)).map(|p| p.name.clone()).collect();
        if names.is_empty() {
            return Err(BackendError::NotFound("none of the selected profiles exists any more".into()));
        }
        st.profiles.retain(|p| !ids.contains(&p.profile_id));
        for id in ids {
            st.running.remove(id);
            st.locks.remove(id);
            st.shared.remove(id);
        }
        for n in names {
            Self::log(&mut st, Level::Info, None, format!("deleted profile {n} (for everybody it was shared with)"));
        }
        Ok(())
    }

    fn force_unlock(&self, profile_id: &str) -> BResult<()> {
        let mut st = self.lock();
        match st.locks.remove(profile_id) {
            Some(l) => {
                Self::log(
                    &mut st,
                    Level::Warn,
                    Some(profile_id),
                    format!("force-unlocked a profile held by {} on {}", l.user, l.device),
                );
                Ok(())
            }
            None => Err(BackendError::NotFound("that profile is not locked".into())),
        }
    }

    fn record(&self, level: Level, profile_id: Option<&str>, message: &str) {
        Self::log(&mut self.lock(), level, profile_id, message.to_owned());
    }

    fn set_kernel_override(&self, _path: Option<String>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> MockBackend {
        MockBackend::demo_at(now_ts()).with_latency(Duration::ZERO)
    }

    fn rows(b: &MockBackend) -> Vec<Row> {
        b.list_profiles(&Query::default()).unwrap()
    }

    #[test]
    fn the_demo_is_rich_enough_for_screenshots() {
        let b = demo();
        let rows = rows(&b);
        assert_eq!(rows.len(), 14);
        assert_eq!(b.groups().unwrap().len(), 5);
        let running = rows.iter().filter(|r| r.run.is_open()).count();
        let locked = rows.iter().filter(|r| r.run.is_locked()).count();
        assert!((2..=5).contains(&running), "{running} running");
        assert_eq!(locked, 2);
        assert_eq!(b.failing_ids().len(), 2);
        assert!(rows.iter().any(|r| r.profile.user_proxy_config.is_direct()));
        let mut ids: Vec<&str> = rows.iter().map(Row::id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 14, "ids are unique");
        // times relative to `now`, never in the future
        let now = now_ts();
        assert!(rows.iter().all(|r| r.profile.last_open_time <= now && r.profile.last_close_time <= now));
    }

    #[test]
    fn demo_data_has_only_documentation_hosts_and_fake_secrets_that_never_reach_rows() {
        let b = demo();
        let dump = format!("{:?}", rows(&b));
        assert!(!dump.contains("demo-pass") && !dump.contains("demo-password"));
        let full = b.profile(&fake_id(0)).unwrap();
        assert_eq!(full.user_proxy_config.hop().unwrap().host, "203.0.113.21");
        for p in b.lock().profiles.iter() {
            if let Some(h) = p.user_proxy_config.hop() {
                assert!(h.host.starts_with("203.0.113.") || h.host.ends_with(".example.net"), "{}", h.host);
            }
        }
    }

    #[test]
    fn open_and_close_update_state_times_and_log() {
        let b = demo();
        let id = fake_id(1); // closed
        let before = b.profile(&id).unwrap();
        let info = b.open(&id, &OpenOpts::default()).unwrap();
        assert!(info.pid > 0);
        let r = rows(&b).into_iter().find(|r| r.id() == id).unwrap();
        assert!(r.run.is_open());
        assert!(r.profile.last_open_time > before.last_open_time);
        assert_eq!(b.open(&id, &OpenOpts::default()).unwrap().pid, info.pid, "opening twice is idempotent");
        b.close(&id).unwrap();
        let after = b.profile(&id).unwrap();
        assert!(after.last_close_time >= after.last_open_time);
        assert_eq!(rows(&b).into_iter().find(|r| r.id() == id).unwrap().run, RunState::Closed);
        let log = b.logs(&LogQuery { profile_id: Some(id), limit: 10, ..Default::default() }).unwrap();
        assert!(
            log.iter().any(|l| l.message.starts_with("opened "))
                && log.iter().any(|l| l.message.starts_with("closed "))
        );
    }

    #[test]
    fn locked_and_failing_profiles_refuse_to_open() {
        let b = demo();
        let locked = rows(&b).into_iter().find(|r| r.run.is_locked()).unwrap();
        match b.open(locked.id(), &OpenOpts::default()).unwrap_err() {
            OpenError::LockedElsewhere { user, device, .. } => assert!(!user.is_empty() && !device.is_empty()),
            e => panic!("{e:?}"),
        }
        b.force_unlock(locked.id()).unwrap();
        assert!(b.open(locked.id(), &OpenOpts::default()).is_ok());
        let bad = &b.failing_ids()[0];
        let OpenError::Other(msg) = b.open(bad, &OpenOpts::default()).unwrap_err() else { panic!() };
        assert!(msg.contains("DevTools"));
        assert!(b.force_unlock("nope").is_err());
    }

    #[test]
    fn crud_groups_and_export() {
        let b = demo();
        let p = b.save_profile(Profile { name: "  New  ".into(), ..Default::default() }).unwrap();
        assert_eq!((p.name.as_str(), p.serial_number, p.group_id.as_str()), ("New", 15, "0"));
        assert_eq!(rows(&b).len(), 15);
        let g = b.save_group(Group { group_name: "Fresh".into(), ..Default::default() }).unwrap();
        assert_eq!(g.group_id, "6");
        assert!(b.save_group(Group { group_name: "fresh".into(), ..Default::default() }).is_err());
        b.move_to_group(std::slice::from_ref(&p.profile_id), &g.group_id).unwrap();
        b.delete_group(&g.group_id).unwrap();
        assert_eq!(b.profile(&p.profile_id).unwrap().group_id, "0");
        b.delete_profiles(std::slice::from_ref(&p.profile_id)).unwrap();
        assert_eq!(rows(&b).len(), 14);

        let text = String::from_utf8(b.export_csv(None, None, true).unwrap()).unwrap();
        assert!(text.starts_with('\u{feff}'));
        assert!(!text.contains("demo-pass"));
        assert_eq!(text.matches("\r\n").count(), 15);
        let locked_line = text.lines().find(|l| l.contains(",lena,LENA-PC")).expect("a locked row with its holder");
        assert!(locked_line.contains(",open,"));
    }

    #[test]
    fn kernel_install_reports_progress_and_updates_the_status() {
        let b = demo();
        let steps = Mutex::new(Vec::new());
        let st = b.install_kernel(&|d, t| steps.lock().unwrap().push((d, t))).unwrap();
        let steps = steps.into_inner().unwrap();
        assert_eq!(steps.first().unwrap().0, 0);
        assert_eq!(steps.last().unwrap().0, steps.last().unwrap().1 / 20 * 20);
        assert_eq!(st.installed.unwrap().version, "152.0.7981.40");
        assert!(!b.kernel_status().unwrap().update_available());
    }

    #[test]
    fn proxy_check_and_hub() {
        let b = demo();
        let ok = ProxyConfig::from_url("socks5://u:p@203.0.113.21:1080").unwrap();
        assert!(b.check_proxy(&ok).unwrap().ok);
        let bad = ProxyConfig::from_url("socks5://u:p@203.0.113.44:1080").unwrap();
        let r = b.check_proxy(&bad).unwrap();
        assert!(!r.ok && r.message.contains("rejected"));
        assert!(b.check_proxy(&ProxyConfig::default()).is_err());
        assert!(b.hub_status().connected);
        assert_eq!(b.teams().unwrap()[0].members.len(), 5);
        assert_eq!(b.info().kind, BackendKind::Demo);
    }

    #[test]
    fn team_profiles_are_shared_pulled_and_deleted_for_everyone() {
        let b = demo();
        let team_of = |b: &MockBackend| rows(b).into_iter().filter(|r| r.team.is_some()).count();
        let before = team_of(&b);
        assert!(before > 0 && b.teams().unwrap()[0].profile_count == before, "the demo has a team to look at");
        // a private profile can be shared, once
        let private = rows(&b).into_iter().find(|r| r.team.is_none()).unwrap();
        assert!(b.share_profiles(&[private.id().to_owned()]).unwrap().starts_with("Shared 1 profile with Vinted Ops"));
        assert_eq!(team_of(&b), before + 1);
        assert!(b.share_profiles(&[private.id().to_owned()]).unwrap().starts_with("Shared 0 profiles"));
        // a team profile is not deleted by accident, only for everybody on purpose
        let err = b.delete_profiles(&[private.id().to_owned()]).unwrap_err();
        assert!(matches!(&err, BackendError::Invalid(m) if m.contains("team hub")), "{err:?}");
        b.delete_profiles_everywhere(&[private.id().to_owned()]).unwrap();
        assert!(rows(&b).iter().all(|r| r.id() != private.id()));
        // pulling works while connected; after disconnecting there is no team and nothing to pull
        assert!(b.pull_team().unwrap().contains("up to date"));
        assert!(!b.disconnect_hub().unwrap().connected);
        assert!(b.teams().unwrap().is_empty() && b.pull_team().is_err() && b.share_profiles(&[]).is_err());
        let again = b.connect_hub("https://hub.example.net", "tok", "MIA-LAPTOP", "Atelier").unwrap();
        assert!(again.connected && again.team == "Atelier");
    }
}
