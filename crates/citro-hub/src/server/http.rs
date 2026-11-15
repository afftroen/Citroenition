//! The HTTP surface of the hub: routing, bearer authentication, request parsing and response shaping.
//!
//! The rules (who may see or change what) live in the state layer; this file only translates HTTP into those calls.
//! Every state operation runs on a blocking thread (`App::run` / `App::read`): it takes the global store mutex and
//! may write to disk. Request bodies are read by hand so that every failure is a JSON `{"error":{..}}` answer.

use super::blobs;
use super::csv::export_profiles;
use super::error::{ApiError, ApiResult};
use super::fsio::{create_private_dir, private_options};
use super::limiter::{AuthLimiter, RateLimiter};
use super::logs::{LogQuery, SERVER_LOG};
use super::profiles::ProfileQuery;
use super::state::{Cx, State as HubState};
use super::store::Store;
use super::util::{clean_text, hex, iso_utc, random_alnum};
use crate::model::{
    DATA_SHA256_HEADER, DATA_VERSION_HEADER, DEVICE_HEADER, DataMeta, DataVersion, HubGroup, HubProfile, LEASE_HEADER,
    Lease, LogEntry, LogKind, LogLevel, LogRecord, Me, Member, ProfilePage, TeamEntry, TeamRole, User, UserRole,
};
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Extension, Json, Router};
use citro_core::profile::Profile;
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// How much of a refused request body is read and thrown away so the client can still see our answer.
const DRAIN_CAP: usize = 16 * 1024 * 1024;
/// A data upload that sends nothing for this long is dropped (and its temp file removed).
const UPLOAD_IDLE: Duration = Duration::from_secs(60);
/// Chunk size of a data download.
const DOWNLOAD_CHUNK: usize = 64 * 1024;
const MAX_BULK_DELETE: usize = 1000;
const DEFAULT_LOG_LIMIT: usize = 200;
const MAX_LOG_LIMIT: usize = 5000;

#[derive(Clone)]
pub struct App {
    store: Arc<Store>,
    limiter: Arc<AuthLimiter>,
    rate: Arc<RateLimiter>,
}

/// Who is calling: the authenticated user and the device they announced (`X-Citro-Device`, for log lines only).
#[derive(Clone)]
struct Caller {
    user: User,
    device: String,
}

impl App {
    async fn run<T: Send + 'static>(
        &self,
        c: &Caller,
        f: impl FnOnce(&mut HubState, &mut Cx) -> ApiResult<T> + Send + 'static,
    ) -> ApiResult<T> {
        let (store, user, device) = (self.store.clone(), c.user.clone(), c.device.clone());
        blocking(move || store.run(&user, &device, f)).await?
    }

    async fn read<T: Send + 'static>(
        &self,
        c: &Caller,
        f: impl FnOnce(&mut HubState, &mut Cx) -> ApiResult<T> + Send + 'static,
    ) -> ApiResult<T> {
        let (store, user, device) = (self.store.clone(), c.user.clone(), c.device.clone());
        blocking(move || store.read(&user, &device, f)).await?
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| ApiError::internal(format!("worker failed: {e}")))
}

// ---- request helpers -----------------------------------------------------------------------------------------------

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty()).then(|| token.trim())
}

/// The address a request counts for: the peer, or — behind a trusted reverse proxy — the last entry of the
/// configured header (the one the proxy itself appended).
fn client_ip(real_ip_header: Option<&str>, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    real_ip_header
        .and_then(|name| headers.get(name))
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .and_then(|s| s.trim().parse::<IpAddr>().ok())
        .unwrap_or_else(|| peer.ip())
}

fn lease_of(headers: &HeaderMap) -> Option<String> {
    headers.get(LEASE_HEADER).map(|v| String::from_utf8_lossy(v.as_bytes()).trim().to_owned()).filter(|s| !s.is_empty())
}

fn content_length(headers: &HeaderMap) -> ApiResult<Option<u64>> {
    match headers.get(header::CONTENT_LENGTH) {
        None => Ok(None),
        Some(v) => v
            .to_str()
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(Some)
            .ok_or_else(|| ApiError::invalid("bad Content-Length header")),
    }
}

fn parse_query<T: DeserializeOwned>(uri: &Uri) -> ApiResult<T> {
    serde_urlencoded::from_str(uri.query().unwrap_or(""))
        .map_err(|e| ApiError::invalid(format!("bad query string: {e}")))
}

fn parse_flag(v: Option<&str>, name: &str) -> ApiResult<bool> {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("0") | Some("false") | Some("no") => Ok(false),
        Some("1") | Some("true") | Some("yes") => Ok(true),
        Some(other) => Err(ApiError::invalid(format!("`{name}` must be 0 or 1, not `{other}`"))),
    }
}

/// Read and throw away what a refused client is still sending (bounded), so it can read our answer.
async fn drain<S: Stream<Item = Result<Bytes, axum::Error>> + Unpin>(stream: &mut S, mut cap: usize) {
    while cap > 0 {
        match stream.next().await {
            Some(Ok(chunk)) => cap = cap.saturating_sub(chunk.len()),
            _ => break,
        }
    }
}

async fn read_limited(headers: &HeaderMap, body: Body, limit: usize) -> ApiResult<Vec<u8>> {
    let too_big = || ApiError::too_large(format!("the request body is larger than {limit} bytes"));
    let mut stream = body.into_data_stream();
    if content_length(headers)?.is_some_and(|n| n > limit as u64) {
        drain(&mut stream, DRAIN_CAP).await;
        return Err(too_big());
    }
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ApiError::invalid("the request body could not be read"))?;
        if buf.len() + chunk.len() > limit {
            drain(&mut stream, DRAIN_CAP).await;
            return Err(too_big());
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

async fn read_json<T: DeserializeOwned>(app: &App, headers: &HeaderMap, body: Body) -> ApiResult<T> {
    let bytes = read_limited(headers, body, app.store.config().max_json_bytes).await?;
    serde_json::from_slice(&bytes).map_err(|e| ApiError::invalid(format!("invalid JSON body: {e}")))
}

/// Like [`read_json`], but an empty body means "all defaults".
async fn read_json_or_default<T: DeserializeOwned + Default>(
    app: &App,
    headers: &HeaderMap,
    body: Body,
) -> ApiResult<T> {
    let bytes = read_limited(headers, body, app.store.config().max_json_bytes).await?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(&bytes).map_err(|e| ApiError::invalid(format!("invalid JSON body: {e}")))
}

fn header_name(s: &str) -> HeaderName {
    HeaderName::from_bytes(s.as_bytes()).expect("a valid header name")
}

// ---- authentication ------------------------------------------------------------------------------------------------

/// Set on the response of an authenticated request, so the access log can name the user.
#[derive(Clone)]
struct AuthedUser(String);

/// One line per request: who (address and user name), what (method and path, never the query string or any header)
/// and how it went. Tokens, leases and profile data never appear here.
async fn access_log(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let ip = client_ip(app.store.config().real_ip_header.as_deref(), peer, req.headers());
    let (method, path) = (req.method().clone(), req.uri().path().to_owned());
    let resp = next.run(req).await;
    let user = resp.extensions().get::<AuthedUser>().map_or("-", |u| u.0.as_str());
    let (status, ms) = (resp.status().as_u16(), started.elapsed().as_millis());
    if status >= 500 {
        tracing::error!(target: "citro_hub::access", %ip, user, %method, path, status, ms, "request");
    } else {
        tracing::info!(target: "citro_hub::access", %ip, user, %method, path, status, ms, "request");
    }
    resp
}

async fn authenticate(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request,
    next: Next,
) -> Response {
    let ip = client_ip(app.store.config().real_ip_header.as_deref(), peer, req.headers());
    if let Some(wait) = app.limiter.blocked(ip, Instant::now()) {
        return ApiError::rate_limited(wait).into_response();
    }
    let token = bearer_token(req.headers()).map(str::to_owned);
    let user = match token {
        Some(t) => {
            let store = app.store.clone();
            blocking(move || store.authenticate(&t)).await.ok().flatten()
        }
        None => None,
    };
    let Some(user) = user else {
        let blocked = app.limiter.fail(ip, Instant::now());
        let store = app.store.clone();
        let note = if blocked { "; too many failures, the address is blocked for a while" } else { "" };
        let msg = format!("authentication failed from {ip}{note}");
        let level = if blocked { LogLevel::Error } else { LogLevel::Warn };
        let _ = blocking(move || store.log_server(level, Some(ip), &msg)).await;
        return ApiError::unauthorized("missing, unknown or revoked token").into_response();
    };
    let authed = AuthedUser(user.name.clone());
    if let Err(wait) = app.rate.check(ip, &user.id, Instant::now()) {
        let mut resp = ApiError::rate_limited(wait)
            .with_message(format!("too many requests from this user and address; retry in {wait} s"))
            .into_response();
        resp.extensions_mut().insert(authed);
        return resp;
    }
    let device = req
        .headers()
        .get(DEVICE_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|d| clean_text(d, 128))
        .unwrap_or_default();
    req.extensions_mut().insert(Caller { user, device });
    let mut resp = next.run(req).await;
    resp.extensions_mut().insert(authed);
    resp
}

/// Nothing a hub says may be cached by a proxy in between.
async fn harden(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static("no-store"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    resp
}

// ---- handlers: plain ---------------------------------------------------------------------------------------------

async fn healthz() -> Json<Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

async fn not_found_fallback() -> ApiError {
    ApiError::not_found("no such endpoint")
}

async fn method_not_allowed() -> Response {
    let mut e = ApiError::invalid("method not allowed for this endpoint");
    e.status = StatusCode::METHOD_NOT_ALLOWED;
    e.into_response()
}

async fn me(State(app): State<App>, Extension(c): Extension<Caller>) -> ApiResult<Json<Me>> {
    let me = app.read(&c, |st, cx| Ok(Me { user: cx.user.clone(), teams: st.teams_of(&cx.user) })).await?;
    Ok(Json(me))
}

// ---- handlers: teams and members -----------------------------------------------------------------------------------

async fn list_teams(State(app): State<App>, Extension(c): Extension<Caller>) -> ApiResult<Json<Vec<TeamEntry>>> {
    Ok(Json(app.read(&c, |st, cx| Ok(st.teams_of(&cx.user))).await?))
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn create_team(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<TeamEntry>> {
    let b: NameBody = read_json(&app, &headers, body).await?;
    Ok(Json(app.run(&c, move |st, cx| st.create_team(cx, &b.name)).await?))
}

async fn list_members(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(tid): Path<String>,
) -> ApiResult<Json<Vec<Member>>> {
    Ok(Json(app.read(&c, move |st, cx| st.members_of(&cx.user, &tid)).await?))
}

fn member_role() -> TeamRole {
    TeamRole::Member
}

#[derive(Deserialize)]
struct MemberBody {
    user_name: String,
    #[serde(default = "member_role")]
    role: TeamRole,
}

async fn add_member(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(tid): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<Member>> {
    let b: MemberBody = read_json(&app, &headers, body).await?;
    Ok(Json(app.run(&c, move |st, cx| st.add_member(cx, &tid, &b.user_name, b.role)).await?))
}

async fn remove_member(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path((tid, uid)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    app.run(&c, move |st, cx| st.remove_member(cx, &tid, &uid)).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---- handlers: groups ----------------------------------------------------------------------------------------------

async fn list_groups(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(tid): Path<String>,
) -> ApiResult<Json<Vec<HubGroup>>> {
    Ok(Json(app.read(&c, move |st, cx| st.groups_of(&cx.user, &tid)).await?))
}

#[derive(Deserialize)]
struct GroupBody {
    name: String,
    #[serde(default)]
    remark: String,
}

async fn create_group(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(tid): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<HubGroup>> {
    let b: GroupBody = read_json(&app, &headers, body).await?;
    Ok(Json(app.run(&c, move |st, cx| st.create_group(cx, &tid, &b.name, &b.remark)).await?))
}

#[derive(Deserialize)]
struct GroupPatch {
    name: Option<String>,
    remark: Option<String>,
}

async fn update_group(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(gid): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<HubGroup>> {
    let b: GroupPatch = read_json(&app, &headers, body).await?;
    Ok(Json(app.run(&c, move |st, cx| st.update_group(cx, &gid, b.name.as_deref(), b.remark.as_deref())).await?))
}

async fn delete_group(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(gid): Path<String>,
) -> ApiResult<Json<Value>> {
    let moved = app.run(&c, move |st, cx| st.delete_group(cx, &gid)).await?;
    Ok(Json(json!({ "ok": true, "moved": moved })))
}

// ---- handlers: profiles --------------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct ProfilesQuery {
    group: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn list_profiles(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(tid): Path<String>,
    uri: Uri,
) -> ApiResult<Json<ProfilePage>> {
    let q: ProfilesQuery = parse_query(&uri)?;
    let query = ProfileQuery { group: q.group, q: q.q, limit: q.limit, offset: q.offset };
    Ok(Json(app.read(&c, move |st, cx| st.list_profiles(&cx.user, &tid, &query, cx.now_ms)).await?))
}

async fn create_profile(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(tid): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<HubProfile>> {
    let p: Profile = read_json(&app, &headers, body).await?;
    Ok(Json(app.run(&c, move |st, cx| st.create_profile(cx, &tid, p)).await?))
}

async fn get_profile(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(pid): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<HubProfile>> {
    let lease = lease_of(&headers);
    Ok(Json(app.read(&c, move |st, cx| st.profile_detail(&cx.user, &pid, lease.as_deref(), cx.now_ms)).await?))
}

async fn patch_profile(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(pid): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<HubProfile>> {
    let lease = lease_of(&headers);
    let patch: Value = read_json(&app, &headers, body).await?;
    Ok(Json(app.run(&c, move |st, cx| st.patch_profile(cx, &pid, &patch, lease.as_deref())).await?))
}

/// Remove a profile's hosted data after the profile itself is gone.
async fn remove_blobs(app: &App, pid: &str) {
    if let Some(dir) = app.store.blob_dir(pid) {
        if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("cannot remove the data of deleted profile {pid}: {e}");
            }
        }
    }
}

async fn delete_profile(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(pid): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = pid.clone();
    app.run(&c, move |st, cx| st.delete_profile(cx, &p)).await?;
    remove_blobs(&app, &pid).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct BulkDeleteBody {
    ids: Vec<String>,
}

async fn bulk_delete(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(tid): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<Value>> {
    let b: BulkDeleteBody = read_json(&app, &headers, body).await?;
    if b.ids.len() > MAX_BULK_DELETE {
        return Err(ApiError::too_large(format!("at most {MAX_BULK_DELETE} profiles per request")));
    }
    let (deleted, skipped) = app.run(&c, move |st, cx| st.bulk_delete(cx, &tid, &b.ids)).await?;
    for pid in &deleted {
        remove_blobs(&app, pid).await;
    }
    let skipped: Vec<Value> = skipped.into_iter().map(|(id, reason)| json!({ "id": id, "reason": reason })).collect();
    Ok(Json(json!({ "deleted": deleted, "skipped": skipped })))
}

// ---- handlers: locks -----------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct LockBody {
    device_id: String,
    #[serde(default)]
    device_name: String,
}

async fn lock(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(pid): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<Lease>> {
    let b: LockBody = read_json(&app, &headers, body).await?;
    Ok(Json(app.run(&c, move |st, cx| st.lock_profile(cx, &pid, &b.device_id, &b.device_name)).await?))
}

async fn heartbeat(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(pid): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Lease>> {
    let lease = lease_of(&headers);
    Ok(Json(app.run(&c, move |st, cx| st.heartbeat(cx, &pid, lease.as_deref())).await?))
}

#[derive(Deserialize, Default)]
struct UnlockBody {
    #[serde(default)]
    closed: bool,
}

async fn unlock(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(pid): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<Value>> {
    let lease = lease_of(&headers);
    let b: UnlockBody = read_json_or_default(&app, &headers, body).await?;
    app.run(&c, move |st, cx| st.unlock(cx, &pid, lease.as_deref(), b.closed)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn force_unlock(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(pid): Path<String>,
) -> ApiResult<Json<Value>> {
    let was_locked = app.run(&c, move |st, cx| st.force_unlock(cx, &pid)).await?;
    Ok(Json(json!({ "ok": true, "was_locked": was_locked })))
}

// ---- handlers: hosted data -----------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct DataQuery {
    /// Push: the version the data is based on; refused (409) when somebody pushed in between.
    base_version: Option<u64>,
    /// Pull: an older version that is still kept instead of the current one.
    version: Option<u64>,
}

/// The entity tags of an `If-Match` / `If-None-Match` header: lower-cased, without quotes or `W/`.
fn etags(v: &str) -> Vec<String> {
    v.split(',')
        .map(|t| t.trim().trim_start_matches("W/").trim_matches('"').to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

fn etag_header(headers: &HeaderMap, name: HeaderName) -> Option<Vec<String>> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(etags)
}

/// The conditions of a data push; all of them must hold, and they are checked twice: before the body is read and
/// again under the state mutex right before the new version is put in place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Precondition {
    base_version: Option<u64>,
    /// `If-Match`: the ETag (SHA-256) of the stored data, or `*` for "anything stored".
    if_match: Option<Vec<String>>,
    /// `If-None-Match: *`: only when nothing is stored yet.
    if_none_match_any: bool,
}

impl Precondition {
    fn from_request(headers: &HeaderMap, q: &DataQuery) -> Precondition {
        Precondition {
            base_version: q.base_version,
            if_match: etag_header(headers, header::IF_MATCH),
            if_none_match_any: etag_header(headers, header::IF_NONE_MATCH).is_some_and(|t| t.iter().any(|x| x == "*")),
        }
    }

    fn check(&self, meta: &DataMeta) -> ApiResult<()> {
        if self.base_version.is_some_and(|b| b != meta.version) {
            return Err(ApiError::stale(meta.version));
        }
        if let Some(tags) = &self.if_match {
            if meta.version == 0 || !tags.iter().any(|t| t == "*" || *t == meta.sha256) {
                return Err(ApiError::precondition_failed(meta.version));
            }
        }
        if self.if_none_match_any && meta.version > 0 {
            return Err(ApiError::precondition_failed(meta.version));
        }
        Ok(())
    }
}

/// An upload's temp file; removed again unless the upload made it into place (also when the client hangs up and
/// the handler is dropped half way).
struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl TempFile {
    fn new(path: PathBuf) -> TempFile {
        TempFile { path, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Stream the request body into `path`, hashing on the way. Never holds more than one chunk in memory.
async fn receive(body: Body, path: &FsPath, max: u64) -> ApiResult<(u64, String)> {
    let file = private_options().write(true).create_new(true).open(path)?;
    let mut file = tokio::fs::File::from_std(file);
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut stream = body.into_data_stream();
    loop {
        let next = tokio::time::timeout(UPLOAD_IDLE, stream.next())
            .await
            .map_err(|_| ApiError::invalid("the upload stalled and was dropped"))?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|_| ApiError::invalid("the upload was interrupted"))?;
        size += chunk.len() as u64;
        if size > max {
            return Err(ApiError::too_large(format!("the data is larger than the limit of {max} bytes")));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    file.sync_all().await?;
    Ok((size, hex(&hasher.finalize())))
}

async fn put_data(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(pid): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<DataMeta>> {
    let lease = lease_of(&headers);
    let pre = Precondition::from_request(&headers, &parse_query::<DataQuery>(&uri)?);
    let declared = content_length(&headers)?;
    let (max, keep) = (app.store.config().max_blob_bytes, app.store.config().keep_versions);
    let dir = app.store.blob_dir(&pid).ok_or_else(|| ApiError::not_found("no such profile"))?;

    // 1. Who may write, and do the conditions hold? Answered before a single body byte is read, so a client that
    //    sent `Expect: 100-continue` learns it without uploading anything.
    {
        let (p, l, pre) = (pid.clone(), lease.clone(), pre.clone());
        app.read(&c, move |st, cx| pre.check(&st.data_access(&cx.user, &p, l.as_deref(), cx.now_ms)?)).await?;
    }
    if declared.is_some_and(|n| n > max) {
        return Err(ApiError::too_large(format!("the data is larger than the limit of {max} bytes")));
    }
    if declared == Some(0) {
        return Err(ApiError::invalid("empty data is not accepted"));
    }

    // 2. Stream the body into a temp file next to the final place.
    create_private_dir(&dir)?;
    let tmp = TempFile::new(dir.join(format!("upload-{}", random_alnum(12))));
    let (size, sha) = receive(body, &tmp.path, max).await?;
    if size == 0 {
        return Err(ApiError::invalid("empty data is not accepted"));
    }

    // 3. Commit under the state mutex: the lease must still be live and the conditions must hold right now. The
    //    new version is `current + 1`, so the numbers only ever grow.
    let (p, l, tmp_path) = (pid.clone(), lease, tmp.path.clone());
    let meta = app
        .run(&c, move |st, cx| {
            let current = st.data_access(&cx.user, &p, l.as_deref(), cx.now_ms)?;
            pre.check(&current)?;
            if let Some(lock) = st.locks.get(&p) {
                cx.device_id = lock.device_id.clone();
            }
            blobs::install(&dir, &tmp_path, current.version + 1)?;
            let meta = st.data_commit_apply(cx, &p, size, sha);
            blobs::prune(&dir, keep);
            Ok(meta)
        })
        .await?;
    tmp.disarm();
    Ok(Json(meta))
}

/// What a data pull answers with.
enum Pull {
    /// Nothing is stored yet.
    Empty,
    /// `If-None-Match` named the stored data: the client has it already.
    NotModified,
    Current,
    /// A kept older version (`?version=N`).
    Old(u64),
}

fn data_headers(h: &mut HeaderMap, version: u64, sha256: &str) {
    h.insert(header_name(DATA_VERSION_HEADER), HeaderValue::from(version));
    if let Ok(v) = HeaderValue::from_str(sha256) {
        h.insert(header_name(DATA_SHA256_HEADER), v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("\"{sha256}\"")) {
        h.insert(header::ETAG, v);
    }
}

async fn get_data(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(pid): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let lease = lease_of(&headers);
    let want = parse_query::<DataQuery>(&uri)?.version;
    let none_match = etag_header(&headers, header::IF_NONE_MATCH);
    let dir = app.store.blob_dir(&pid).ok_or_else(|| ApiError::not_found("no such profile"))?;
    let p = pid.clone();
    // Reading is as exclusive as writing: `data_access` refuses everybody but the holder of the live lock.
    let (meta, pull) = app
        .read(&c, move |st, cx| {
            let meta = st.data_access(&cx.user, &p, lease.as_deref(), cx.now_ms)?;
            if let Some(lock) = st.locks.get(&p) {
                cx.device_id = lock.device_id.clone();
            }
            let pull = match want {
                _ if meta.version == 0 => Pull::Empty,
                Some(v) if v == 0 || v > meta.version => {
                    return Err(ApiError::not_found(format!("no version {v} (the current one is {})", meta.version)));
                }
                Some(v) if v != meta.version => Pull::Old(v),
                _ if none_match.is_some_and(|t| t.iter().any(|x| x == "*" || *x == meta.sha256)) => Pull::NotModified,
                _ => Pull::Current,
            };
            let hp = &st.profiles[&p];
            let msg = match &pull {
                Pull::Empty => None,
                Pull::NotModified => Some(format!("data of `{}` is up to date at v{}", hp.profile.name, meta.version)),
                Pull::Old(v) => Some(format!("data pulled for `{}`: older version v{v}", hp.profile.name)),
                Pull::Current => {
                    Some(format!("data pulled for `{}`: v{} ({} bytes)", hp.profile.name, meta.version, meta.size))
                }
            };
            if let Some(m) = msg {
                let team = hp.team_id.clone();
                cx.log(&team, LogLevel::Info, Some(&p), m);
            }
            Ok((meta, pull))
        })
        .await?;
    let (version, sha256, path) = match pull {
        Pull::Empty => return Ok(StatusCode::NO_CONTENT.into_response()),
        Pull::NotModified => {
            let mut resp = StatusCode::NOT_MODIFIED.into_response();
            data_headers(resp.headers_mut(), meta.version, &meta.sha256);
            return Ok(resp);
        }
        Pull::Current => (meta.version, meta.sha256.clone(), blobs::file_path(&dir, meta.version)),
        Pull::Old(v) => {
            let path = blobs::file_path(&dir, v);
            let hashed = {
                let path = path.clone();
                blocking(move || blobs::hash_file(&path)).await?
            };
            match hashed {
                Ok((_, sha)) => (v, sha, path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(ApiError::not_found(format!("version {v} is not kept any more")));
                }
                Err(e) => return Err(e.into()),
            }
        }
    };
    let file = tokio::fs::File::open(&path).await.map_err(|e| {
        tracing::error!("the data v{version} of profile {pid} is recorded but cannot be opened: {e}");
        ApiError::internal("the stored data is missing")
    })?;
    let len = file.metadata().await?.len();
    let stream = futures_util::stream::unfold((file, len), |(mut file, left)| async move {
        if left == 0 {
            return None;
        }
        let mut buf = vec![0u8; left.min(DOWNLOAD_CHUNK as u64) as usize];
        match file.read_exact(&mut buf).await {
            Ok(_) => {
                let n = buf.len() as u64;
                Some((Ok(Bytes::from(buf)), (file, left - n)))
            }
            Err(e) => Some((Err(e), (file, 0))),
        }
    });
    let mut resp = Response::new(Body::from_stream(stream));
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    data_headers(h, version, &sha256);
    Ok(resp)
}

/// The versions of a profile's data that are still on disk, newest first. Metadata only, so any team member (the
/// lease is not needed) may look; the content needs the lease like everything else.
async fn data_versions(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(pid): Path<String>,
) -> ApiResult<Json<Vec<DataVersion>>> {
    let dir = app.store.blob_dir(&pid).ok_or_else(|| ApiError::not_found("no such profile"))?;
    let cur = app
        .read(&c, move |st, cx| {
            let (hp, _) = st.profile_role_min(&cx.user, &pid, TeamRole::Member)?;
            Ok(hp.data.clone())
        })
        .await?;
    let files = blocking(move || blobs::list(&dir)).await?;
    Ok(Json(
        files
            .into_iter()
            .rev()
            .map(|f| {
                let current = f.version == cur.version;
                DataVersion {
                    version: f.version,
                    size: f.size,
                    modified_at: f.modified,
                    current,
                    sha256: current.then(|| cur.sha256.clone()),
                }
            })
            .collect(),
    ))
}

// ---- handlers: logs and CSV ----------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct LogsBody {
    device_id: String,
    entries: Vec<LogEntry>,
}

async fn post_logs(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<Value>> {
    let b: LogsBody = read_json(&app, &headers, body).await?;
    let accepted = b.entries.len();
    let records =
        app.read(&c, move |st, cx| st.stamp_log_entries(&cx.user, &b.device_id, &b.entries, cx.now())).await?;
    let store = app.store.clone();
    blocking(move || store.append_logs(records)).await??;
    Ok(Json(json!({ "accepted": accepted })))
}

#[derive(Deserialize, Default)]
struct LogsQuery {
    /// Only for `GET /v1/logs`: the team (id or exact name); optional when the caller is in just one team.
    team: Option<String>,
    kind: Option<String>,
    #[serde(alias = "profile")]
    profile_id: Option<String>,
    level: Option<String>,
    since: Option<i64>,
    limit: Option<usize>,
}

fn log_query(q: LogsQuery) -> ApiResult<LogQuery> {
    let kind = q
        .kind
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::parse::<LogKind>)
        .transpose()
        .map_err(ApiError::invalid)?;
    let levels = match q.level.as_deref() {
        None | Some("") => Vec::new(),
        Some(list) => {
            list.split(',').map(|l| l.parse::<LogLevel>().map_err(ApiError::invalid)).collect::<ApiResult<Vec<_>>>()?
        }
    };
    Ok(LogQuery {
        kind,
        profile_id: q.profile_id.filter(|p| !p.is_empty()),
        levels,
        since: q.since,
        limit: q.limit.filter(|l| *l > 0).unwrap_or(DEFAULT_LOG_LIMIT).min(MAX_LOG_LIMIT),
    })
}

async fn team_logs(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(tid): Path<String>,
    uri: Uri,
) -> ApiResult<Json<Vec<LogRecord>>> {
    let q = log_query(parse_query(&uri)?)?;
    let t = tid.clone();
    app.read(&c, move |st, cx| st.require_role(&cx.user, &t, TeamRole::Viewer).map(|_| ())).await?;
    let store = app.store.clone();
    Ok(Json(blocking(move || store.query_logs(&tid, &q)).await??))
}

/// `GET /v1/logs?team=&profile=&kind=&level=&since=&limit=`: the same as the team endpoint with the team as a
/// query parameter (id or exact name; it may be left out when the caller is in exactly one team).
async fn logs(State(app): State<App>, Extension(c): Extension<Caller>, uri: Uri) -> ApiResult<Json<Vec<LogRecord>>> {
    let raw: LogsQuery = parse_query(&uri)?;
    let team = raw.team.clone().filter(|t| !t.trim().is_empty());
    let q = log_query(raw)?;
    let tid = app
        .read(&c, move |st, cx| {
            let visible = st.teams_of(&cx.user);
            let entry = match &team {
                Some(t) => visible
                    .iter()
                    .find(|e| e.team.id == *t)
                    .or_else(|| visible.iter().find(|e| e.team.name.eq_ignore_ascii_case(t.trim())))
                    .ok_or_else(|| ApiError::not_found("no such team"))?,
                None if visible.len() == 1 => &visible[0],
                None => return Err(ApiError::invalid("pass ?team=ID (or the team name): you are in several teams")),
            };
            Ok(entry.team.id.clone())
        })
        .await?;
    let store = app.store.clone();
    Ok(Json(blocking(move || store.query_logs(&tid, &q)).await??))
}

/// Server-level events (failed logins, start-ups) that belong to no team; server admins only.
async fn admin_logs(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    uri: Uri,
) -> ApiResult<Json<Vec<LogRecord>>> {
    let q = log_query(parse_query(&uri)?)?;
    require_server_admin(&app, &c).await?;
    let store = app.store.clone();
    Ok(Json(blocking(move || store.query_logs(SERVER_LOG, &q)).await??))
}

async fn admin_users(State(app): State<App>, Extension(c): Extension<Caller>) -> ApiResult<Json<Vec<User>>> {
    Ok(Json(
        app.read(&c, |st, cx| {
            if cx.user.role != UserRole::Admin {
                return Err(ApiError::forbidden("this needs a server admin"));
            }
            Ok(st.users())
        })
        .await?,
    ))
}

async fn require_server_admin(app: &App, c: &Caller) -> ApiResult<()> {
    app.read(c, |_, cx| {
        if cx.user.role == UserRole::Admin { Ok(()) } else { Err(ApiError::forbidden("this needs a server admin")) }
    })
    .await
}

/// `acme-profiles-2026-11-15.csv`-style file names: letters, digits and dashes only.
fn csv_filename(team: &str, now_secs: i64) -> String {
    let mut slug: String =
        team.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' }).collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "team" } else { slug };
    format!("{slug}-profiles-{}.csv", &iso_utc(now_secs)[..10])
}

async fn export_csv(
    State(app): State<App>,
    Extension(c): Extension<Caller>,
    Path(tid): Path<String>,
    uri: Uri,
) -> ApiResult<Response> {
    #[derive(Deserialize, Default)]
    struct Q {
        group: Option<String>,
        bom: Option<String>,
    }
    let q: Q = parse_query(&uri)?;
    let bom = parse_flag(q.bom.as_deref(), "bom")?;
    let group = q.group;
    let (bytes, filename) = app
        .read(&c, move |st, cx| {
            let bytes = export_profiles(st, &cx.user, &tid, group.as_deref(), bom, cx.now_ms)?;
            cx.log(&tid, LogLevel::Info, None, format!("profiles exported as CSV ({} bytes)", bytes.len()));
            Ok((bytes, csv_filename(&st.team_name(&tid), cx.now())))
        })
        .await?;
    let mut resp = Response::new(Body::from(bytes));
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

// ---- the router ----------------------------------------------------------------------------------------------------

pub fn router(store: Arc<Store>) -> Router {
    let cfg = store.config();
    let limiter = Arc::new(AuthLimiter::new(cfg.auth_fail_limit, cfg.auth_fail_window, cfg.auth_block));
    let rate = Arc::new(RateLimiter::new(cfg.rate_limit_per_min));
    let app = App { store, limiter, rate };
    let v1: Router<App> = Router::new()
        .route("/me", get(me))
        .route("/teams", get(list_teams).post(create_team))
        .route("/teams/{tid}/members", get(list_members).post(add_member))
        .route("/teams/{tid}/members/{uid}", delete(remove_member))
        .route("/teams/{tid}/groups", get(list_groups).post(create_group))
        .route("/groups/{gid}", patch(update_group).delete(delete_group))
        .route("/teams/{tid}/profiles", get(list_profiles).post(create_profile))
        .route("/teams/{tid}/profiles/bulk-delete", post(bulk_delete))
        .route("/teams/{tid}/profiles.csv", get(export_csv))
        .route("/teams/{tid}/export/profiles.csv", get(export_csv))
        .route("/teams/{tid}/logs", get(team_logs))
        .route("/profiles/{pid}", get(get_profile).patch(patch_profile).delete(delete_profile))
        .route("/profiles/{pid}/lock", post(lock))
        .route("/profiles/{pid}/heartbeat", post(heartbeat))
        .route("/profiles/{pid}/unlock", post(unlock))
        .route("/profiles/{pid}/force-unlock", post(force_unlock))
        .route("/profiles/{pid}/data", get(get_data).put(put_data))
        .route("/profiles/{pid}/data/versions", get(data_versions))
        .route("/logs", get(logs).post(post_logs))
        .route("/admin/logs", get(admin_logs))
        .route("/admin/users", get(admin_users))
        .layer(middleware::from_fn_with_state(app.clone(), authenticate));
    Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", v1)
        .fallback(not_found_fallback)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::map_response(harden))
        .layer(middleware::from_fn_with_state(app.clone(), access_log))
        .with_state(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_parsing() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer_token(&h), None);
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer citro_abc"));
        assert_eq!(bearer_token(&h), Some("citro_abc"));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("bearer   citro_abc  "));
        assert_eq!(bearer_token(&h), Some("citro_abc"));
        for bad in ["citro_abc", "Basic abc", "Bearer ", "Bearer"] {
            h.insert(header::AUTHORIZATION, HeaderValue::from_str(bad).unwrap());
            assert_eq!(bearer_token(&h), None, "{bad}");
        }
    }

    #[test]
    fn client_ip_uses_the_proxy_header_only_when_configured() {
        let peer: SocketAddr = "10.0.0.9:5555".parse().unwrap();
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("6.6.6.6, 203.0.113.7"));
        assert_eq!(client_ip(None, peer, &h), peer.ip());
        assert_eq!(client_ip(Some("X-Forwarded-For"), peer, &h), "203.0.113.7".parse::<IpAddr>().unwrap());
        h.insert("x-forwarded-for", HeaderValue::from_static("not an ip"));
        assert_eq!(client_ip(Some("X-Forwarded-For"), peer, &h), peer.ip());
        assert_eq!(client_ip(Some("CF-Connecting-IP"), peer, &h), peer.ip());
    }

    #[test]
    fn flags_and_filenames() {
        assert!(!parse_flag(None, "bom").unwrap());
        assert!(!parse_flag(Some("0"), "bom").unwrap());
        assert!(parse_flag(Some("1"), "bom").unwrap());
        assert!(parse_flag(Some("TRUE"), "bom").unwrap());
        assert!(parse_flag(Some("maybe"), "bom").is_err());
        assert_eq!(csv_filename("Acme Shop / EU", 1_794_768_180), "acme-shop-eu-profiles-2026-11-15.csv");
        assert_eq!(csv_filename("***", 1_794_768_180), "team-profiles-2026-11-15.csv");
    }

    #[test]
    fn log_queries_are_validated() {
        let q = log_query(LogsQuery {
            level: Some("warn, error".into()),
            kind: Some("browser".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(q.levels, [LogLevel::Warn, LogLevel::Error]);
        assert_eq!(q.kind, Some(LogKind::Browser));
        assert_eq!(q.limit, DEFAULT_LOG_LIMIT);
        assert_eq!(
            log_query(LogsQuery { limit: Some(10_000_000), ..Default::default() }).unwrap().limit,
            MAX_LOG_LIMIT
        );
        assert!(log_query(LogsQuery { kind: Some("nope".into()), ..Default::default() }).is_err());
        assert!(log_query(LogsQuery { level: Some("loud".into()), ..Default::default() }).is_err());
    }

    fn meta(version: u64, sha: &str) -> DataMeta {
        DataMeta { version, size: 3, sha256: sha.into(), updated_at: 0 }
    }

    fn pre(headers: &[(&str, &str)], query: &str) -> Precondition {
        let mut h = HeaderMap::new();
        for (k, v) in headers {
            h.insert(HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap());
        }
        let q: DataQuery = serde_urlencoded::from_str(query).unwrap();
        Precondition::from_request(&h, &q)
    }

    #[test]
    fn entity_tags_are_normalised() {
        assert_eq!(etags("\"ABC\", W/\"def\" ,*"), ["abc", "def", "*"]);
        assert!(etags("  ").is_empty());
    }

    #[test]
    fn preconditions_decide_whether_a_push_may_replace_the_data() {
        let none = pre(&[], "");
        assert!(none.check(&meta(0, "")).is_ok() && none.check(&meta(4, "aa")).is_ok(), "no condition: always");

        let base = pre(&[], "base_version=4");
        assert!(base.check(&meta(4, "aa")).is_ok());
        let e = base.check(&meta(5, "bb")).unwrap_err();
        assert_eq!((e.status.as_u16(), e.code, e.current_version), (409, "stale", Some(5)));

        let m = pre(&[("if-match", "\"AA\"")], "");
        assert!(m.check(&meta(4, "aa")).is_ok(), "tags compare case-insensitively");
        assert_eq!(m.check(&meta(5, "bb")).unwrap_err().status.as_u16(), 412);
        assert_eq!(m.check(&meta(0, "")).unwrap_err().status.as_u16(), 412, "nothing stored: nothing to match");
        let star = pre(&[("if-match", "*")], "");
        assert!(star.check(&meta(1, "aa")).is_ok() && star.check(&meta(0, "")).is_err());

        let first = pre(&[("if-none-match", "*")], "");
        assert!(first.check(&meta(0, "")).is_ok());
        assert_eq!(first.check(&meta(1, "aa")).unwrap_err().status.as_u16(), 412);
        assert!(pre(&[("if-none-match", "\"aa\"")], "").check(&meta(1, "aa")).is_ok(), "only `*` counts for a push");
    }
}
