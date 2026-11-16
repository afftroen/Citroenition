//! The AdsPower Local API (`http://127.0.0.1:50325`), served by Citroenition.
//!
//! Automation written against AdsPower (Selenium / Puppeteer / Playwright scripts, the SDKs) can point at
//! this server unchanged. Like AdsPower, every reply is HTTP 200 with `{"code": 0|-1, "data": ..., "msg": ...}`;
//! the only exceptions are 401 (bad API key) and 404 (unknown route).
//!
//! Implemented: `/status`; v1 + v2 browser start / stop / active / local-active; profile create / update /
//! delete / list; groups. Saved proxies (`proxyid`) are not supported and are rejected rather than silently
//! ignored, so a profile never ends up connecting directly when you asked for a proxy.

use anyhow::Context;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use citro_core::kernel::Kernel;
use citro_core::launch::{self, LaunchOptions, Running};
use citro_core::profile::Profile;
use citro_core::store::{self, Home, Store};
use citro_core::{Error, Result as CoreResult};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

#[derive(Clone)]
pub struct AppState {
    pub home: Home,
    /// Explicit kernel binary; `None` = discover.
    pub kernel: Option<PathBuf>,
    /// When set, every route except `/status` needs `Authorization: Bearer <key>`.
    pub api_key: Option<Arc<str>>,
    /// Browser launches are serialised: opening a profile twice at once must not spawn two kernels.
    open_lock: Arc<Mutex<()>>,
}

impl AppState {
    pub fn new(home: Home, kernel: Option<PathBuf>, api_key: Option<String>) -> Self {
        Self {
            home,
            kernel,
            api_key: api_key.filter(|k| !k.is_empty()).map(Arc::from),
            open_lock: Arc::new(Mutex::new(())),
        }
    }
}

type Reply = Json<Value>;

fn ok(data: Value) -> Reply {
    Json(json!({"code": 0, "data": data, "msg": "success"}))
}

fn fail(msg: impl std::fmt::Display) -> Reply {
    Json(json!({"code": -1, "data": {}, "msg": msg.to_string()}))
}

/// A sync error as the core error the routes speak: what the core said stays as it was, the rest is its message
/// (who has the profile open, that the hub does not answer, ...).
fn sync_err(e: citro_sync::Error) -> Error {
    match e {
        citro_sync::Error::Core(c) => c,
        other => Error::Invalid(other.to_string()),
    }
}

/// Run blocking core work off the async runtime.
async fn run<F>(f: F) -> Reply
where
    F: FnOnce() -> CoreResult<Value> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(data)) => ok(data),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "request failed");
            fail(e)
        }
        Err(e) => fail(format!("internal error: {e}")),
    }
}

// ---------------------------------------------------------------------------------------------------
// lenient request parsing: API clients disagree about strings vs numbers vs booleans

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "1" } else { "0" }.to_owned()),
        _ => None,
    }
}

fn field(m: &Map<String, Value>, key: &str) -> Option<String> {
    scalar(m.get(key)?)
}

fn truthy(m: &Map<String, Value>, key: &str, default: bool) -> bool {
    match field(m, key).as_deref() {
        Some("1" | "true") => true,
        Some("0" | "false") => false,
        _ => default,
    }
}

fn int(m: &Map<String, Value>, key: &str) -> Option<usize> {
    field(m, key)?.trim().parse().ok()
}

/// A JSON array of scalars, a JSON-encoded array in a string (query strings), or one bare string.
fn str_list(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(scalar).collect(),
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => a.iter().filter_map(scalar).collect(),
            _ if s.trim().is_empty() => Vec::new(),
            _ => vec![s.clone()],
        },
        other => scalar(other).into_iter().collect(),
    }
}

fn body_map(bytes: &[u8]) -> Result<Map<String, Value>, Reply> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(fail("the request body must be a JSON object")),
        Err(e) => Err(fail(format!("invalid JSON body: {e}"))),
    }
}

fn qmap(q: HashMap<String, String>) -> Map<String, Value> {
    q.into_iter().map(|(k, v)| (k, Value::String(v))).collect()
}

/// The profile a request is about: `profile_id` / `user_id` win over `profile_no` / `serial_number`.
fn key_of(m: &Map<String, Value>) -> Option<String> {
    ["profile_id", "user_id", "profile_no", "serial_number"]
        .iter()
        .find_map(|k| field(m, k))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn resolve_id(home: &Home, key: &str) -> CoreResult<String> {
    Ok(Store::open(home)?.get(key)?.profile_id.clone())
}

// ---------------------------------------------------------------------------------------------------
// routes

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/status", get(status))
        // v1
        .route("/api/v1/browser/start", get(start_get))
        .route("/api/v1/browser/stop", get(stop_get))
        .route("/api/v1/browser/active", get(active_get))
        .route("/api/v1/browser/local-active", get(local_active))
        .route("/api/v1/user/list", get(list_get))
        .route("/api/v1/user/create", post(create_v1))
        .route("/api/v1/user/update", post(update_v1))
        .route("/api/v1/user/delete", post(delete_v1))
        .route("/api/v1/group/list", get(group_list))
        .route("/api/v1/group/create", post(group_create))
        // v2
        .route("/api/v2/browser-profile/start", post(start_post))
        .route("/api/v2/browser-profile/stop", post(stop_post))
        .route("/api/v2/browser-profile/active", post(active_post))
        .route("/api/v2/browser-profile/local-active", get(local_active))
        .route("/api/v2/browser-profile/create", post(create_v2))
        .route("/api/v2/browser-profile/update", post(update_v2))
        .route("/api/v2/browser-profile/delete", post(delete_v2))
        .route("/api/v2/browser-profile/list", post(list_post))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state)
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(json!({"code": -1, "data": {}, "msg": "no such endpoint"})))
}

async fn status() -> Reply {
    Json(json!({"code": 0, "msg": "success"}))
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

async fn auth(State(st): State<AppState>, req: Request, next: Next) -> Response {
    if let Some(key) = st.api_key.as_deref()
        && req.uri().path() != "/status"
    {
        let presented = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if !ct_eq(presented.as_bytes(), key.as_bytes()) {
            let msg = "missing or invalid API key (send the header `Authorization: Bearer <api key>`)";
            return (StatusCode::UNAUTHORIZED, Json(json!({"code": -1, "data": {}, "msg": msg}))).into_response();
        }
    }
    next.run(req).await
}

// ---------------------------------------------------------------------------------------------------
// browser control

pub(crate) fn endpoints(r: &Running) -> Value {
    let webdriver = Kernel::from_path(&r.kernel)
        .ok()
        .and_then(|k| k.chromedriver())
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    json!({
        "ws": {"selenium": format!("127.0.0.1:{}", r.debug_port), "puppeteer": r.ws},
        "debug_port": r.debug_port.to_string(),
        "webdriver": webdriver,
    })
}

fn do_start(st: &AppState, key: &str, opts: LaunchOptions) -> CoreResult<Value> {
    let _one_at_a_time = st.open_lock.lock().unwrap_or_else(PoisonError::into_inner);
    let profile = Store::open(&st.home)?.get(key)?.clone();
    let kernel = Kernel::discover(&st.home, st.kernel.as_deref())?;
    // a profile of the team hub is opened under the hub's lock (refused when another device has it, whoever asks);
    // anything else is launched as it is
    let r = citro_sync::open(&st.home, &kernel, &profile, &opts).map_err(sync_err)?;
    // a failed bookkeeping write must not fail an open that already succeeded
    let _ = store::locked(&st.home, |s| s.touch_opened(&profile.profile_id));
    Ok(endpoints(&r))
}

async fn start_from(st: AppState, m: Map<String, Value>, v1: bool) -> Reply {
    let Some(key) = key_of(&m) else { return fail("profile_id (or profile_no) is required") };
    let opts = LaunchOptions {
        headless: truthy(&m, "headless", false),
        launch_args: m.get("launch_args").map(str_list).unwrap_or_default(),
        // v1's `open_tabs=1` means "do not reopen the last tabs"
        last_opened_tabs: if v1 { !truthy(&m, "open_tabs", false) } else { truthy(&m, "last_opened_tabs", true) },
        ..Default::default()
    };
    run(move || do_start(&st, &key, opts)).await
}

async fn start_get(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Reply {
    start_from(st, qmap(q), true).await
}

async fn start_post(State(st): State<AppState>, body: Bytes) -> Reply {
    match body_map(&body) {
        Ok(m) => start_from(st, m, false).await,
        Err(r) => r,
    }
}

async fn stop_from(st: AppState, m: Map<String, Value>) -> Reply {
    let Some(key) = key_of(&m) else { return fail("profile_id (or profile_no) is required") };
    run(move || {
        let id = resolve_id(&st.home, &key)?;
        // a profile of the team hub is closed, its data uploaded and its lock handed back before this answers
        let report = citro_sync::close(&st.home, &id, Duration::from_secs(120)).map_err(sync_err)?;
        match report.outcome {
            Some(out) if !out.is_clean() => {
                Err(Error::Invalid(format!("the browser is closed, but the hub side needs a look: {}", out.summary())))
            }
            Some(out) => Ok(json!({ "hub": out.summary() })),
            None => Ok(json!({})),
        }
    })
    .await
}

async fn stop_get(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Reply {
    stop_from(st, qmap(q)).await
}

async fn stop_post(State(st): State<AppState>, body: Bytes) -> Reply {
    match body_map(&body) {
        Ok(m) => stop_from(st, m).await,
        Err(r) => r,
    }
}

async fn active_from(st: AppState, m: Map<String, Value>) -> Reply {
    let Some(key) = key_of(&m) else { return fail("profile_id (or profile_no) is required") };
    run(move || {
        let id = resolve_id(&st.home, &key)?;
        Ok(match launch::running(&st.home, &id) {
            Some(r) => {
                let mut d = endpoints(&r);
                d["status"] = json!("Active");
                d
            }
            None => json!({"status": "Inactive"}),
        })
    })
    .await
}

async fn active_get(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Reply {
    active_from(st, qmap(q)).await
}

async fn active_post(State(st): State<AppState>, body: Bytes) -> Reply {
    match body_map(&body) {
        Ok(m) => active_from(st, m).await,
        Err(r) => r,
    }
}

async fn local_active(State(st): State<AppState>) -> Reply {
    run(move || {
        let list: Vec<Value> = launch::list_running(&st.home)
            .iter()
            .map(|r| {
                let mut d = endpoints(r);
                d["user_id"] = json!(r.profile_id);
                d["profile_id"] = json!(r.profile_id);
                d
            })
            .collect();
        Ok(json!({"list": list}))
    })
    .await
}

// ---------------------------------------------------------------------------------------------------
// profiles

/// Copy the fields present in `m` onto `p` (AdsPower field names, v1 and v2 spellings).
fn apply_body(p: &mut Profile, m: &Map<String, Value>) -> Result<(), String> {
    let set = |dst: &mut String, keys: &[&str]| {
        if let Some(v) = keys.iter().find_map(|k| field(m, k)) {
            *dst = v;
        }
    };
    set(&mut p.name, &["name"]);
    set(&mut p.group_id, &["group_id"]);
    set(&mut p.remark, &["remark"]);
    set(&mut p.domain_name, &["platform", "domain_name"]);
    set(&mut p.username, &["username"]);
    set(&mut p.password, &["password"]);
    set(&mut p.fakey, &["fakey"]);
    set(&mut p.ip, &["ip"]);
    set(&mut p.country, &["country"]);
    set(&mut p.category_id, &["category_id", "sys_app_cate_id"]);
    if p.name.chars().count() > 100 {
        return Err("name must be at most 100 characters".into());
    }
    if let Some(c) = m.get("cookie") {
        p.cookie = match c {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
    }
    if let Some(v) = m.get("tabs").or_else(|| m.get("open_urls")) {
        p.tabs = str_list(v);
    }
    if let Some(v) = m.get("launch_args") {
        p.launch_args = str_list(v);
    }
    if let Some(v) = m.get("user_proxy_config") {
        p.user_proxy_config =
            serde_json::from_value(v.clone()).map_err(|e| format!("invalid user_proxy_config: {e}"))?;
    } else if field(m, "proxyid").is_some_and(|v| !v.is_empty()) {
        return Err("saved proxies (`proxyid`) are not supported: pass `user_proxy_config` instead".into());
    }
    match m.get("fingerprint_config") {
        Some(Value::Object(fp)) => p.fingerprint_config.merge(fp),
        Some(Value::Null) | None => {}
        Some(_) => return Err("fingerprint_config must be an object".into()),
    }
    Ok(())
}

fn check_group(s: &Store, p: &Profile) -> CoreResult<()> {
    let g = p.group_id.as_str();
    if g.is_empty() || g == "0" || s.groups().iter().any(|x| x.group_id == g) {
        Ok(())
    } else {
        Err(Error::Invalid(format!("group {g} does not exist")))
    }
}

async fn create_from(st: AppState, m: Map<String, Value>, v1: bool) -> Reply {
    run(move || {
        let mut p = Profile::default();
        apply_body(&mut p, &m).map_err(Error::Invalid)?;
        let made = store::locked(&st.home, |s| {
            check_group(s, &p)?;
            s.create(p)
        })?;
        Ok(if v1 {
            json!({"id": made.profile_id, "serial_number": made.serial_number.to_string()})
        } else {
            json!({"profile_id": made.profile_id, "profile_no": made.serial_number.to_string()})
        })
    })
    .await
}

async fn create_v1(State(st): State<AppState>, body: Bytes) -> Reply {
    match body_map(&body) {
        Ok(m) => create_from(st, m, true).await,
        Err(r) => r,
    }
}

async fn create_v2(State(st): State<AppState>, body: Bytes) -> Reply {
    match body_map(&body) {
        Ok(m) => create_from(st, m, false).await,
        Err(r) => r,
    }
}

async fn update_from(st: AppState, m: Map<String, Value>) -> Reply {
    let Some(key) = key_of(&m) else { return fail("profile_id (or user_id) is required") };
    run(move || {
        // validated on a copy first: a bad request must leave the stored profile untouched. A profile of the team
        // hub is changed on the hub, so everybody gets the same one.
        citro_sync::edit(&st.home, &key, |s, staged| {
            apply_body(staged, &m).map_err(Error::Invalid)?;
            check_group(s, staged)
        })
        .map_err(sync_err)?;
        Ok(json!({}))
    })
    .await
}

async fn update_v1(State(st): State<AppState>, body: Bytes) -> Reply {
    match body_map(&body) {
        Ok(m) => update_from(st, m).await,
        Err(r) => r,
    }
}

async fn update_v2(State(st): State<AppState>, body: Bytes) -> Reply {
    match body_map(&body) {
        Ok(m) => update_from(st, m).await,
        Err(r) => r,
    }
}

async fn delete_from(st: AppState, m: Map<String, Value>) -> Reply {
    let keys: Vec<String> = ["profile_id", "user_ids", "user_id", "profile_no"]
        .iter()
        .filter_map(|k| m.get(*k))
        .flat_map(str_list)
        .collect();
    if keys.is_empty() {
        return fail("profile_id (a list of ids) is required");
    }
    // a profile of the team hub is the whole team's: it goes only when the request says `"everywhere": true`
    let everywhere = truthy(&m, "everywhere", false);
    run(move || {
        let mut gone = 0;
        for key in &keys {
            match citro_sync::delete(&st.home, key, everywhere) {
                Ok(_) => gone += 1,
                Err(citro_sync::Error::Core(Error::ProfileNotFound(_))) => {}
                Err(e) => return Err(sync_err(e)),
            }
        }
        if gone == 0 {
            return Err(Error::ProfileNotFound(keys.join(",")));
        }
        Ok(json!({}))
    })
    .await
}

async fn delete_v1(State(st): State<AppState>, body: Bytes) -> Reply {
    match body_map(&body) {
        Ok(m) => delete_from(st, m).await,
        Err(r) => r,
    }
}

async fn delete_v2(State(st): State<AppState>, body: Bytes) -> Reply {
    match body_map(&body) {
        Ok(m) => delete_from(st, m).await,
        Err(r) => r,
    }
}

fn row(s: &Store, p: &Profile) -> Value {
    json!({
        "serial_number": p.serial_number.to_string(),
        "profile_no": p.serial_number.to_string(),
        "user_id": p.profile_id,
        "profile_id": p.profile_id,
        "name": p.name,
        "group_id": p.group_id,
        "group_name": s.group_name(&p.group_id),
        "domain_name": p.domain_name,
        "platform": p.domain_name,
        "username": p.username,
        "password": p.password,
        "remark": p.remark,
        "sys_app_cate_id": p.category_id,
        "created_time": p.created_time.to_string(),
        "last_open_time": p.last_open_time.to_string(),
        "last_close_time": p.last_close_time.to_string(),
        "ip": p.ip,
        "ip_country": p.country,
        "user_proxy_config": p.user_proxy_config.redacted(),
    })
}

/// `user_sort` = `{"serial_number":"desc"}` (an object or a JSON string); default newest serial first.
fn sort_spec(m: &Map<String, Value>) -> (String, bool) {
    let parsed = match m.get("user_sort") {
        Some(Value::Object(o)) => Some(o.clone()),
        Some(Value::String(s)) => serde_json::from_str::<Map<String, Value>>(s).ok(),
        _ => None,
    };
    parsed
        .and_then(|o| o.into_iter().next())
        .map(|(k, v)| (k, !v.as_str().is_some_and(|d| d.eq_ignore_ascii_case("asc"))))
        .unwrap_or_else(|| ("serial_number".into(), true))
}

fn list_profiles(st: &AppState, m: &Map<String, Value>, v2: bool) -> CoreResult<Value> {
    launch::sync_closed(&st.home)?;
    let s = Store::open(&st.home)?;
    let wanted = |keys: &[&str]| -> Vec<String> { keys.iter().filter_map(|k| m.get(*k)).flat_map(str_list).collect() };
    let ids = wanted(&["profile_id", "user_id"]);
    let nos = wanted(&["profile_no", "serial_number"]);
    let group = field(m, "group_id").filter(|g| !g.is_empty());
    let mut rows: Vec<&Profile> = s
        .profiles()
        .iter()
        .filter(|p| {
            (ids.is_empty() || ids.contains(&p.profile_id))
                && (nos.is_empty() || nos.contains(&p.serial_number.to_string()))
                && group.as_ref().is_none_or(|g| &p.group_id == g)
        })
        .collect();
    let (key, desc) = sort_spec(m);
    rows.sort_by_key(|p| match key.as_str() {
        "last_open_time" => p.last_open_time,
        "last_close_time" => p.last_close_time,
        "created_time" => p.created_time,
        _ => p.serial_number as i64,
    });
    if desc {
        rows.reverse();
    }
    // AdsPower's documented default page size is 1 (max 100)
    let page = int(m, "page").unwrap_or(1).max(1);
    let size = int(m, "page_size").or_else(|| int(m, "limit")).unwrap_or(1).clamp(1, 100);
    let list: Vec<Value> = rows.iter().skip((page - 1) * size).take(size).map(|p| row(&s, p)).collect();
    let size_key = if v2 { "limit" } else { "page_size" };
    Ok(json!({"list": list, "page": page, size_key: size, "total": rows.len()}))
}

async fn list_get(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Reply {
    let m = qmap(q);
    run(move || list_profiles(&st, &m, false)).await
}

async fn list_post(State(st): State<AppState>, body: Bytes) -> Reply {
    match body_map(&body) {
        Ok(m) => run(move || list_profiles(&st, &m, true)).await,
        Err(r) => r,
    }
}

// ---------------------------------------------------------------------------------------------------
// groups

async fn group_list(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Reply {
    let m = qmap(q);
    run(move || {
        let s = Store::open(&st.home)?;
        let needle = field(&m, "group_name").map(|n| n.to_lowercase()).filter(|n| !n.is_empty());
        let all: Vec<Value> = s
            .groups()
            .iter()
            .filter(|g| needle.as_ref().is_none_or(|n| g.group_name.to_lowercase().contains(n)))
            .map(|g| json!({"group_id": g.group_id, "group_name": g.group_name, "remark": g.remark}))
            .collect();
        let page = int(&m, "page").unwrap_or(1).max(1);
        let size = int(&m, "page_size").unwrap_or(10).clamp(1, 100);
        let list: Vec<Value> = all.iter().skip((page - 1) * size).take(size).cloned().collect();
        Ok(json!({"list": list, "page": page, "page_size": size}))
    })
    .await
}

async fn group_create(State(st): State<AppState>, body: Bytes) -> Reply {
    let m = match body_map(&body) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let Some(name) = field(&m, "group_name").map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()) else {
        return fail("group_name is required");
    };
    let remark = field(&m, "remark").unwrap_or_default();
    run(move || {
        let g = store::locked(&st.home, |s| s.create_group(&name, &remark))?;
        Ok(json!({"group_id": g.group_id, "group_name": g.group_name}))
    })
    .await
}

// ---------------------------------------------------------------------------------------------------
// serving

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! { () = ctrl_c => {}, () = term => {} }
}

fn adopt_orphans(home: &Home) {
    match citro_sync::Hub::connect(home) {
        Ok(Some(hub)) => {
            for o in hub.adopt() {
                tracing::warn!(profile = %o.profile_id, "{}", o.summary());
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "cannot look for browsers that outlived their session"),
    }
}

/// Serve until Ctrl-C / SIGTERM. Running browsers are separate processes and keep running.
pub async fn serve(state: AppState, bind: SocketAddr) -> anyhow::Result<()> {
    // browsers of hub profiles that outlived their session (a crash, a kill): back under the lock, or stopped when
    // somebody else has the profile by now
    let home = state.home.clone();
    let _ = tokio::task::spawn_blocking(move || adopt_orphans(&home)).await;
    let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("cannot listen on {bind}"))?;
    let addr = listener.local_addr()?;
    // AdsPower publishes its address in a `local_api` file; so do we
    let local_api = state.home.root.join("local_api");
    std::fs::create_dir_all(&state.home.root)?;
    let shown = if addr.ip().is_unspecified() { "127.0.0.1".to_owned() } else { addr.ip().to_string() };
    std::fs::write(&local_api, format!("http://{shown}:{}\n", addr.port()))?;
    tracing::info!(%addr, auth = state.api_key.is_some(), home = %state.home.root.display(), "citro Local API listening");
    let served = axum::serve(listener, router(state)).with_graceful_shutdown(shutdown_signal()).await;
    let _ = std::fs::remove_file(&local_api);
    served?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A stand-in kernel that announces a listening port the way Chromium does, then idles.
    fn fake_kernel(root: &std::path::Path) -> (PathBuf, std::net::TcpListener) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let bin = root.join("chrome_157").join("SunBrowser");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in --user-data-dir=*) ud=\"${{a#--user-data-dir=}}\";; esac; done\n\
                 printf '%s\\n%s\\n' {port} /devtools/browser/api-test > \"$ud/DevToolsActivePort\"\nsleep 60 &\nwait\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        (bin, l)
    }

    async fn http(addr: SocketAddr, method: &str, path: &str, key: Option<&str>, body: Option<Value>) -> (u16, Value) {
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        );
        if let Some(k) = key {
            req.push_str(&format!("Authorization: Bearer {k}\r\n"));
        }
        req.push_str("\r\n");
        req.push_str(&body);
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf).into_owned();
        let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
        let payload = text.split_once("\r\n\r\n").map_or("", |(_, b)| b);
        (status, serde_json::from_str(payload).unwrap_or(Value::Null))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adspower_compatible_api_end_to_end() {
        let root = std::env::temp_dir().join(format!("citro-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (kernel, _kernel_port) = fake_kernel(&root);
        let state = AppState::new(Home { root: root.join("home") }, Some(kernel), Some("sekret".into()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, router(state)).into_future());
        let k = Some("sekret");

        // auth: /status is open, everything else needs the key
        assert_eq!(http(addr, "GET", "/status", None, None).await, (200, json!({"code": 0, "msg": "success"})));
        assert_eq!(http(addr, "GET", "/api/v1/user/list", None, None).await.0, 401);
        assert_eq!(http(addr, "GET", "/api/v1/user/list", Some("wrong"), None).await.0, 401);
        assert_eq!(http(addr, "GET", "/nope", k, None).await.0, 404);

        // groups
        let (_, g) =
            http(addr, "POST", "/api/v1/group/create", k, Some(json!({"group_name": "fr", "remark": "x"}))).await;
        let gid = g["data"]["group_id"].as_str().unwrap().to_owned();
        let (_, gl) = http(addr, "GET", "/api/v1/group/list", k, None).await;
        assert_eq!(gl["data"]["list"][0]["group_name"], "fr");

        // a profile that asks for a saved proxy is refused, not silently made direct
        let (_, bad) =
            http(addr, "POST", "/api/v2/browser-profile/create", k, Some(json!({"name": "x", "proxyid": "42"}))).await;
        assert_eq!(bad["code"], -1);
        let (_, bad_group) =
            http(addr, "POST", "/api/v2/browser-profile/create", k, Some(json!({"name": "x", "group_id": "999"})))
                .await;
        assert_eq!(bad_group["code"], -1);

        // create (v2), with a proxy whose port is a number and a partial fingerprint
        let (_, c) = http(
            addr,
            "POST",
            "/api/v2/browser-profile/create",
            k,
            Some(json!({
                "name": "alpha", "group_id": gid, "platform": "example.com", "remark": "r",
                "user_proxy_config": {"proxy_soft": "other", "proxy_type": "http", "proxy_host": "10.0.0.1", "proxy_port": 8080, "proxy_user": "u", "proxy_password": "hunter2"},
                "fingerprint_config": {"language": ["fr-FR", "fr"], "webrtc": "proxy"},
                "country": "fr"
            })),
        )
        .await;
        assert_eq!(c["code"], 0, "{c}");
        let pid = c["data"]["profile_id"].as_str().unwrap().to_owned();
        assert_eq!(c["data"]["profile_no"], "1");

        // v1 create returns {id}
        let (_, c1) =
            http(addr, "POST", "/api/v1/user/create", k, Some(json!({"name": "beta", "group_id": "0"}))).await;
        assert_eq!(c1["data"]["serial_number"], "2");

        // list: v1 defaults (page size 1, newest serial first), v2 filters, proxy password never echoed
        let (_, l1) = http(addr, "GET", "/api/v1/user/list?page_size=10", k, None).await;
        assert_eq!(l1["data"]["list"][0]["name"], "beta");
        assert_eq!(l1["data"]["list"][1]["group_name"], "fr");
        let (_, l2) =
            http(addr, "POST", "/api/v2/browser-profile/list", k, Some(json!({"profile_id": [pid], "limit": 5}))).await;
        assert_eq!(l2["data"]["list"].as_array().unwrap().len(), 1);
        assert_eq!(l2["data"]["list"][0]["user_proxy_config"]["proxy_password"], "********");
        assert!(!l2.to_string().contains("hunter2"));

        // update: partial fingerprint merge keeps earlier keys; a bad update changes nothing
        let (_, u) = http(
            addr,
            "POST",
            "/api/v2/browser-profile/update",
            k,
            Some(json!({"profile_id": pid, "name": "alpha2", "fingerprint_config": {"webrtc": "disabled"}})),
        )
        .await;
        assert_eq!(u["code"], 0, "{u}");
        let (_, u_bad) = http(
            addr,
            "POST",
            "/api/v2/browser-profile/update",
            k,
            Some(json!({"profile_id": pid, "name": "never", "fingerprint_config": 5})),
        )
        .await;
        assert_eq!(u_bad["code"], -1);
        let (_, l3) = http(addr, "POST", "/api/v2/browser-profile/list", k, Some(json!({"profile_no": ["1"]}))).await;
        assert_eq!(l3["data"]["list"][0]["name"], "alpha2");

        // open: endpoints in AdsPower's shape; second open is idempotent
        let (_, o) =
            http(addr, "POST", "/api/v2/browser-profile/start", k, Some(json!({"profile_id": pid, "headless": "1"})))
                .await;
        assert_eq!(o["code"], 0, "{o}");
        let port = o["data"]["debug_port"].as_str().unwrap().to_owned();
        assert_eq!(o["data"]["ws"]["selenium"], format!("127.0.0.1:{port}"));
        assert_eq!(o["data"]["ws"]["puppeteer"], format!("ws://127.0.0.1:{port}/devtools/browser/api-test"));
        let (_, o2) = http(addr, "GET", &format!("/api/v1/browser/start?user_id={pid}"), k, None).await;
        assert_eq!(o2["data"]["debug_port"], port);
        let (_, a) = http(addr, "GET", &format!("/api/v1/browser/active?user_id={pid}"), k, None).await;
        assert_eq!(a["data"]["status"], "Active");
        let (_, la) = http(addr, "GET", "/api/v1/browser/local-active", k, None).await;
        assert_eq!(la["data"]["list"][0]["user_id"], pid);
        let (_, la2) = http(addr, "GET", "/api/v2/browser-profile/local-active", k, None).await;
        assert_eq!(la2["data"]["list"][0]["profile_id"], pid);

        // close
        let (_, s) = http(addr, "POST", "/api/v2/browser-profile/stop", k, Some(json!({"profile_id": pid}))).await;
        assert_eq!(s["code"], 0, "{s}");
        let (_, a2) = http(addr, "POST", "/api/v2/browser-profile/active", k, Some(json!({"profile_no": "1"}))).await;
        assert_eq!(a2["data"]["status"], "Inactive");

        // delete wipes the record and the data directory
        let (_, d) = http(addr, "POST", "/api/v2/browser-profile/delete", k, Some(json!({"profile_id": [pid]}))).await;
        assert_eq!(d["code"], 0, "{d}");
        let (_, d2) = http(addr, "POST", "/api/v2/browser-profile/delete", k, Some(json!({"profile_id": [pid]}))).await;
        assert_eq!(d2["code"], -1);
        assert!(!root.join("home/profiles").join(&pid).exists());
        let (_, l4) = http(addr, "GET", "/api/v1/user/list?page_size=10", k, None).await;
        assert_eq!(l4["data"]["total"], 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
