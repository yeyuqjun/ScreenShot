use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use rand::{RngCore, rngs::OsRng};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

#[derive(Debug)]
pub struct ApiError(StatusCode, &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
impl From<rusqlite::Error> for ApiError {
    fn from(error: rusqlite::Error) -> Self {
        tracing::error!(%error, "database operation failed");
        Self(StatusCode::INTERNAL_SERVER_ERROR, "Database is temporarily unavailable")
    }
}
fn internal() -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, "Service is temporarily unavailable")
}
fn invalid(message: &'static str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message)
}
fn unauthorized() -> ApiError {
    ApiError(StatusCode::UNAUTHORIZED, "Session expired; please sign in again")
}
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn random_bytes(count: usize) -> Vec<u8> {
    let mut value = vec![0; count];
    OsRng.fill_bytes(&mut value);
    value
}
fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

pub fn open_database(path: impl AsRef<Path>) -> anyhow::Result<Connection> {
    let db = Connection::open(path)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch(include_str!("../schema.sql"))?;
    Ok(db)
}

pub fn validate_credentials(username: &str, password: &str) -> Result<String, ApiError> {
    let username = username.trim().to_lowercase();
    if !(3..=40).contains(&username.chars().count())
        || !username
            .chars()
            .all(|c| c.is_alphanumeric() || "_.-".contains(c))
    {
        return Err(invalid(
            "Username must be 3-40 letters, digits, underscores, dots, or hyphens",
        ));
    }
    if !(10..=128).contains(&password.chars().count()) || password.len() > 512 {
        return Err(invalid("Password must be 10-128 characters"));
    }
    Ok(username)
}

pub fn password_hash(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| anyhow::anyhow!("password hashing failed"))
}

pub fn create_user(db: &Connection, username: &str, password: &str) -> anyhow::Result<()> {
    let username = validate_credentials(username, password).map_err(|e| anyhow::anyhow!(e.1))?;
    db.execute(
        "INSERT INTO users(id,username,password_hash,vault_salt,created_at) VALUES(?1,?2,?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            username,
            password_hash(password)?,
            STANDARD.encode(random_bytes(32)),
            now()
        ],
    )?;
    Ok(())
}

pub fn set_setting(db: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    match key {
        "registration_enabled" if ["true", "false"].contains(&value) => {}
        "session_ttl_hours" if value.parse::<u32>().is_ok_and(|n| (1..=720).contains(&n)) => {}
        _ => anyhow::bail!(
            "Allowed settings: registration_enabled=true/false, session_ttl_hours=1..720"
        ),
    }
    db.execute(
        "UPDATE server_settings SET value=?2 WHERE key=?1",
        params![key, value],
    )?;
    Ok(())
}

#[derive(Clone)]
pub struct AppState {
    database: Arc<Mutex<Connection>>,
    limiter: Arc<Mutex<HashMap<(IpAddr, &'static str), (i64, u32)>>>,
    hashing: Arc<Semaphore>,
    dummy_hash: Arc<String>,
}
impl AppState {
    pub fn new(database: Connection) -> anyhow::Result<Self> {
        Ok(Self {
            database: Arc::new(Mutex::new(database)),
            limiter: Arc::default(),
            hashing: Arc::new(Semaphore::new(4)),
            dummy_hash: Arc::new(password_hash("dummy-password-never-used")?),
        })
    }
    async fn db<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, ApiError> + Send + 'static,
    ) -> Result<T, ApiError> {
        let db = self.database.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = db.lock().map_err(|_| internal())?;
            operation(&mut connection)
        })
        .await
        .map_err(|_| internal())?
    }
    fn limit(&self, ip: IpAddr, kind: &'static str, max: u32, window: i64) -> Result<(), ApiError> {
        let mut limits = self.limiter.lock().map_err(|_| internal())?;
        let time = now();
        if limits.len() > 10_000 {
            limits.retain(|_, (start, _)| time - *start < 3600);
        }
        if limits.len() >= 20_000 && !limits.contains_key(&(ip, kind)) {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many requests; please try again later",
            ));
        }
        let entry = limits.entry((ip, kind)).or_insert((time, 0));
        if time - entry.0 >= window {
            *entry = (time, 0);
        }
        if entry.1 >= max {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many requests; please try again later",
            ));
        }
        entry.1 += 1;
        Ok(())
    }
    async fn user(&self, headers: &HeaderMap) -> Result<String, ApiError> {
        let hash = bearer_hash(headers)?;
        self.db(move |db| {
            db.query_row(
                "SELECT user_id FROM sessions WHERE token_hash=?1 AND expires_at>?2",
                params![hash, now()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(unauthorized)
        })
        .await
    }
}
fn bearer_hash(headers: &HeaderMap) -> Result<String, ApiError> {
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| s.len() == 43)
        .ok_or_else(unauthorized)?;
    Ok(token_hash(token))
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/api/v1/meta", get(meta))
        .route("/api/v1/auth/register", post(register))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/config", get(get_config).put(put_config))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(axum::middleware::map_response(
            |mut response: Response| async move {
                let headers = response.headers_mut();
                headers.insert("cache-control", "no-store".parse().unwrap());
                headers.insert("x-content-type-options", "nosniff".parse().unwrap());
                headers.insert(
                    "strict-transport-security",
                    "max-age=31536000".parse().unwrap(),
                );
                response
            },
        ))
        .with_state(state)
}
async fn meta(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let enabled = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT value='true' FROM server_settings WHERE key='registration_enabled'",
                [],
                |r| r.get::<_, bool>(0),
            )?)
        })
        .await?;
    Ok(Json(
        json!({"name":"SnapDock", "version":env!("CARGO_PKG_VERSION"), "registration_enabled":enabled, "config_encryption":"AES-256-GCM", "kdf":"scrypt-16384-8-1"}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    username: String,
    password: String,
}
#[derive(Serialize)]
struct Session {
    token: String,
    expires_at: i64,
    username: String,
    vault_salt: String,
}

async fn new_session(state: &AppState, id: String) -> Result<Json<Session>, ApiError> {
    let token = URL_SAFE_NO_PAD.encode(random_bytes(32));
    let hash = token_hash(&token);
    let (username, vault_salt, expires_at) = state
        .db(move |db| {
            let (username, salt): (String, String) = db.query_row(
                "SELECT username,vault_salt FROM users WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let hours: i64 = db.query_row(
                "SELECT CAST(value AS INTEGER) FROM server_settings WHERE key='session_ttl_hours'",
                [],
                |r| r.get(0),
            )?;
            let expires = now() + hours.clamp(1, 720) * 3600;
            let tx = db.transaction()?;
            tx.execute("DELETE FROM sessions WHERE expires_at<=?1", [now()])?;
            tx.execute(
                "INSERT INTO sessions(token_hash,user_id,expires_at) VALUES(?1,?2,?3)",
                params![hash, id, expires],
            )?;
            tx.commit()?;
            Ok((username, salt, expires))
        })
        .await?;
    Ok(Json(Session {
        token,
        expires_at,
        username,
        vault_salt,
    }))
}
async fn register(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Credentials>,
) -> Result<Json<Session>, ApiError> {
    state.limit(addr.ip(), "register", 10, 3600)?;
    let username = validate_credentials(&body.username, &body.password)?;
    let enabled = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT value='true' FROM server_settings WHERE key='registration_enabled'",
                [],
                |r| r.get::<_, bool>(0),
            )?)
        })
        .await?;
    if !enabled {
        return Err(ApiError(StatusCode::FORBIDDEN, "Registration is disabled by the administrator"));
    }
    let permit = state
        .hashing
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| internal())?;
    let password = body.password;
    let hash = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        password_hash(&password)
    })
    .await
    .map_err(|_| internal())?
    .map_err(|_| internal())?;
    let id = uuid::Uuid::new_v4().to_string();
    let new_id = id.clone();
    state.db(move |db| {
        let tx = db.transaction()?;
        // Re-read inside the same transaction: a concurrent admin change must take effect.
        let enabled: bool = tx.query_row("SELECT value='true' FROM server_settings WHERE key='registration_enabled'", [], |r| r.get(0))?;
        if !enabled { return Err(ApiError(StatusCode::FORBIDDEN, "Registration is disabled by the administrator")); }
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE username=?1)",[&username],|r|r.get(0))?;
        if exists { return Err(ApiError(StatusCode::CONFLICT, "This username is already taken")); }
        tx.execute("INSERT INTO users(id,username,password_hash,vault_salt,created_at) VALUES(?1,?2,?3,?4,?5)",params![new_id,username,hash,STANDARD.encode(random_bytes(32)),now()])?;
        tx.commit()?;
        Ok(())
    }).await?;
    new_session(&state, id).await
}
async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Credentials>,
) -> Result<Json<Session>, ApiError> {
    state.limit(addr.ip(), "login", 15, 60)?;
    let username = validate_credentials(&body.username, &body.password)?;
    let user: Option<(String, String)> = state
        .db(move |db| {
            Ok(db
                .query_row(
                    "SELECT id,password_hash FROM users WHERE username=?1",
                    [username],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?)
        })
        .await?;
    let hash = user
        .as_ref()
        .map(|u| u.1.clone())
        .unwrap_or_else(|| state.dummy_hash.to_string());
    let permit = state
        .hashing
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| internal())?;
    let matched = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        PasswordHash::new(&hash).is_ok_and(|h| {
            Argon2::default()
                .verify_password(body.password.as_bytes(), &h)
                .is_ok()
        })
    })
    .await
    .map_err(|_| internal())?;
    if !matched || user.is_none() {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "Invalid username or password"));
    }
    new_session(&state, user.unwrap().0).await
}
async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let hash = bearer_hash(&headers)?;
    state
        .db(move |db| {
            db.execute("DELETE FROM sessions WHERE token_hash=?1", [hash])?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    pub revision: i64,
    pub nonce: Option<String>,
    pub ciphertext: Option<String>,
    pub updated_at: Option<i64>,
}
async fn get_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Config>, ApiError> {
    let user = state.user(&headers).await?;
    let config = state
        .db(move |db| {
            Ok(db
                .query_row(
                    "SELECT revision,nonce,ciphertext,updated_at FROM configs WHERE user_id=?1",
                    [user],
                    |r| {
                        Ok(Config {
                            revision: r.get(0)?,
                            nonce: r.get(1)?,
                            ciphertext: r.get(2)?,
                            updated_at: r.get(3)?,
                        })
                    },
                )
                .optional()?
                .unwrap_or(Config {
                    revision: 0,
                    nonce: None,
                    ciphertext: None,
                    updated_at: None,
                }))
        })
        .await?;
    Ok(Json(config))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutConfig {
    expected_revision: i64,
    nonce: String,
    ciphertext: String,
}
async fn put_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PutConfig>,
) -> Result<Json<Value>, ApiError> {
    let user = state.user(&headers).await?;
    if body.expected_revision < 0
        || STANDARD.decode(&body.nonce).map_or(true, |v| v.len() != 12)
        || STANDARD
            .decode(&body.ciphertext)
            .map_or(true, |v| !(16..=128 * 1024).contains(&v.len()))
    {
        return Err(invalid("Invalid encrypted configuration format"));
    }
    let (revision, updated_at) = state.db(move |db| {
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let revision: i64 = tx.query_row("SELECT revision FROM configs WHERE user_id=?1",[&user],|r|r.get(0)).optional()?.unwrap_or(0);
        if revision != body.expected_revision { return Err(ApiError(StatusCode::CONFLICT, "Cloud settings have changed; please download the latest cloud settings first")); }
        let next = revision.checked_add(1).ok_or_else(internal)?;
        let time = now();
        tx.execute("INSERT INTO configs(user_id,revision,nonce,ciphertext,updated_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(user_id) DO UPDATE SET revision=excluded.revision,nonce=excluded.nonce,ciphertext=excluded.ciphertext,updated_at=excluded.updated_at",params![user,next,body.nonce,body.ciphertext,time])?;
        tx.commit()?;
        Ok((next,time))
    }).await?;
    Ok(Json(json!({"revision":revision,"updated_at":updated_at})))
}

#[cfg(test)]
mod tests;
