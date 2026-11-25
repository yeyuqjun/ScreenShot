//! Global shortcuts: pure-Rust `global-hotkey` registration and parsing.

use crate::i18n;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::GlobalHotKeyManager;
use serde_json::Value;
use std::collections::HashMap;
use std::str::FromStr;

pub struct HotkeyRegistry {
    manager: Option<GlobalHotKeyManager>,
    registered: Vec<HotKey>,
    pub actions: HashMap<u32, String>,
}

impl HotkeyRegistry {
    pub fn new() -> Self {
        let manager = GlobalHotKeyManager::new().ok();
        Self {
            manager,
            registered: Vec::new(),
            actions: HashMap::new(),
        }
    }

    /// Unregister all hotkeys and re-register from settings; returns user-facing warnings for failures.
    pub fn register(&mut self, settings: &Value) -> Vec<String> {
        let mut warnings = Vec::new();
        let Some(manager) = &self.manager else {
            warnings.push(i18n::t("Failed to initialize global hotkey service").into());
            return warnings;
        };
        if !self.registered.is_empty() {
            let _ = manager.unregister_all(&self.registered);
            self.registered.clear();
            self.actions.clear();
        }
        let Some(map) = settings.get("shortcuts").and_then(|v| v.as_object()) else {
            return warnings;
        };
        for (name, value) in map {
            let key = value.as_str().unwrap_or("").trim().to_string();
            if key.is_empty() {
                continue;
            }
            match parse_hotkey(&key) {
                Some(hotkey) => match manager.register(hotkey) {
                    Ok(()) => {
                        self.actions.insert(hotkey.id(), name.clone());
                        self.registered.push(hotkey);
                    }
                    Err(_) => {
                        warnings.push(i18n::tf(
                            "Shortcut {key} is already in use by another application",
                            &[("key", &key)],
                        ));
                    }
                },
                None => {
                    warnings.push(i18n::tf(
                        "Shortcut {key} has an invalid format",
                        &[("key", &key)],
                    ));
                }
            }
        }
        warnings
    }
}

/// Parse strings like "Alt+Shift+A" or "CommandOrControl+Shift+S" into a `HotKey`.
pub fn parse_hotkey(spec: &str) -> Option<HotKey> {
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;
    for part in spec.split('+') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "commandorcontrol" | "cmdorctrl" | "control" | "ctrl" => {
                mods |= Modifiers::CONTROL;
            }
            "command" | "cmd" | "super" | "meta" | "win" => {
                mods |= Modifiers::META;
            }
            "alt" | "option" | "altgr" => {
                mods |= Modifiers::ALT;
            }
            "shift" => {
                mods |= Modifiers::SHIFT;
            }
            _ => {
                if code.is_some() {
                    return None;
                }
                code = Some(parse_code(&lower)?);
            }
        }
    }
    let code = code?;
    Some(HotKey::new(if mods.is_empty() { None } else { Some(mods) }, code))
}

fn parse_code(token: &str) -> Option<Code> {
    if token.len() == 1 {
        let c = token.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Code::from_str(&format!("Key{}", c.to_ascii_uppercase())).ok();
        }
        if c.is_ascii_digit() {
            return Code::from_str(&format!("Digit{}", c)).ok();
        }
        return None;
    }
    if token.len() >= 2
        && token.starts_with('f')
        && token[1..].chars().all(|c| c.is_ascii_digit())
    {
        return Code::from_str(&token.to_ascii_uppercase()).ok();
    }
    let mapped = match token {
        "space" => "Space",
        "enter" | "return" => "Enter",
        "tab" => "Tab",
        "escape" | "esc" => "Escape",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "insert" | "ins" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "arrowup" | "up" => "ArrowUp",
        "arrowdown" | "down" => "ArrowDown",
        "arrowleft" | "left" => "ArrowLeft",
        "arrowright" | "right" => "ArrowRight",
        "minus" | "-" => "Minus",
        "equal" | "=" => "Equal",
        "quote" => "Quote",
        "comma" | "," => "Comma",
        "period" | "." => "Period",
        "slash" | "/" => "Slash",
        "backslash" | "\\" => "Backslash",
        "semicolon" | ";" => "Semicolon",
        "bracketleft" | "[" => "BracketLeft",
        "bracketright" | "]" => "BracketRight",
        "printscreen" | "prtsc" => "PrintScreen",
        _ => return Code::from_str(token).ok(),
    };
    Code::from_str(mapped).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_shortcuts() {
        assert!(parse_hotkey("Alt+Shift+A").is_some());
        assert!(parse_hotkey("CommandOrControl+Shift+S").is_some());
        assert!(parse_hotkey("Ctrl+F1").is_some());
        assert!(parse_hotkey("Alt+Space").is_some());
        assert!(parse_hotkey("").is_none());
        assert!(parse_hotkey("Alt+A+B").is_none());
    }
}
