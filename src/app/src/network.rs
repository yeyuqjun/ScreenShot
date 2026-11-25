//! Networking: HTTP requests (custom CA, size/timeout limits), sync encryption envelope,
//! and OCR/translation remote providers.

use crate::i18n;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{json, Value};

const AAD: &[u8] = b"snapdock-config-v1";
const MAX_RESPONSE: usize = 4 * 1024 * 1024;

/// Validate an endpoint URL: HTTPS required (localhost HTTP permitted only for AI / API providers).
pub fn endpoint(value: &str, allow_local_http: bool) -> Result<url::Url, String> {
    let parsed = url::Url::parse(value).map_err(|_| i18n::t("Please enter a complete endpoint URL"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(i18n::t("Endpoint URL must not contain credentials or fragments").into());
    }
    if parsed.fragment().is_some() {
        return Err(i18n::t("Endpoint URL must not contain credentials or fragments").into());
    }
    let host = parsed.host_str().unwrap_or("");
    let local = matches!(host.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "[::1]" | "::1");
    if parsed.scheme() != "https"
        && !(allow_local_http && local && parsed.scheme() == "http")
    {
        return Err(i18n::t("Endpoint must use HTTPS (local HTTP is allowed for local AI / API services)").into());
    }
    Ok(parsed)
}

/// JSON request with optional custom CA, 45s timeout, and 4 MB response limit.
pub async fn request_json(
    value: &str,
    method: &str,
    token: Option<&str>,
    body: Option<&Value>,
    ca_pem: Option<&str>,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let parsed = endpoint(value, true)?;
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .use_rustls_tls();
    if let Some(pem) = ca_pem {
        if parsed.scheme() == "https" {
            let certificate = reqwest::Certificate::from_pem(pem.as_bytes()).map_err(|_| {
                i18n::t("Invalid certificate format; please import a PEM public certificate")
            })?;
            builder = builder.add_root_certificate(certificate);
        }
    }
    let client = builder.build().map_err(|e| {
        i18n::tf(
            "Failed to create HTTP client: {err}",
            &[("err", &e.to_string())],
        )
    })?;
    let mut request = match method {
        "POST" => client.post(parsed.clone()),
        "PUT" => client.put(parsed.clone()),
        _ => client.get(parsed.clone()),
    };
    if let Some(body) = body {
        request = request.json(body);
    }
    if let Some(auth) = token.or(api_key) {
        request = request.bearer_auth(auth);
    }
    let response = request
        .send()
        .await
        .map_err(|error| map_request_error(&error))?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(|e| {
        i18n::tf(
            "Failed to read response body: {err}",
            &[("err", &e.to_string())],
        )
    })?;
    if bytes.len() > MAX_RESPONSE {
        return Err(i18n::t("Response payload is too large").into());
    }
    let result: Value = serde_json::from_slice(&bytes).map_err(|_| {
        i18n::tf(
            "Endpoint did not return valid JSON (HTTP {status})",
            &[("status", &status.as_u16().to_string())],
        )
    })?;
    if !status.is_success() {
        let raw = result
            .get("error")
            .cloned()
            .map(|error| match error {
                Value::String(text) => text,
                Value::Object(map) => map
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or_default()
                    .to_string(),
                _ => String::new(),
            })
            .or_else(|| {
                result
                    .get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            })
            .filter(|text| !text.is_empty());
        let message = raw
            .map(|text| text.chars().take(300).collect::<String>())
            .unwrap_or_else(|| {
                i18n::tf(
                    "Request failed (HTTP {status})",
                    &[("status", &status.as_u16().to_string())],
                )
            });
        return Err(message);
    }
    Ok(result)
}

fn map_request_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        i18n::t("Request timed out").into()
    } else if error.is_connect() {
        i18n::t("Unable to connect to server; check the URL and ensure the service is running")
            .into()
    } else {
        i18n::tf("Request failed: {err}", &[("err", &error.to_string())])
    }
}

/// Sync server URL: `<base>/api/v1/<route>`.
pub fn service_url(settings: &Value, route: &str) -> Result<url::Url, String> {
    let mut url = endpoint(settings["service"]["url"].as_str().unwrap_or(""), false)?;
    let base = url.path().trim_end_matches('/').to_string();
    url.set_path(&format!("{base}/api/v1/{route}"));
    url.set_query(None);
    Ok(url)
}

pub async fn service_request(
    settings: &Value,
    route: &str,
    method: &str,
    token: Option<&str>,
    body: Option<&Value>,
) -> Result<Value, String> {
    let url = service_url(settings, route)?;
    let ca = settings["service"]["caPem"].as_str();
    request_json(url.as_str(), method, token, body, ca, None).await
}

/// Derive a 32-byte encryption key via scrypt (N=16384, r=8, p=1).
pub fn derive_key(password: &str, salt: &str) -> Result<[u8; 32], String> {
    use base64::engine::general_purpose::STANDARD;
    let salt_bytes = STANDARD
        .decode(salt)
        .map_err(|_| i18n::t("Server returned an invalid encryption salt"))?;
    if salt_bytes.len() != 32 {
        return Err(i18n::t("Server returned an invalid encryption salt").into());
    }
    let params = scrypt::Params::new(14, 8, 1, 32)
        .map_err(|_| i18n::t("Invalid encryption parameters"))?;
    let mut output = [0u8; 32];
    scrypt::scrypt(password.as_bytes(), &salt_bytes, &params, &mut output)
        .map_err(|_| i18n::t("Key derivation failed"))?;
    Ok(output)
}

/// Encrypt configuration with AES-256-GCM (12-byte random nonce + authentication tag).
pub fn encrypt_config(value: &Value, key: &[u8; 32]) -> Result<Value, String> {
    use aes_gcm::aead::OsRng;
    use aes_gcm::aead::rand_core::RngCore;
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let ciphertext = cipher
        .encrypt(nonce, Payload { msg: &plaintext, aad: AAD })
        .map_err(|_| i18n::t("Configuration encryption failed"))?;
    Ok(json!({
        "nonce": STANDARD.encode(nonce_bytes),
        "ciphertext": STANDARD.encode(ciphertext),
    }))
}

/// Decrypt a cloud configuration envelope with AES-256-GCM.
pub fn decrypt_config(envelope: &Value, key: &[u8; 32]) -> Result<Value, String> {
    let fail = || i18n::t("Failed to decrypt cloud settings: password or payload mismatch").to_string();
    let decode = |field: &str| -> Result<Vec<u8>, String> {
        envelope
            .get(field)
            .and_then(|v| v.as_str())
            .map(|v| STANDARD.decode(v))
            .transpose()
            .map_err(|_| fail())?
            .ok_or_else(fail)
    };
    let ciphertext = decode("ciphertext")?;
    let nonce_bytes = decode("nonce")?;
    if nonce_bytes.len() != 12 || ciphertext.len() < 16 || ciphertext.len() > 128 * 1024 {
        return Err(fail());
    }
    let cipher = Aes256Gcm::new(key.into());
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: &ciphertext,
                aad: AAD,
            },
        )
        .map_err(|_| fail())?;
    serde_json::from_slice(&plaintext).map_err(|_| fail())
}

/// Extract a text field along a dot-separated path (supporting array indices); blocks prototype keys.
fn response_text(value: &Value, path: &str) -> Result<String, String> {
    if path.is_empty()
        || path
            .split('.')
            .any(|part| matches!(part, "__proto__" | "prototype" | "constructor"))
    {
        return Err(i18n::t("Please configure a valid response field path").into());
    }
    let mut cursor: Option<&Value> = Some(value);
    for part in path.split('.') {
        cursor = cursor.and_then(|current| match part.parse::<usize>() {
            Ok(index) => current.get(index),
            Err(_) => current.get(part),
        });
    }
    let Some(text) = cursor.and_then(|v| v.as_str()) else {
        return Err(i18n::tf(
            "Response does not contain text field [{path}]",
            &[("path", path)],
        ));
    };
    Ok(text.to_string())
}

/// Execute remote OCR or translation via API or AI provider.
pub async fn process_text(
    settings: &Value,
    kind: &str,
    image: Option<&str>,
    text: Option<&str>,
) -> Result<String, String> {
    let Some(provider) = settings["providers"].get(kind) else {
        return Err(i18n::t("Unknown processing type").into());
    };
    let mode = provider.get("mode").and_then(|v| v.as_str()).unwrap_or("api");
    let Some(selected) = provider.get(mode) else {
        return Err(i18n::t("Unknown processing type").into());
    };
    let selected_url = selected.get("url").and_then(|v| v.as_str()).unwrap_or("");
    endpoint(selected_url, true)?;
    let is_ocr = kind == "ocr";
    if is_ocr && (image.is_none() || !image.unwrap().starts_with("data:image/png;base64,")) {
        return Err(i18n::t("Please select a capture first").into());
    }
    if !is_ocr && text.is_none_or(|t| t.trim().is_empty()) {
        return Err(i18n::t("No text available to translate").into());
    }
    let api_key = selected.get("key").and_then(|v| v.as_str());
    if mode == "api" {
        let body = if is_ocr {
            json!({
                "image_base64": image.unwrap().split(',').nth(1).unwrap_or(""),
                "mime_type": "image/png",
            })
        } else {
            json!({
                "text": text.unwrap_or(""),
                "source": provider["source"],
                "target": provider["target"],
            })
        };
        let result = request_json(selected_url, "POST", None, Some(&body), None, api_key).await?;
        let path = selected
            .get("responsePath")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return response_text(&result, path);
    }
    let model = selected
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if model.is_empty() {
        return Err(i18n::t("Please configure an AI model name in Settings").into());
    }
    let raw_target = provider
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("en");
    let target_code = crate::translate::normalize_lang_code(raw_target);
    let target = crate::translate::find_language(&target_code)
        .map(|l| l.en_name)
        .unwrap_or(raw_target);
    let raw_source = provider
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    let source_code = crate::translate::normalize_lang_code(raw_source);
    let source = crate::translate::find_language(&source_code)
        .map(|l| l.en_name)
        .unwrap_or("Auto Detect");
    let prompt = selected
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .replace("{target}", target)
        .replace("{source}", source);
    let content = if is_ocr {
        json!([
            { "type": "text", "text": prompt },
            { "type": "image_url", "image_url": { "url": image.unwrap() } },
        ])
    } else {
        json!(format!("{}\n\n{}", prompt, text.unwrap_or("")))
    };
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": content }],
        "stream": false,
    });
    let result = request_json(selected_url, "POST", None, Some(&body), None, api_key).await?;
    response_text(&result, "choices.0.message.content")
}

/// Parse a PEM public certificate and return `(fingerprint256, subject)`.
pub fn parse_certificate(pem: &str) -> Result<(String, String), String> {
    use der::{Decode, Encode};
    use sha2::{Digest, Sha256};
    // Strip PEM headers/footers and decode base64 into DER.
    let der = pem
        .lines()
        .filter(|line| {
            !line.trim().starts_with("-----")
        })
        .fold(String::new(), |mut body, line| {
            body.push_str(line.trim());
            body
        });
    let der = base64::engine::general_purpose::STANDARD
        .decode(der)
        .map_err(|_| i18n::t("Certificate file is not a valid PEM certificate"))?;
    let certificate = x509_cert::Certificate::from_der(&der)
        .map_err(|_| i18n::t("Certificate file is not a valid PEM certificate"))?;
    let der = certificate
        .to_der()
        .map_err(|_| i18n::t("Certificate encoding failed"))?;
    let mut hasher = Sha256::new();
    hasher.update(&der);
    let fingerprint = format!("{:x}", hasher.finalize())
        .as_bytes()
        .chunks(2)
        .map(|pair| format!("{:02X}", u8::from_str_radix(std::str::from_utf8(pair).unwrap_or("00"), 16).unwrap_or(0)))
        .collect::<Vec<String>>()
        .join(":");
    let subject = certificate.tbs_certificate.subject.to_string();
    Ok((fingerprint, subject))
}

/// Format Unix milliseconds as UTC ISO-8601 timestamp (used for `lastSync`).
pub fn utc_iso(millis: i64) -> String {
    let secs = millis / 1000;
    let millis_part = millis % 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year,
        month,
        day,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        millis_part
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_validates_scheme() {
        assert!(endpoint("https://example.com", false).is_ok());
        assert!(endpoint("http://127.0.0.1:8080/v1", true).is_ok());
        assert!(endpoint("http://example.com", false).is_err());
        assert!(endpoint("ftp://example.com", false).is_err());
        assert!(endpoint("https://user:pass@example.com", false).is_err());
        assert!(endpoint("not a url", false).is_err());
    }

    #[test]
    fn service_url_appends_route() {
        let settings = json!({ "service": { "url": "https://127.0.0.1:8899" } });
        let url = service_url(&settings, "auth/login").unwrap();
        assert_eq!(url.as_str(), "https://127.0.0.1:8899/api/v1/auth/login");
    }

    #[test]
    fn config_envelope_round_trips() {
        let key = [7u8; 32];
        let payload = json!({ "general": { "theme": "dark" } });
        let envelope = encrypt_config(&payload, &key).unwrap();
        let restored = decrypt_config(&envelope, &key).unwrap();
        assert_eq!(restored, payload);
        let wrong_key = [8u8; 32];
        assert!(decrypt_config(&envelope, &wrong_key).is_err());
    }

    #[test]
    fn derive_key_length_is_32() {
        let salt = base64::engine::general_purpose::STANDARD.encode([1u8; 32]);
        let key = derive_key("password123", &salt).unwrap();
        assert_eq!(key.len(), 32);
        assert!(derive_key("x", "short").is_err());
    }

    #[test]
    fn response_text_blocks_prototype_paths() {
        let value = json!({ "data": { "text": "hi" } });
        assert_eq!(response_text(&value, "data.text").unwrap(), "hi");
        assert!(response_text(&value, "__proto__.text").is_err());
    }

    #[test]
    fn utc_iso_formats_utc() {
        assert_eq!(utc_iso(0), "1970-01-01T00:00:00.000Z");
    }
}
