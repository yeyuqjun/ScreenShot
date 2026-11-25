use super::*;
use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn setup(registration: bool) -> (Router, AppState) {
    let db = open_database(":memory:").unwrap();
    set_setting(
        &db,
        "registration_enabled",
        if registration { "true" } else { "false" },
    )
    .unwrap();
    let state = AppState::new(db).unwrap();
    (router(state.clone()), state)
}
async fn send(
    app: &Router,
    method: &str,
    route: &str,
    body: Value,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(route)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let mut request = request.body(Body::from(body.to_string())).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}
async fn register_account(app: &Router, username: &str) -> String {
    let (status, result) = send(
        app,
        "POST",
        "/api/v1/auth/register",
        json!({"username":username,"password":"secure-password-123"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        STANDARD
            .decode(result["vault_salt"].as_str().unwrap())
            .unwrap()
            .len(),
        32
    );
    result["token"].as_str().unwrap().to_string()
}
fn envelope(revision: i64) -> Value {
    json!({"expected_revision":revision,"nonce":STANDARD.encode([7u8;12]),"ciphertext":STANDARD.encode([9u8;40])})
}

#[tokio::test]
async fn registration_switch_is_read_from_database_on_each_request() {
    let (app, state) = setup(false);
    let (status, meta) = send(&app, "GET", "/api/v1/meta", json!(null), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["registration_enabled"], false);
    let (status, _) = send(
        &app,
        "POST",
        "/api/v1/auth/register",
        json!({"username":"tester","password":"secure-password-123"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    state
        .db(|db| {
            set_setting(db, "registration_enabled", "true").unwrap();
            Ok(())
        })
        .await
        .unwrap();
    register_account(&app, "tester").await;
    state
        .db(|db| {
            set_setting(db, "registration_enabled", "false").unwrap();
            Ok(())
        })
        .await
        .unwrap();
    let (status, _) = send(
        &app,
        "POST",
        "/api/v1/auth/register",
        json!({"username":"tester2","password":"secure-password-123"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
#[tokio::test]
async fn login_hashing_salts_and_revocation() {
    let (app, state) = setup(true);
    let token = register_account(&app, "Tester").await;
    let (status, login) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        json!({"username":"TESTER","password":"secure-password-123"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(login["username"], "tester");
    state
        .db(move |db| {
            let hash: String = db.query_row("SELECT password_hash FROM users", [], |r| r.get(0))?;
            assert!(hash.starts_with("$argon2id$"));
            assert!(!hash.contains("secure-password"));
            let stored: String =
                db.query_row("SELECT token_hash FROM sessions LIMIT 1", [], |r| r.get(0))?;
            assert_eq!(stored.len(), 64);
            assert_ne!(stored, token);
            Ok(())
        })
        .await
        .unwrap();
    let token = login["token"].as_str().unwrap();
    let (status, _) = send(&app, "POST", "/api/v1/auth/logout", json!({}), Some(token)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, "GET", "/api/v1/config", json!(null), Some(token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        json!({"username":"tester","password":"wrong-password"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn config_is_private_and_revision_conflicts_do_not_overwrite() {
    let (app, _) = setup(true);
    let alice = register_account(&app, "alice").await;
    let bob = register_account(&app, "bobby").await;
    let (status, empty) = send(&app, "GET", "/api/v1/config", json!(null), Some(&alice)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["revision"], 0);
    let (status, written) = send(&app, "PUT", "/api/v1/config", envelope(0), Some(&alice)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(written["revision"], 1);
    let (status, _) = send(&app, "PUT", "/api/v1/config", envelope(0), Some(&alice)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, read) = send(&app, "GET", "/api/v1/config", json!(null), Some(&alice)).await;
    assert_eq!(read["revision"], 1);
    assert_eq!(read["ciphertext"], envelope(0)["ciphertext"]);
    let (_, bob_config) = send(&app, "GET", "/api/v1/config", json!(null), Some(&bob)).await;
    assert_eq!(bob_config["revision"], 0);
    assert_eq!(bob_config["ciphertext"], Value::Null);
    let (status, _) = send(&app, "PUT", "/api/v1/config", envelope(1), Some(&alice)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, "GET", "/api/v1/config", json!(null), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn expired_sessions_and_malformed_envelopes_are_rejected() {
    let (app, state) = setup(true);
    let token = register_account(&app, "tester").await;
    for value in [
        json!({"expected_revision":0,"nonce":"bad","ciphertext":"bad"}),
        json!({"expected_revision":-1,"nonce":STANDARD.encode([0;12]),"ciphertext":STANDARD.encode([0;16])}),
    ] {
        let (status, _) = send(&app, "PUT", "/api/v1/config", value, Some(&token)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    state
        .db(|db| {
            db.execute("UPDATE sessions SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let (status, _) = send(&app, "GET", "/api/v1/config", json!(null), Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn auth_rate_limit_is_bounded() {
    let (app, state) = setup(false);
    for _ in 0..15 {
        state
            .limit("127.0.0.1".parse().unwrap(), "login", 15, 60)
            .unwrap();
    }
    let (status, _) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        json!({"username":"tester","password":"wrong-password"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}
#[test]
fn validates_passwords_usernames_and_admin_settings() {
    assert!(validate_credentials(" A_B-9 ", "correct-horse-123").is_ok());
    assert!(validate_credentials("ab", "correct-horse-123").is_err());
    assert!(validate_credentials("bad name", "correct-horse-123").is_err());
    assert!(validate_credentials("tester", "short").is_err());
    let db = open_database(":memory:").unwrap();
    assert!(set_setting(&db, "registration_enabled", "yes").is_err());
    assert!(set_setting(&db, "session_ttl_hours", "99999").is_err());
}
