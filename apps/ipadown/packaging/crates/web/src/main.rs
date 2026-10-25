mod jobs;

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    Json, Router,
};
use base64::Engine;
use ipadown_store as store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncSeekExt;

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
/// 网站图标（与 lpk 应用图标同源）
const ICON_PNG: &[u8] = include_bytes!("../static/icon.png");

/// 历史版本目录与 App Store 更新日志缓存有效期。
const HISTORY_CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const INSTALL_TOKEN_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_HISTORY_CACHE_ENTRIES: usize = 64;
const MAX_INSTALL_TOKENS: usize = 128;
const ARTWORK_CACHE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_ARTWORK_CACHE_ENTRIES: usize = 256;
const DEFAULT_MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Default)]
struct VersionsCache {
    /// "country\0app_id\0bundle_id\0platform" -> 历史版本目录
    catalogs: HashMap<String, (Instant, Arc<store::history::VersionCatalog>)>,
}

impl VersionsCache {
    fn get(&mut self, key: &str) -> Option<Arc<store::history::VersionCatalog>> {
        self.catalogs
            .retain(|_, (created_at, _)| created_at.elapsed() < HISTORY_CACHE_TTL);
        self.catalogs.get(key).map(|(_, catalog)| catalog.clone())
    }

    fn insert(&mut self, key: String, catalog: Arc<store::history::VersionCatalog>) {
        self.catalogs
            .retain(|_, (created_at, _)| created_at.elapsed() < HISTORY_CACHE_TTL);
        if self.catalogs.len() >= MAX_HISTORY_CACHE_ENTRIES {
            let oldest = self
                .catalogs
                .iter()
                .min_by_key(|(_, (created_at, _))| *created_at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.catalogs.remove(&oldest);
            }
        }
        self.catalogs.insert(key, (Instant::now(), catalog));
    }
}

#[derive(Default)]
struct ArtworkCache {
    /// "country\0platform\0bundle_id" -> App Store 图标；None 也会缓存，避免反复查询已下架应用。
    entries: HashMap<String, (Instant, Option<String>)>,
}

impl ArtworkCache {
    fn get(&mut self, key: &str) -> Option<Option<String>> {
        self.entries
            .retain(|_, (created_at, _)| created_at.elapsed() < ARTWORK_CACHE_TTL);
        self.entries.get(key).map(|(_, url)| url.clone())
    }

    fn insert(&mut self, key: String, url: Option<String>) {
        self.entries
            .retain(|_, (created_at, _)| created_at.elapsed() < ARTWORK_CACHE_TTL);
        if self.entries.len() >= MAX_ARTWORK_CACHE_ENTRIES {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (created_at, _))| *created_at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key, (Instant::now(), url));
    }
}

#[derive(Clone)]
struct AppState {
    public_base_url: Option<String>,
    /// 是否信任反向代理传入的 X-Forwarded-* 头（自建反代时显式开启）。
    trust_proxy: bool,
    jobs: jobs::Registry,
    versions: Arc<Mutex<VersionsCache>>,
    artwork: Arc<Mutex<ArtworkCache>>,
    install_tokens: InstallTokens,
}

#[derive(Clone, Default)]
struct InstallTokens(Arc<Mutex<HashMap<String, InstallGrant>>>);

struct InstallGrant {
    file: String,
    expires_at: Instant,
}

impl InstallTokens {
    fn issue(&self, file: &str) -> String {
        let token = random_id();
        let mut grants = lock_ignore_poison(&self.0);
        let now = Instant::now();
        grants.retain(|_, grant| grant.expires_at > now);
        if grants.len() >= MAX_INSTALL_TOKENS {
            let earliest = grants
                .iter()
                .min_by_key(|(_, grant)| grant.expires_at)
                .map(|(token, _)| token.clone());
            if let Some(earliest) = earliest {
                grants.remove(&earliest);
            }
        }
        grants.insert(
            token.clone(),
            InstallGrant {
                file: file.to_string(),
                expires_at: now + INSTALL_TOKEN_TTL,
            },
        );
        token
    }

    fn allows(&self, token: &str, file: &str) -> bool {
        let mut grants = lock_ignore_poison(&self.0);
        grants.retain(|_, grant| grant.expires_at > Instant::now());
        grants.get(token).is_some_and(|grant| grant.file == file)
    }
}

fn random_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("无法从操作系统获取安全随机数");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 锁中毒（持锁线程 panic）时数据本身仍完整，取回继续而不是连环 panic。
fn lock_ignore_poison<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone)]
struct BasicAuth {
    username: String,
    password: String,
}

impl BasicAuth {
    fn from_env() -> Option<Self> {
        let password = std::env::var("IPADOWN_WEB_PASSWORD")
            .ok()
            .filter(|value| !value.is_empty())?;
        Some(Self {
            username: std::env::var("IPADOWN_WEB_USERNAME")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "ipadown".into()),
            password,
        })
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        diff |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    diff == 0
}

async fn require_basic_auth(
    State(auth): State<BasicAuth>,
    request: Request,
    next: Next,
) -> Response {
    let valid = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
        })
        .and_then(|decoded| String::from_utf8(decoded).ok())
        .and_then(|decoded| {
            let (username, password) = decoded.split_once(':')?;
            Some(
                constant_time_eq(username.as_bytes(), auth.username.as_bytes())
                    && constant_time_eq(password.as_bytes(), auth.password.as_bytes()),
            )
        })
        .unwrap_or(false);

    if valid {
        next.run(request).await
    } else {
        Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(
                header::WWW_AUTHENTICATE,
                "Basic realm=\"ipadown\", charset=\"UTF-8\"",
            )
            .body(Body::from("需要访问密码"))
            .unwrap()
    }
}

fn apply_security_headers(response: &mut Response, path: &str) {
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    // 前端已无内联事件处理器与内联脚本，可启用严格 CSP（内联 style 属性仍需要 unsafe-inline）。
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' https: data:; connect-src 'self'; object-src 'none'; \
             base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
        ),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    // 缓存策略：OTA 走 ETag/Range 条件请求；静态页面可复验；API 响应不落盘。
    let cache_policy = if path.starts_with("/ota/") {
        "private, max-age=0, must-revalidate"
    } else if path == "/" || path == "/app.js" || path == "/icon.png" {
        "no-cache"
    } else {
        "no-store"
    };
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_policy),
    );
}

async fn security_headers(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let mut response = next.run(request).await;
    apply_security_headers(&mut response, &path);
    response
}

/// HTTP 访问日志：方法、路径、状态码、耗时。
/// 前端轮询端点（任务/文件/健康检查）只在出错时才记录，避免日志噪声。
async fn http_access_log(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    let noisy_poll = method == axum::http::Method::GET
        && (path == "/api/downloads" || path == "/api/files" || path == "/healthz");
    if !noisy_poll || status >= 400 {
        store::log_info!(
            "http",
            "{method} {path} -> {status} dur={}ms",
            started.elapsed().as_millis()
        );
    }
    response
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            store::log_error!("shutdown", "cannot listen for ctrl-c: {error}");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => store::log_error!("shutdown", "cannot listen for SIGTERM: {error}"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    store::log_info!("shutdown", "signal received, stopping");
}

async fn wait_for_shutdown(mut receiver: tokio::sync::watch::Receiver<bool>) {
    if *receiver.borrow() {
        return;
    }
    while receiver.changed().await.is_ok() {
        if *receiver.borrow() {
            return;
        }
    }
}

fn arm_tls_shutdown(
    handle: axum_server::Handle<SocketAddr>,
    receiver: tokio::sync::watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        wait_for_shutdown(receiver).await;
        handle.graceful_shutdown(Some(Duration::from_secs(10)));
    });
}

#[tokio::main]
async fn main() {
    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_sender.send(true);
    });

    // 启动时固化配置（确保 keychain_passphrase 等字段落盘，避免每次启动重新生成）
    match store::config::Config::load_or_create() {
        Ok(mut cfg) => {
            store::ipatool::migrate_legacy(&mut cfg);
            store::log_info!("startup", "config ready accounts={}", cfg.accounts.len());
        }
        Err(e) => store::log_error!("startup", "config init failed error={e}"),
    }
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let public_base_url = std::env::var("PUBLIC_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim_end_matches('/').to_string());
    let trust_proxy = std::env::var("IPADOWN_TRUST_PROXY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let state = AppState {
        public_base_url: public_base_url.clone(),
        trust_proxy,
        jobs: jobs::new_registry(),
        versions: Arc::new(Mutex::new(VersionsCache::default())),
        artwork: Arc::new(Mutex::new(ArtworkCache::default())),
        install_tokens: InstallTokens::default(),
    };

    let protected = Router::new()
        .route("/", axum::routing::get(|| async { Html(INDEX_HTML) }))
        .route(
            "/app.js",
            axum::routing::get(|| async {
                (
                    [(
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    APP_JS,
                )
            }),
        )
        .route(
            "/icon.png",
            axum::routing::get(|| async { ([(header::CONTENT_TYPE, "image/png")], ICON_PNG) }),
        )
        .route("/api/config", axum::routing::get(api_config))
        .route("/api/auth/login", axum::routing::post(api_login))
        .route("/api/auth/logout", axum::routing::post(api_logout))
        .route("/api/auth/accounts", axum::routing::get(api_accounts))
        .route("/api/auth/refresh", axum::routing::post(api_auth_refresh))
        .route("/api/auth/select", axum::routing::post(api_auth_select))
        .route("/api/search", axum::routing::get(api_search))
        .route("/api/versions", axum::routing::get(api_versions))
        .route(
            "/api/downloads",
            axum::routing::get(api_downloads).post(api_download_start),
        )
        .route(
            "/api/downloads/{id}/pause",
            axum::routing::post(api_download_pause),
        )
        .route(
            "/api/downloads/{id}/resume",
            axum::routing::post(api_download_resume),
        )
        .route(
            "/api/downloads/{id}",
            axum::routing::delete(api_download_delete),
        )
        .route("/api/files", axum::routing::get(api_files))
        .route("/api/files/{file}", axum::routing::delete(api_file_delete))
        .route("/api/install/{file}", axum::routing::get(api_install));
    let basic_auth = BasicAuth::from_env();
    let protected = match basic_auth {
        Some(auth) => protected.layer(middleware::from_fn_with_state(auth, require_basic_auth)),
        None => {
            if std::env::var_os("IPADOWN_PLATFORM_AUTH").is_none() {
                store::log_warn!(
                    "startup",
                    "IPADOWN_WEB_PASSWORD not set; web/api should only be exposed on trusted networks"
                );
            }
            protected
        }
    };
    let ota_routes = Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .route(
            "/ota/{file}/manifest.plist",
            axum::routing::get(ota_manifest),
        )
        .route("/ota/{file}/app.ipa", axum::routing::get(ota_ipa));
    let app = ota_routes
        .merge(protected)
        .layer(middleware::from_fn(http_access_log))
        .layer(middleware::from_fn(security_headers))
        .with_state(state.clone());

    // 双栈监听：优先 [::]（Linux 下同时接受 IPv4/IPv6，兼容 lazycat ingress 走 ::1 的回源）
    let addr_v6 = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 0], port));
    let addr_v4 = SocketAddr::from(([0, 0, 0, 0], port));
    let tls_cert = std::env::var("TLS_CERT").ok();
    let tls_key = std::env::var("TLS_KEY").ok();

    store::log_info!(
        "startup",
        "ipadown-web starting port={port} tls={} public_base_url={} trust_proxy={trust_proxy}",
        tls_cert.is_some(),
        public_base_url.as_deref().unwrap_or("<unset>")
    );

    match (tls_cert, tls_key) {
        (Some(cert), Some(key)) => {
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
                .await
                .expect("TLS 证书加载失败");
            store::log_info!("startup", "listening on https://[::]:{port}");
            let handle = axum_server::Handle::new();
            arm_tls_shutdown(handle.clone(), shutdown_receiver);
            if let Err(error) = axum_server::bind_rustls(addr_v6, tls)
                .handle(handle)
                .serve(app.into_make_service())
                .await
            {
                store::log_error!("startup", "https server exited: {error}");
            }
        }
        _ => {
            if state.public_base_url.is_none() {
                store::log_warn!(
                    "startup",
                    "PUBLIC_BASE_URL not set; browser install requires a trusted HTTPS base URL"
                );
            }
            let listener = match tokio::net::TcpListener::bind(addr_v6).await {
                Ok(l) => l,
                Err(_) => tokio::net::TcpListener::bind(addr_v4)
                    .await
                    .expect("端口绑定失败"),
            };
            store::log_info!("startup", "listening on http://[::]:{port}");
            if let Err(error) = axum::serve(listener, app)
                .with_graceful_shutdown(wait_for_shutdown(shutdown_receiver))
                .await
            {
                store::log_error!("startup", "http server exited: {error}");
            }
        }
    }
}

// ---------- 工具 ----------

fn err_json(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// 已知错误文案翻译成可操作的中文提示
pub(crate) fn friendly_msg(raw: &str) -> String {
    let l = raw.to_lowercase();
    if l.contains("invalid 2fa") || raw.contains("双重验证码无效") || raw.contains("双重验证码错误")
    {
        "双重验证码无效或已过期，请重新输入".into()
    } else if l.contains("2fa") || raw.contains("双重验证码") {
        "需要双重验证码，请完成验证后继续下载".into()
    } else if l.contains("license is required") {
        "账号未拥有该 App（免费 App 需先在手机 App Store「获取」一次，付费 App 需先购买）；若确认已拥有，则可能是登录会话过期，请重新登录".into()
    } else if l.contains("password token is expired") || l.contains("sign in required") {
        "登录会话已过期，请重新登录".into()
    } else if l.contains("http 403") || l.contains("non-plist body") {
        "苹果拒绝了下载请求（403）。若持续出现，请更新镜像中的 ipatool 版本，或更换 Apple ID 重试"
            .into()
    } else if raw.contains("未找到登录账号") {
        "未登录或登录已失效，请登录 Apple ID".into()
    } else if l.contains("connection reset")
        || l.contains("request failed")
        || l.contains("timed out")
        || l.contains("i/o timeout")
    {
        "访问苹果服务器失败（连接被重置/超时，已自动重试）。国内网络建议在应用设置中配置 HTTPS 代理"
            .into()
    } else {
        raw.to_string()
    }
}

/// 是否属于登录态问题（前端据此提示重新登录）
fn is_auth_problem(raw: &str) -> bool {
    let l = raw.to_lowercase();
    l.contains("password token is expired")
        || l.contains("sign in required")
        || l.contains("license is required")
}

fn store_err(e: store::StoreError) -> Response {
    let code_required = jobs::needs_two_factor(&e);
    let status = match &e {
        store::StoreError::NoAccount => StatusCode::UNAUTHORIZED,
        store::StoreError::CodeRequired => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_GATEWAY,
    };
    let auth_expired = matches!(&e, store::StoreError::NoAccount)
        || code_required
        || is_auth_problem(&e.to_string());
    (
        status,
        Json(serde_json::json!({
            "error": friendly_msg(&e.to_string()),
            "auth_expired": auth_expired,
            "code_required": code_required,
        })),
    )
        .into_response()
}

#[allow(clippy::result_large_err)] // 保留完整 HTTP 响应，让调用点可直接 return
fn sanitize_file(name: &str) -> std::result::Result<PathBuf, Response> {
    if name.contains('/') || name.contains("..") || !name.ends_with(".ipa") {
        return Err(err_json(StatusCode::BAD_REQUEST, "非法文件名"));
    }
    let path = match store::config::ipas_dir() {
        Ok(dir) => dir.join(name),
        Err(e) => return Err(err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    if !path.exists() {
        return Err(err_json(StatusCode::NOT_FOUND, "文件不存在"));
    }
    Ok(path)
}

#[allow(clippy::result_large_err)]
fn ensure_download_space() -> std::result::Result<(), Response> {
    let dir = store::config::ipas_dir()
        .map_err(|error| err_json(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let available = fs2::available_space(&dir)
        .map_err(|error| err_json(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let required = std::env::var("IPADOWN_MIN_FREE_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_MIN_FREE_BYTES);
    if available < required {
        return Err(err_json(
            StatusCode::INSUFFICIENT_STORAGE,
            format!(
                "存储空间不足：至少需要保留 {:.1} GB 可用空间",
                required as f64 / 1024_f64.powi(3)
            ),
        ));
    }
    Ok(())
}

/// HTTP/1.1 走 host 头，HTTP/2 的 :authority 由 hyper 写入 URI
fn req_host<'a>(headers: &'a HeaderMap, uri: &'a axum::http::Uri) -> &'a str {
    if let Some(h) = headers.get("host").and_then(|v| v.to_str().ok()) {
        return h;
    }
    uri.authority().map(|a| a.as_str()).unwrap_or("localhost")
}

/// 决定对外 base URL：优先 PUBLIC_BASE_URL；自建反代可显式开启
/// IPADOWN_TRUST_PROXY=1 后才信任 X-Forwarded-* 头，避免伪造头注入安装链接。
fn base_url(state: &AppState, headers: &HeaderMap, uri: &axum::http::Uri) -> String {
    if let Some(u) = &state.public_base_url {
        return u.clone();
    }
    if state.trust_proxy {
        if let Some(host) = headers
            .get("x-forwarded-host")
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty())
        {
            let proto = headers
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.is_empty())
                .unwrap_or("http");
            return format!("{proto}://{host}");
        }
    }
    format!("http://{}", req_host(headers, uri))
}

// ---------- API ----------

async fn api_config(State(s): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "public_base_url": s.public_base_url,
    }))
}

#[derive(Deserialize)]
struct LoginReq {
    email: String,
    password: String,
    code: Option<String>,
}

async fn api_login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> Response {
    let login_email = req.email.trim().to_lowercase();
    if login_email.is_empty() || req.password.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "缺少 Apple ID 或密码");
    }
    let cfg = match store::config::Config::load_or_create() {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    let ipatool = match store::ipatool::Ipatool::for_account(&cfg, &login_email) {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    match ipatool
        .login(&login_email, &req.password, req.code.as_deref())
        .await
    {
        Ok(session) => {
            // 从钥匙串 JWE 解出姓名与国家，登记到多账号配置并设为当前账号。
            // 登录可能耗时数分钟，在 config 锁内重新加载，避免覆盖期间新增的其他账号。
            let meta = store::ipatool::read_account_meta(&cfg, &login_email)
                .map(|mut meta| {
                    meta.email = login_email.clone();
                    meta
                })
                .unwrap_or_else(|| store::config::AccountMeta {
                    email: login_email.clone(),
                    country: None,
                    name: None,
                });
            if let Err(error) = store::config::update(|latest| {
                latest.upsert_account(meta);
                latest.current_account = Some(login_email.clone());
                Ok(())
            }) {
                return store_err(error);
            }
            let cleared_jobs = jobs::clear_two_factor_failures(&state.jobs, &login_email);
            store::log_info!(
                "auth",
                "login ok email={} cleared_jobs={cleared_jobs}",
                session.email
            );
            Json(serde_json::json!({
                "ok": true,
                "email": session.email,
                "cleared_jobs": cleared_jobs,
            }))
            .into_response()
        }
        Err(store::StoreError::CodeRequired) => {
            store::log_warn!("auth", "login 2fa required email={login_email}");
            (
                StatusCode::OK,
                Json(serde_json::json!({ "ok": false, "code_required": true })),
            )
                .into_response()
        }
        Err(e) => {
            store::log_warn!("auth", "login failed email={login_email} error={e}");
            store_err(e)
        }
    }
}

#[derive(Deserialize)]
struct LogoutReq {
    #[serde(default)]
    email: String,
}

async fn api_logout(Json(req): Json<LogoutReq>) -> Response {
    let cfg = match store::config::Config::load_or_create() {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    let email = req.email.trim().to_string();
    if email.is_empty() {
        return err_json(StatusCode::BAD_REQUEST, "缺少 email");
    }
    let ipatool = match store::ipatool::Ipatool::for_account(&cfg, &email) {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    let _ = ipatool.logout().await; // 吊销失败也继续移除本地记录
    if let Err(e) = store::config::update(|latest| {
        latest.remove_account(&email);
        Ok(())
    }) {
        return store_err(e);
    }
    store::log_info!("auth", "logout ok email={email}");
    Json(serde_json::json!({ "ok": true })).into_response()
}

#[derive(Serialize)]
struct AccountInfo {
    email: String,
    country: Option<String>,
    name: Option<String>,
    current: bool,
}

/// 主动刷新/验证登录状态（真实请求苹果商店，而不仅是读本地钥匙串）
async fn api_auth_refresh() -> Response {
    let cfg = match store::config::Config::load_or_create() {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    let ipatool = match store::ipatool::Ipatool::new(&cfg) {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    match ipatool.validate_session().await {
        Ok(session) => {
            Json(serde_json::json!({ "ok": true, "email": session.email })).into_response()
        }
        Err(e) => store_err(e),
    }
}

async fn api_accounts() -> Response {
    let cfg = match store::config::Config::load_or_create() {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    let current = cfg.current_meta().map(|m| m.email.clone());
    let list: Vec<AccountInfo> = cfg
        .accounts
        .iter()
        .map(|a| AccountInfo {
            email: a.email.clone(),
            country: a.country.clone(),
            name: a.name.clone(),
            current: Some(&a.email) == current.as_ref(),
        })
        .collect();
    Json(list).into_response()
}

/// 切换当前账号
#[derive(Deserialize)]
struct SelectReq {
    email: String,
}

async fn api_auth_select(Json(req): Json<SelectReq>) -> Response {
    const NO_SUCH_ACCOUNT: &str = "账号不存在";
    let result = store::config::update(|cfg| {
        if !cfg.accounts.iter().any(|a| a.email == req.email) {
            return Err(store::StoreError::Config(NO_SUCH_ACCOUNT.into()));
        }
        cfg.current_account = Some(req.email.clone());
        Ok(())
    });
    match result {
        Ok(()) => {
            store::log_info!("auth", "switch account email={}", req.email);
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(store::StoreError::Config(message)) if message == NO_SUCH_ACCOUNT => {
            err_json(StatusCode::NOT_FOUND, NO_SUCH_ACCOUNT)
        }
        Err(e) => store_err(e),
    }
}

#[derive(Deserialize)]
struct SearchQuery {
    term: String,
    country: Option<String>,
    limit: Option<u32>,
    #[serde(default)]
    platform: store::search::DevicePlatform,
}

async fn api_search(Query(q): Query<SearchQuery>) -> Response {
    let term = q.term.trim();
    if term.is_empty() || term.chars().count() > 200 {
        return err_json(StatusCode::BAD_REQUEST, "搜索词长度必须在 1–200 个字符之间");
    }
    let country = q.country.unwrap_or_else(|| "CN".into()).to_uppercase();
    if country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return err_json(StatusCode::BAD_REQUEST, "非法国家/地区代码");
    }
    match store::search::search(
        term,
        &country,
        q.limit.unwrap_or(15).clamp(1, 50),
        q.platform,
    )
    .await
    {
        Ok(results) => Json(results).into_response(),
        Err(e) => {
            store::log_warn!(
                "search",
                "search failed term={term} country={country} error={e}"
            );
            store_err(e)
        }
    }
}

#[derive(Deserialize)]
struct VersionsQuery {
    bundle_id: String,
    app_id: i64,
    country: String,
    #[serde(default)]
    platform: store::search::DevicePlatform,
}

impl VersionsQuery {
    #[allow(clippy::result_large_err)]
    fn validate(&self) -> std::result::Result<(), Response> {
        jobs::download_filename(&self.bundle_id, None, self.platform)
            .map_err(|message| err_json(StatusCode::BAD_REQUEST, message))?;
        if self.app_id <= 0 {
            return Err(err_json(StatusCode::BAD_REQUEST, "非法 App Store ID"));
        }
        if self.country.len() != 2 || !self.country.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return Err(err_json(StatusCode::BAD_REQUEST, "非法国家/地区代码"));
        }
        Ok(())
    }

    fn cache_key(&self) -> String {
        // 历史版本目录与设备类型无关，不同平台复用同一份缓存
        format!(
            "{}\0{}\0{}",
            self.country.to_ascii_uppercase(),
            self.app_id,
            self.bundle_id
        )
    }
}

/// 爱果提供历史版本 ID，App Store 提供官方更新日志；ipatool 仅用于最终下载。
#[allow(clippy::result_large_err)]
async fn get_version_catalog(
    state: &AppState,
    query: &VersionsQuery,
) -> std::result::Result<Arc<store::history::VersionCatalog>, Response> {
    query.validate()?;
    let key = query.cache_key();
    if let Some(catalog) = lock_ignore_poison(&state.versions).get(&key) {
        return Ok(catalog);
    }

    let catalog = store::history::versions(query.app_id, &query.country)
        .await
        .map_err(store_err)?;
    if catalog.bundle_id != query.bundle_id {
        return Err(store_err(store::StoreError::UnexpectedResponse(format!(
            "爱果返回的 Bundle ID 不匹配：期望 {}，实际 {}",
            query.bundle_id, catalog.bundle_id
        ))));
    }
    let catalog = Arc::new(catalog);
    if catalog
        .versions
        .iter()
        .any(|version| version.release_notes.is_some())
    {
        lock_ignore_poison(&state.versions).insert(key, catalog.clone());
    }
    Ok(catalog)
}

async fn api_versions(State(s): State<AppState>, Query(q): Query<VersionsQuery>) -> Response {
    match get_version_catalog(&s, &q).await {
        Ok(catalog) => Json(serde_json::json!({
            "app": {
                "name": catalog.name,
                "bundle_id": catalog.bundle_id,
                "version": catalog.current_version,
            },
            "versions": catalog.versions.iter().map(|item| serde_json::json!({
                "version_id": item.version_id,
                "display_version": item.version,
                "release_date": item.release_date,
                "size": item.size,
                "release_notes": item.release_notes,
            })).collect::<Vec<_>>(),
            "source": "app.agzy.cn",
            "release_notes_source": "apps.apple.com",
            "updated_at": catalog.updated_at,
        }))
        .into_response(),
        Err(e) => e,
    }
}

async fn api_downloads(State(s): State<AppState>) -> impl IntoResponse {
    Json(jobs::list(&s.jobs))
}

/// 暂停进行中的下载（中止 ipatool 子进程，保留断点）
async fn api_download_pause(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    match jobs::pause_job(&s.jobs, &id) {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(message) => err_json(StatusCode::BAD_REQUEST, message),
    }
}

/// 继续已暂停的下载（ipatool 从 .tmp 断点续传）
async fn api_download_resume(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(account) = jobs::job_account(&s.jobs, &id) else {
        return err_json(StatusCode::NOT_FOUND, "任务不存在");
    };
    let cfg = match store::config::Config::load_or_create() {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    let ipatool = match store::ipatool::Ipatool::for_account(&cfg, &account) {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    match jobs::resume_job(&s.jobs, &id, ipatool) {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(message) => err_json(StatusCode::BAD_REQUEST, message),
    }
}

/// 删除任务记录（进行中的任务先中止）
async fn api_download_delete(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    if jobs::remove_job(&s.jobs, &id) {
        Json(serde_json::json!({ "ok": true })).into_response()
    } else {
        err_json(StatusCode::NOT_FOUND, "任务不存在")
    }
}

#[derive(Deserialize)]
struct DownloadReq {
    bundle_id: String,
    version_id: Option<String>,
    #[serde(default)]
    platform: store::search::DevicePlatform,
    /// 预估安装包大小（字节，来自历史版本目录的体积字符串），仅用于进度条展示
    #[serde(default)]
    size: Option<u64>,
}

const MAX_EXPECTED_IPA_BYTES: u64 = 64 * 1024 * 1024 * 1024;

async fn api_download_start(State(s): State<AppState>, Json(req): Json<DownloadReq>) -> Response {
    if let Err(message) =
        jobs::download_filename(&req.bundle_id, req.version_id.as_deref(), req.platform)
    {
        return err_json(StatusCode::BAD_REQUEST, message);
    }
    if let Err(response) = ensure_download_space() {
        return response;
    }
    let cfg = match store::config::Config::load_or_create() {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    let ipatool = match store::ipatool::Ipatool::new(&cfg) {
        Ok(c) => c,
        Err(e) => return store_err(e),
    };
    let account = cfg
        .current_meta()
        .map(|meta| meta.email.clone())
        .unwrap_or_default();
    let total = req
        .size
        .filter(|size| *size > 0 && *size <= MAX_EXPECTED_IPA_BYTES);
    // 会话有效性在任务内校验：ipatool 子进程可能耗时数分钟，
    // 不阻塞本请求；失效或 2FA 时任务失败，前端轮询到后弹出对应处理。
    store::log_info!(
        "download",
        "requested bundle={} version={} platform={} account={account} total={}",
        req.bundle_id,
        req.version_id.as_deref().unwrap_or("latest"),
        req.platform.ipatool_value(),
        total.unwrap_or(0)
    );
    match jobs::spawn_download(
        s.jobs.clone(),
        req.bundle_id,
        req.version_id,
        req.platform,
        account,
        total,
        ipatool,
    ) {
        Ok(reused) => Json(serde_json::json!({
            "reused": reused,
        }))
        .into_response(),
        Err(message) => err_json(
            if message.starts_with("下载队列已满") {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::BAD_REQUEST
            },
            message,
        ),
    }
}

#[derive(Serialize)]
struct FileInfo {
    name: String,
    size: u64,
    bundle_id: Option<String>,
    version: Option<String>,
    title: Option<String>,
    platform: store::search::DevicePlatform,
    artwork_url: Option<String>,
}

fn downloaded_file_platform(name: &str) -> store::search::DevicePlatform {
    let stem = name.strip_suffix(".ipa").unwrap_or(name);
    if stem.ends_with("_appletv") {
        store::search::DevicePlatform::Appletv
    } else if stem.ends_with("_ipad") {
        store::search::DevicePlatform::Ipad
    } else {
        store::search::DevicePlatform::Iphone
    }
}

async fn artwork_for_file(
    state: &AppState,
    bundle_id: &str,
    country: &str,
    platform: store::search::DevicePlatform,
) -> Option<String> {
    let key = format!("{country}\0{}\0{bundle_id}", platform.ipatool_value());
    if let Some(cached) = lock_ignore_poison(&state.artwork).get(&key) {
        return cached;
    }

    let mut countries = vec![country];
    if country != "CN" {
        countries.push("CN");
    }
    if country != "US" {
        countries.push("US");
    }
    let mut artwork = None;
    for candidate in countries {
        if let Ok(app) = store::search::lookup(bundle_id, candidate, platform).await {
            artwork = app
                .artwork_url
                .map(|url| url.replace("512x512bb", "100x100bb"));
            if artwork.is_some() {
                break;
            }
        }
    }
    lock_ignore_poison(&state.artwork).insert(key, artwork.clone());
    artwork
}
async fn api_files(State(state): State<AppState>) -> Response {
    let dir = match store::config::ipas_dir() {
        Ok(d) => d,
        Err(e) => return store_err(e),
    };
    let mut files = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".ipa") {
                continue;
            }
            let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            let path = entry.path();
            let meta = tokio::task::spawn_blocking(move || store::ipa::read_metadata(&path))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
            files.push(FileInfo {
                platform: downloaded_file_platform(&name),
                name,
                size,
                bundle_id: meta.bundle_id,
                version: meta.bundle_version,
                title: meta.display_name,
                artwork_url: None,
            });
        }
    }
    files.sort_by(|a, b| b.name.cmp(&a.name));
    let country = store::config::Config::load_or_create()
        .ok()
        .and_then(|config| config.current_meta().and_then(|meta| meta.country.clone()))
        .map(|country| country.to_uppercase())
        .unwrap_or_else(|| "CN".into());
    let mut groups: HashMap<String, (String, store::search::DevicePlatform, Vec<usize>)> =
        HashMap::new();
    for (index, file) in files.iter().enumerate() {
        if let Some(bundle_id) = file.bundle_id.as_deref() {
            let key = format!("{country}\0{}\0{bundle_id}", file.platform.ipatool_value());
            groups
                .entry(key)
                .or_insert_with(|| (bundle_id.to_string(), file.platform, Vec::new()))
                .2
                .push(index);
        }
    }
    let limiter = Arc::new(tokio::sync::Semaphore::new(6));
    let mut lookups = tokio::task::JoinSet::new();
    for (_, (bundle_id, platform, indices)) in groups {
        let state = state.clone();
        let country = country.clone();
        let limiter = limiter.clone();
        lookups.spawn(async move {
            let _permit = limiter.acquire_owned().await.ok();
            let artwork = artwork_for_file(&state, &bundle_id, &country, platform).await;
            (indices, artwork)
        });
    }
    while let Some(Ok((indices, artwork))) = lookups.join_next().await {
        for index in indices {
            files[index].artwork_url = artwork.clone();
        }
    }
    Json(files).into_response()
}

async fn api_file_delete(State(s): State<AppState>, Path(file): Path<String>) -> Response {
    let path = match sanitize_file(&file) {
        Ok(path) => path,
        Err(response) => return response,
    };
    if jobs::is_file_active(&s.jobs, &file) {
        return err_json(StatusCode::CONFLICT, "该文件正在下载，无法删除");
    }
    match tokio::fs::remove_file(path).await {
        Ok(()) => {
            store::log_info!("files", "deleted name={file}");
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(error) => {
            store::log_error!("files", "delete failed name={file} error={error}");
            err_json(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
        }
    }
}

struct OtaEntry {
    base: String,
    bundle_id: String,
    bundle_version: String,
    title: String,
}

#[allow(clippy::result_large_err)]
fn ota_entry_for(
    state: &AppState,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
    file: &str,
) -> std::result::Result<OtaEntry, Response> {
    let path = sanitize_file(file)?;
    let meta = store::ipa::read_metadata(&path).unwrap_or_default();
    Ok(OtaEntry {
        base: base_url(state, headers, uri),
        bundle_id: meta.bundle_id.unwrap_or_else(|| "unknown".into()),
        bundle_version: meta.bundle_version.unwrap_or_else(|| "1.0".into()),
        title: meta.display_name.unwrap_or_else(|| file.to_string()),
    })
}

fn install_link(base: &str, file: &str, token: &str) -> serde_json::Value {
    let file_segment = ipadown_ota::url_encode(file);
    let manifest_url = format!("{base}/ota/{file_segment}/manifest.plist?token={token}");
    let itms_url = format!(
        "itms-services://?action=download-manifest&url={}",
        ipadown_ota::url_encode(&manifest_url)
    );
    serde_json::json!({
        "itms_url": itms_url,
        "https": base.starts_with("https://"),
    })
}

fn is_safari_user_agent(headers: &HeaderMap) -> bool {
    let Some(user_agent) = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let has_safari_engine = user_agent.contains("AppleWebKit/")
        && user_agent.contains("Safari/")
        && user_agent.contains("Version/");
    let is_other_browser = [
        "CriOS/",
        "Chrome/",
        "FxiOS/",
        "Firefox/",
        "EdgiOS/",
        "Edg/",
        "OPiOS/",
        "OPR/",
        "DuckDuckGo/",
        "GSA/",
        "YaBrowser/",
        "SamsungBrowser/",
    ]
    .iter()
    .any(|marker| user_agent.contains(marker));
    has_safari_engine && !is_other_browser
}

async fn api_install(
    State(s): State<AppState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    Path(file): Path<String>,
) -> Response {
    if !is_safari_user_agent(&headers) {
        return err_json(
            StatusCode::BAD_REQUEST,
            "仅支持使用 Safari 浏览器打开并安装",
        );
    }
    if let Err(response) = sanitize_file(&file) {
        return response;
    }
    let token = s.install_tokens.issue(&file);
    store::log_info!("ota", "install link issued file={file}");
    let install = install_link(&base_url(&s, &headers, &uri), &file, &token);
    Json(serde_json::json!({
        "install": install,
    }))
    .into_response()
}

async fn ota_manifest(
    State(s): State<AppState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    Path(file): Path<String>,
    Query(access): Query<OtaAccessQuery>,
) -> Response {
    if !ota_access_allowed(&s, &file, access.token.as_deref()) {
        return err_json(StatusCode::FORBIDDEN, "安装链接无效或已过期，请重新生成");
    }
    let entry = match ota_entry_for(&s, &headers, &uri, &file) {
        Ok(v) => v,
        Err(r) => return r,
    };
    store::log_info!("ota", "manifest served file={file}");
    let token = access.token.unwrap_or_default();
    let file_segment = ipadown_ota::url_encode(&file);
    let ipa_url = format!("{}/ota/{file_segment}/app.ipa?token={token}", entry.base);
    let xml = ipadown_ota::manifest_plist(
        &ipa_url,
        &entry.bundle_id,
        &entry.bundle_version,
        &entry.title,
    );
    ([("Content-Type", "application/xml")], xml).into_response()
}

#[derive(Deserialize)]
struct OtaAccessQuery {
    token: Option<String>,
}

fn ota_access_allowed(state: &AppState, file: &str, token: Option<&str>) -> bool {
    token.is_some_and(|token| state.install_tokens.allows(token, file))
}

async fn ota_ipa(
    State(s): State<AppState>,
    Path(file): Path<String>,
    Query(access): Query<OtaAccessQuery>,
    headers: HeaderMap,
) -> Response {
    if !ota_access_allowed(&s, &file, access.token.as_deref()) {
        return err_json(StatusCode::FORBIDDEN, "安装链接无效或已过期，请重新生成");
    }
    let path = match sanitize_file(&file) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let (response, range) = serve_ipa(&path, &headers).await;
    match range {
        Some((start, end)) => {
            store::log_info!(
                "ota",
                "ipa served file={file} status=206 range={start}-{end}"
            )
        }
        None => store::log_info!(
            "ota",
            "ipa served file={file} status={}",
            response.status().as_u16()
        ),
    }
    response
}

/// 提供已下载 IPA：支持 ETag 条件请求（304）与单段 Range（断点续传），
/// 避免 iOS 安装中断后整包重下。
async fn serve_ipa(path: &std::path::Path, headers: &HeaderMap) -> (Response, Option<(u64, u64)>) {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => return (err_json(StatusCode::NOT_FOUND, e.to_string()), None),
    };
    let metadata = match file.metadata().await {
        Ok(m) => m,
        Err(e) => {
            return (
                err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
                None,
            )
        }
    };
    let len = metadata.len();
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let etag = format!("\"{len:x}-{mtime:x}\"");
    let last_modified = store::logging::httpdate_from_unix(mtime);

    if let Some(if_none_match) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        let matched = if_none_match.split(',').any(|candidate| {
            let candidate = candidate.trim();
            candidate == etag || candidate == "*"
        });
        if matched {
            let response = Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header(header::ETAG, etag)
                .header(header::LAST_MODIFIED, last_modified)
                .body(Body::empty())
                .unwrap();
            return (response, None);
        }
    }

    let served_range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| match parse_range(raw, len) {
            Ok(range) => range,
            Err(()) => Some((len, len)), // 不可满足：走 416 分支
        });
    let Some((start, end)) = served_range else {
        // 整包 200（含空文件）
        let response = Response::builder()
            .header("Content-Type", "application/octet-stream")
            .header(header::CONTENT_LENGTH, len.to_string())
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::ETAG, etag)
            .header(header::LAST_MODIFIED, last_modified)
            .body(Body::from_stream(tokio_util::io::ReaderStream::new(file)))
            .unwrap();
        return (response, None);
    };
    if start >= len {
        // 416：告知客户端总长
        let response = Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{len}"))
            .body(Body::empty())
            .unwrap();
        return (response, None);
    }
    let length = end - start + 1;
    if start > 0 {
        if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
            return (
                err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
                None,
            );
        }
    }
    let response = Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header("Content-Type", "application/octet-stream")
        .header(header::CONTENT_LENGTH, length.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, etag)
        .header(header::LAST_MODIFIED, last_modified)
        .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
        .body(Body::from_stream(tokio_util::io::ReaderStream::new(file)))
        .unwrap();
    (response, Some((start, end)))
}

/// 解析单段 `bytes=` Range 头。
/// - `Ok(Some((start, end)))`：合法区间
/// - `Ok(None)`：格式不认识或多段请求，回退整包 200
/// - `Err(())`：语法合法但不可满足（416）
fn parse_range(raw: &str, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = raw.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') {
        return Ok(None);
    }
    let Some((start_spec, end_spec)) = spec.split_once('-') else {
        return Ok(None);
    };
    if start_spec.is_empty() {
        // bytes=-N：最后 N 字节
        let Ok(suffix) = end_spec.parse::<u64>() else {
            return Ok(None);
        };
        if suffix == 0 || len == 0 {
            return Err(());
        }
        Ok(Some((len.saturating_sub(suffix), len - 1)))
    } else {
        let Ok(start) = start_spec.parse::<u64>() else {
            return Ok(None);
        };
        if start >= len {
            return Err(());
        }
        let end = if end_spec.is_empty() {
            len - 1
        } else {
            let Ok(end) = end_spec.parse::<u64>() else {
                return Ok(None);
            };
            end.min(len - 1)
        };
        if start > end {
            return Err(());
        }
        Ok(Some((start, end)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_tokens_are_unpredictable_and_bound_to_one_file() {
        let tokens = InstallTokens::default();
        let first = tokens.issue("com.example_1.ipa");
        let second = tokens.issue("com.example_1.ipa");

        assert_ne!(first, second);
        assert!(tokens.allows(&first, "com.example_1.ipa"));
        assert!(!tokens.allows(&first, "com.example_2.ipa"));
        assert!(!tokens.allows("missing", "com.example_1.ipa"));
    }

    #[test]
    fn install_token_store_is_bounded() {
        let tokens = InstallTokens::default();
        for index in 0..(MAX_INSTALL_TOKENS + 10) {
            tokens.issue(&format!("com.example_{index}.ipa"));
        }
        assert_eq!(lock_ignore_poison(&tokens.0).len(), MAX_INSTALL_TOKENS);
    }

    #[test]
    fn version_catalog_cache_is_bounded() {
        let mut cache = VersionsCache::default();
        for index in 0..(MAX_HISTORY_CACHE_ENTRIES + 10) {
            cache.insert(
                index.to_string(),
                Arc::new(store::history::VersionCatalog {
                    name: "App".into(),
                    bundle_id: "com.example.app".into(),
                    current_version: "1.0".into(),
                    updated_at: String::new(),
                    versions: Vec::new(),
                }),
            );
        }
        assert_eq!(cache.catalogs.len(), MAX_HISTORY_CACHE_ENTRIES);
    }

    #[test]
    fn artwork_cache_is_bounded_and_caches_missing_icons() {
        let mut cache = ArtworkCache::default();
        for index in 0..(MAX_ARTWORK_CACHE_ENTRIES + 10) {
            cache.insert(index.to_string(), None);
        }
        assert_eq!(cache.entries.len(), MAX_ARTWORK_CACHE_ENTRIES);
        assert_eq!(cache.get("265"), Some(None));
    }

    #[test]
    fn downloaded_filenames_recover_the_selected_platform() {
        assert_eq!(
            downloaded_file_platform("com.example_1.ipa"),
            store::search::DevicePlatform::Iphone
        );
        assert_eq!(
            downloaded_file_platform("com.example_1_ipad.ipa"),
            store::search::DevicePlatform::Ipad
        );
        assert_eq!(
            downloaded_file_platform("com.example_1_appletv.ipa"),
            store::search::DevicePlatform::Appletv
        );
    }

    #[test]
    fn security_headers_protect_sensitive_responses() {
        let mut response = Response::new(Body::empty());
        apply_security_headers(&mut response, "/api/search");

        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert!(response.headers().contains_key("permissions-policy"));
    }

    #[test]
    fn security_headers_apply_strict_csp_without_inline_script() {
        let mut response = Response::new(Body::empty());
        apply_security_headers(&mut response, "/");

        let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        assert!(csp.contains("script-src 'self'"));
        assert!(!csp.contains("unsafe-eval"));
        assert!(!csp.contains("script-src 'unsafe-inline'"));
    }

    #[test]
    fn cache_policy_follows_the_request_path() {
        for (path, expected) in [
            ("/api/search", "no-store"),
            ("/", "no-cache"),
            ("/app.js", "no-cache"),
            ("/icon.png", "no-cache"),
            ("/ota/a.ipa/app.ipa", "private, max-age=0, must-revalidate"),
        ] {
            let mut response = Response::new(Body::empty());
            apply_security_headers(&mut response, path);
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                expected,
                "path {path}"
            );
        }
    }

    #[test]
    fn range_parser_handles_single_suffix_and_open_ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Ok(Some((0, 99))));
        assert_eq!(parse_range("bytes=0-0", 100), Ok(Some((0, 0))));
        assert_eq!(parse_range("bytes=10-20", 100), Ok(Some((10, 20))));
        assert_eq!(parse_range("bytes=10-", 100), Ok(Some((10, 99))));
        assert_eq!(parse_range("bytes=-10", 100), Ok(Some((90, 99))));
        // 结尾越界按总长截断
        assert_eq!(parse_range("bytes=90-200", 100), Ok(Some((90, 99))));
        // 不可满足
        assert_eq!(parse_range("bytes=100-", 100), Err(()));
        assert_eq!(parse_range("bytes=-0", 100), Err(()));
        assert_eq!(parse_range("bytes=5-2", 100), Err(()));
        // 无法处理（回退整包 200）
        assert_eq!(parse_range("bytes=0-10,20-30", 100), Ok(None));
        assert_eq!(parse_range("octets=0-10", 100), Ok(None));
        assert_eq!(parse_range("garbage", 100), Ok(None));
    }

    #[test]
    fn credential_comparison_checks_content_and_length() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"Secret"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
    }

    fn headers_with_user_agent(user_agent: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::USER_AGENT, user_agent.parse().unwrap());
        headers
    }

    #[test]
    fn safari_detection_accepts_safari_and_rejects_wrapped_browsers() {
        let safari = headers_with_user_agent(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
        );
        let chrome = headers_with_user_agent(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/128.0 Mobile/15E148 Safari/604.1",
        );
        let webview = headers_with_user_agent(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148",
        );

        assert!(is_safari_user_agent(&safari));
        assert!(!is_safari_user_agent(&chrome));
        assert!(!is_safari_user_agent(&webview));
    }

    #[test]
    fn frontend_uses_no_inline_event_handlers() {
        // HTML 属性形态的内联处理器（onclick="…" 等）一律不允许；
        // JS 属性赋值（el.onclick = fn）不受 CSP 限制，属于正常写法。
        assert!(!INDEX_HTML.contains("onclick="));
        assert!(!INDEX_HTML.contains("onkeydown="));
        assert!(!INDEX_HTML.contains("onerror="));
        for pattern in ["onclick=\"", "onclick='", "onkeydown=\"", "onerror=\""] {
            assert!(!APP_JS.contains(pattern), "inline handler {pattern} found");
        }
        assert!(INDEX_HTML.contains("<script src=\"/app.js\"></script>"));
    }

    #[test]
    fn frontend_uses_lpak_icon_as_favicon() {
        assert!(INDEX_HTML.contains(r#"<link rel="icon" type="image/png" href="/icon.png">"#));
        assert!(INDEX_HTML.contains(r#"<link rel="apple-touch-icon" href="/icon.png">"#));
    }

    #[test]
    fn download_jobs_show_percentage_progress() {
        assert!(APP_JS.contains("function parseSize("));
        // 百分比进度条与总量展示
        assert!(
            APP_JS.contains("job.total ? Math.min(100,Math.floor(job.downloaded/job.total*100))")
        );
        assert!(APP_JS.contains("bar.style.width=`${pct}%`"));
        assert!(APP_JS.contains("${fmtSize(job.downloaded)} / ${fmtSize(job.total)}"));
        // 大小随下载请求提交，2FA 重试与登录重试都保留
        assert!(APP_JS.contains("size:size || undefined"));
        assert!(APP_JS.contains("size:challenge.total || null"));
        assert!(APP_JS.contains(
            "startDownload(retry.bundleId,retry.versionId,retry.platform,null,retry.size)"
        ));
        assert!(APP_JS.contains("data-size=\"${parseSize(entry.size) || ''}\""));
    }

    #[test]
    fn download_jobs_and_files_use_swipe_actions_with_pause_resume() {
        // 左滑手势基础设施
        assert!(APP_JS.contains("function makeSwipeable("));
        assert!(APP_JS.contains("class=\"swipe-action\""));
        assert!(INDEX_HTML.contains(".swipe-row { position:relative; overflow:hidden;"));
        // 任务：左滑删除，点击暂停/继续/安装
        assert!(APP_JS.contains("data-act=\"delete-job\""));
        assert!(APP_JS.contains("${encodeURIComponent(id)}/pause`,{method:'POST'}"));
        assert!(APP_JS.contains("${encodeURIComponent(id)}/resume`,{method:'POST'}"));
        assert!(APP_JS.contains("${encodeURIComponent(id)}`,{method:'DELETE'}"));
        assert!(APP_JS.contains("status==='paused'"));
        assert!(APP_JS.contains("'已暂停'"));
        // 文件：点击安装，左滑删除（不再有内联按钮）
        assert!(!APP_JS.contains("data-act=\"install-file\""));
        assert!(APP_JS.contains(
            "makeSwipeable(row, () => installFile(decodeURIComponent(row.dataset.name),null))"
        ));
        // swipe-row 必须在模板内闭合，否则下一行嵌套进来、删除按钮会盖住多行
        assert!(APP_JS.contains("</div></div></div></div>`).join('')"));
        // 进度轮询改为行内更新，避免滑动手势被重绘打断
        assert!(APP_JS.contains("function buildJobRow("));
        assert!(APP_JS.contains("function updateJobRow("));
    }

    #[test]
    fn custom_select_dropdown_raises_its_card_above_siblings() {
        assert!(INDEX_HTML.contains(".card.menu-open { z-index:6; }"));
        assert!(APP_JS.contains("classList.toggle('menu-open', open)"));
        assert!(APP_JS.contains("root.closest('.card')?.classList.remove('menu-open')"));
    }

    #[test]
    fn every_data_act_has_a_registered_action() {
        let mut used: Vec<&str> = Vec::new();
        for source in [INDEX_HTML, APP_JS] {
            let mut rest = source;
            while let Some(index) = rest.find("data-act=\"") {
                rest = &rest[index + "data-act=\"".len()..];
                let end = rest.find('"').unwrap();
                used.push(&rest[..end]);
            }
        }
        assert!(!used.is_empty());
        for act in used {
            assert!(
                APP_JS.contains(&format!("'{act}':")),
                "no ACTIONS entry for data-act=\"{act}\"",
            );
        }
    }

    #[test]
    fn frontend_browser_gate_shows_and_copies_the_current_app_address() {
        assert!(INDEX_HTML.contains("id=\"safari-address\""));
        assert!(INDEX_HTML.contains("id=\"copy-safari-address\""));
        assert!(APP_JS.contains("resolvedSafariAddress=location.origin"));
        assert!(APP_JS.contains("config.public_base_url"));
        assert!(APP_JS.contains("const address=await safariAddressPromise"));
        assert!(APP_JS.contains("navigator.clipboard?.writeText"));
        assert!(APP_JS.contains("document.execCommand('copy')"));
        assert!(!APP_JS.contains("subdomain.微服名.heiyu.space"));
    }

    #[test]
    fn frontend_downloaded_files_use_compact_rows_with_artwork() {
        assert!(INDEX_HTML.contains(".file-row"));
        assert!(APP_JS.contains("class=\"file-row\""));
        assert!(APP_JS.contains("file.artwork_url"));
        assert!(APP_JS.contains("platformLabel(file.platform)"));
        assert!(!INDEX_HTML.contains("responsive-table"));
    }

    #[tokio::test]
    async fn two_factor_error_response_is_machine_readable() {
        let response = store_err(store::StoreError::CodeRequired);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code_required"], true);
        assert_eq!(json["auth_expired"], true);
        assert!(!json["error"].as_str().unwrap().contains("--code"));
    }

    #[test]
    fn frontend_prompts_for_two_factor_and_uses_three_tab_navigation() {
        assert!(APP_JS.contains("error.codeRequired"));
        assert!(APP_JS.contains("job.needs_2fa"));
        assert!(APP_JS.contains("openLoginModal(true,challenge.account"));

        let header = INDEX_HTML.split("</header>").next().unwrap();
        assert!(!header.contains("account-trigger"));
        assert!(INDEX_HTML.contains("data-tab=\"account\""));
        assert!(INDEX_HTML.contains("data-tab=\"search\""));
        assert!(INDEX_HTML.contains("data-tab=\"download\""));
        assert_eq!(INDEX_HTML.matches("class=\"tab-button").count(), 3);
    }

    #[test]
    fn frontend_platform_switch_reaches_search_versions_and_downloads() {
        for platform in ["iphone", "ipad", "appletv"] {
            assert!(INDEX_HTML.contains(&format!("data-platform=\"{platform}\"")));
        }
        let header = INDEX_HTML
            .split("<header class=\"nav\">")
            .nth(1)
            .unwrap()
            .split("</header>")
            .next()
            .unwrap();
        assert!(header.contains("id=\"device-trigger\""));
        assert!(header.contains("id=\"device-menu\""));
        assert_eq!(header.matches("data-platform=").count(), 3);
        let search_panel = INDEX_HTML
            .split("id=\"panel-search\"")
            .nth(1)
            .unwrap()
            .split("id=\"panel-download\"")
            .next()
            .unwrap();
        assert!(!search_panel.contains("data-platform="));
        assert!(APP_JS.contains("&platform=${encodeURIComponent(platform)}"));
        assert!(APP_JS.contains(
            "body:JSON.stringify({bundle_id:bundleId,version_id:versionId || undefined,platform,size:size || undefined})"
        ));
        assert!(APP_JS.contains("pendingDownload={bundleId,versionId,platform}"));
    }

    #[test]
    fn frontend_versions_open_details_and_load_on_scroll_without_filters_or_pages() {
        assert!(APP_JS.contains("function showVersionDetail"));
        assert!(APP_JS.contains("IntersectionObserver"));
        assert!(APP_JS.contains("VERSION_BATCH_SIZE=20"));
        assert!(APP_JS.contains("(result.versions || []).map"));
        assert!(!APP_JS.contains("result.ids"));
        assert!(!APP_JS.contains("class=\"filters\""));
        assert!(!APP_JS.contains("function pageWindow"));

        let list_renderer = APP_JS
            .split("function versionItemHtml")
            .nth(1)
            .unwrap()
            .split("function stopVersionObserver")
            .next()
            .unwrap();
        assert!(!list_renderer.contains("start-download"));

        let detail_renderer = APP_JS
            .split("function showVersionDetail")
            .nth(1)
            .unwrap()
            .split("function backToVersionList")
            .next()
            .unwrap();
        assert!(detail_renderer.contains("data-act=\"start-download\""));
        assert!(detail_renderer.contains("entry.release_notes"));
        assert!(detail_renderer.contains("更新日志"));
        assert!(APP_JS.contains("function versionDetailIsOpen()"));
        assert!(APP_JS
            .contains("if (!force && versionDetailIsOpen()) { backToVersionList(); return; }"));
        assert!(APP_JS.contains("closeModal(true); showTab('download')"));
    }

    #[test]
    fn frontend_escapes_inline_arguments_and_ignores_stale_requests() {
        assert!(APP_JS.contains("replace(/'/g,'%27')"));
        assert!(APP_JS.contains("let searchRequestId=0"));
        assert!(APP_JS.contains("let versionRequestId=0"));
        assert!(APP_JS.contains("if (requestId!==searchRequestId)"));
        assert!(APP_JS.contains("if (requestId!==versionRequestId) return"));
    }

    #[test]
    fn frontend_login_modal_is_compact_aligned_and_free_of_decorative_copy() {
        assert!(INDEX_HTML.contains(".modal.compact { width:min(430px,94vw); }"));
        assert!(INDEX_HTML.contains(".modal h3 { min-height:30px;"));
        assert!(INDEX_HTML.contains("position:absolute; top:17px; right:17px;"));
        assert!(INDEX_HTML.contains(".modal .close { top:14px; right:12px; }"));
        assert!(!INDEX_HTML.contains("margin:0 0 -30px auto"));
        assert!(APP_JS.contains("class=\"login-form\""));
        assert!(APP_JS.contains("</div>`,false,true);"));
        assert!(!INDEX_HTML.contains("float:right"));
        assert!(!INDEX_HTML.contains("历史版本 · Safari 直接安装"));
        assert!(!INDEX_HTML.contains("新到旧 ·"));
    }

    #[test]
    fn package_includes_store_required_english_locale() {
        let package = include_str!("../../../package.yml");
        let locales = package
            .split_once("\nlocales:\n")
            .and_then(|(_, rest)| rest.split_once("\npermissions:"))
            .map(|(locales, _)| locales)
            .expect("package.yml must contain locales before permissions");

        assert!(locales.contains("  en:\n    name: "));
        assert!(locales.contains("  en:\n    name: ipaDown\n    description: "));
    }

    #[test]
    fn frontend_refreshes_files_on_changes_instead_of_continuous_polling() {
        assert!(APP_JS.contains("function keepJobsUpdated"));
        assert!(APP_JS.contains("completedJobIds"));
        assert!(!APP_JS.contains("setInterval(refreshFiles"));
    }

    #[test]
    fn frontend_avoids_ios_input_auto_zoom_without_disabling_accessibility_zoom() {
        let viewport = INDEX_HTML
            .split("<meta name=\"viewport\" content=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        assert!(!viewport.contains("user-scalable=no"));
        assert!(!viewport.contains("maximum-scale=1"));
        assert!(INDEX_HTML.contains("input { font-size:16px; }"));
        assert!(INDEX_HTML.contains("touch-action:manipulation"));
    }

    #[test]
    fn frontend_modal_locks_ios_page_scroll_and_guards_scroll_edges() {
        assert!(APP_JS.contains("function lockPageScroll()"));
        assert!(APP_JS.contains("function unlockPageScroll()"));
        assert!(APP_JS.contains("lockPageScroll();"));
        assert!(APP_JS.contains("unlockPageScroll();"));
        assert!(APP_JS.contains("function guardModalTouchMove"));
        assert!(APP_JS.contains("{passive:false}"));
    }

    #[test]
    fn frontend_download_gives_immediate_progress_feedback() {
        assert!(APP_JS.contains("id=\"download-feedback\""));
        assert!(APP_JS.contains("aria-busy"));
        assert!(APP_JS.contains("class=\"button-spinner\""));
        assert!(APP_JS.contains("正在创建下载任务，请稍候…"));

        let start_download = APP_JS
            .split("async function startDownload")
            .nth(1)
            .unwrap()
            .split("async function pollJobs")
            .next()
            .unwrap();
        assert!(
            start_download.find("button.disabled=true").unwrap()
                < start_download.find("await api('/api/downloads'").unwrap()
        );
    }

    #[test]
    fn frontend_desktop_debug_unlock_requires_five_silent_icon_clicks() {
        assert!(INDEX_HTML.contains("id=\"browser-gate-icon\""));
        assert!(APP_JS.contains("function isDesktopBrowser()"));
        assert!(APP_JS.contains("function handleDesktopDebugClick()"));
        assert!(APP_JS.contains("desktopDebugClicks < 5"));
        assert!(APP_JS.contains("sessionStorage"));
        assert!(INDEX_HTML.contains("id=\"desktop-debug-banner\""));

        let before_unlock = APP_JS
            .split("function handleDesktopDebugClick()")
            .nth(1)
            .unwrap()
            .split("enableDesktopDebug();")
            .next()
            .unwrap();
        assert!(!before_unlock.contains("toast("));
        assert!(!before_unlock.contains("textContent"));
    }
}
