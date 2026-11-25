//! Local encrypted storage (Windows DPAPI) and capture history index.
//! `vault.bin` stores `{settings, account}` and remains compatible with legacy Electron safeStorage payloads.

use crate::i18n;
use crate::state::{Account, App, CaptureItem};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct VaultData {
    settings: Value,
    account: Option<Account>,
}

#[cfg(windows)]
mod dpapi {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN,
        CRYPT_INTEGER_BLOB,
    };

    fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr() as *mut u8,
        }
    }

    /// Return decrypted plaintext bytes, or `None` on failure.
    pub fn unprotect(input: &[u8]) -> Option<Vec<u8>> {
        let input = blob(input);
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
            .ok()?;
            let data =
                std::slice::from_raw_parts(output.pbData, output.cbData as usize);
            let plain = data.to_vec();
            let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
            Some(plain)
        }
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob(plain);
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
            .map_err(|e| e.to_string())?;
            let data =
                std::slice::from_raw_parts(output.pbData, output.cbData as usize);
            let protected = data.to_vec();
            let _ = LocalFree(Some(HLOCAL(output.pbData as *mut _)));
            Ok(protected)
        }
    }
}

/// Read `vault.bin`. If decryption fails, back up the original file and start from defaults
/// without overwriting user data; return an error if decrypted JSON is corrupted.
pub fn read_vault(app: &App) -> Result<(), String> {
    if !app.vault_file.exists() {
        return Ok(());
    }
    let stored = match fs::read(&app.vault_file) {
        Ok(bytes) => bytes,
        Err(error) => return Err(error.to_string()),
    };
    let Some(plain) = decrypt_electron(&stored) else {
        let backup = app
            .vault_file
            .with_extension("bin.unreadable");
        let _ = fs::rename(&app.vault_file, &backup);
        return Ok(());
    };
    let text = String::from_utf8_lossy(&plain);
    let data: VaultData = serde_json::from_str(&text).map_err(|error| {
        i18n::tf(
            "Local configuration is corrupted: {err}",
            &[("err", &error.to_string())],
        )
    })?;
    let mut core = app.core.lock().unwrap();
    core.settings = crate::settings::normalize(Some(&data.settings));
    if data
        .account
        .as_ref()
        .is_some_and(|a| a.expires_at <= now_seconds())
    {
        core.account = None;
    } else {
        core.account = data.account;
    }
    Ok(())
}

/// Legacy-compatible Electron safeStorage decryption (raw DPAPI, `"DPAPI"`, or `"v10"` prefix).
fn decrypt_electron(stored: &[u8]) -> Option<Vec<u8>> {
    #[cfg(windows)]
    {
        if let Some(plain) = dpapi::unprotect(stored) {
            return Some(plain);
        }
        for prefix in [b"DPAPI".as_slice(), b"v10".as_slice()] {
            if stored.starts_with(prefix) {
                if let Some(plain) = dpapi::unprotect(&stored[prefix.len()..]) {
                    return Some(plain);
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    None
}

pub async fn save_vault(app: &App) -> Result<(), String> {
    let (settings, account) = {
        let core = app.core.lock().unwrap();
        (core.settings.clone(), core.account.clone())
    };
    let plain =
        serde_json::to_vec(&VaultData { settings, account })
            .map_err(|e| e.to_string())?;
    let encrypted = protect_bytes(&plain)?;
    let lock = app.vault_lock.lock().await;
    let result = atomic_write(&app.vault_file, &encrypted);
    drop(lock);
    result
}

fn protect_bytes(plain: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        dpapi::protect(plain)
    }
    #[cfg(not(windows))]
    {
        Err(i18n::t("Encrypted storage is not supported on this platform").into())
    }
}

/// Load capture history index, filtering missing or invalid filenames.
pub fn read_history(app: &App) {
    let contents = fs::read_to_string(&app.index_file).unwrap_or_default();
    let items: Vec<CaptureItem> = match serde_json::from_str(&contents) {
        Ok(items) => items,
        Err(error) => {
            eprintln!("[snapdock] failed to parse history: {error}");
            Vec::new()
        }
    };
    let before = items.len();
    let valid: Vec<CaptureItem> = items
        .into_iter()
        .filter(|item| {
            valid_capture_file(&item.file)
                && app.capture_dir.join(&item.file).exists()
        })
        .collect();
    eprintln!("[snapdock] loaded history {}/{} items", valid.len(), before);
    app.core.lock().unwrap().history = valid;
}

pub fn valid_capture_file(file: &str) -> bool {
    let bytes = file.as_bytes();
    let (stem, ext) = match file.rsplit_once('.') {
        Some(parts) => parts,
        None => return false,
    };
    (ext == "png" || ext == "gif")
        && stem.len() == 36
        && stem
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'-')
        && bytes.len() <= 64
}

#[allow(dead_code)]
pub async fn save_history(app: &App) -> Result<(), String> {
    let contents = {
        let core = app.core.lock().unwrap();
        serde_json::to_string(&core.history).map_err(|e| e.to_string())?
    };
    let lock = app.index_lock.lock().await;
    let result = atomic_write(&app.index_file, contents.as_bytes());
    drop(lock);
    result
}

/// Atomic write via temporary file and rename.
pub fn atomic_write(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = target.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&tmp, target).map_err(|e| e.to_string())
}

pub fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_file_names_are_validated() {
        assert!(valid_capture_file(
            "0e8d9c2f-8f2e-4a70-9d1f-2b6e0a9c1f33.png"
        ));
        assert!(valid_capture_file(
            "0e8d9c2f-8f2e-4a70-9d1f-2b6e0a9c1f33.gif"
        ));
        assert!(!valid_capture_file("../secret.png"));
        assert!(!valid_capture_file("notes.txt"));
        assert!(!valid_capture_file(
            "zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz.png"
        ));
    }
}
