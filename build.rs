//! Configura el backend y la API key del tenant desde `.env` en tiempo de
//! compilación.
//!
//! `cargo:rustc-env` deja los valores en `env!("DEEPGRAM_BACKEND_URL")` y
//! `env!("DEEPGRAM_TENANT_KEY")`, así que el APK de Android también los lleva.
//! En escritorio, `config.rs` puede sobrescribirlos con un `.env` de runtime
//! sin recompilar.

use std::collections::HashMap;
use std::path::Path;

const DEFAULT_BACKEND: &str = "http://127.0.0.1:8000";

fn main() {
    println!("cargo:rerun-if-changed=.env");
    let vars = std::fs::read_to_string(Path::new(".env"))
        .ok()
        .map(|text| parse(&text))
        .unwrap_or_default();

    let backend = resolve_backend(&vars);
    println!("cargo:rustc-env=DEEPGRAM_BACKEND_URL={backend}");
    println!(
        "cargo:rustc-env=DEEPGRAM_TENANT_KEY={}",
        vars.get("TENANT_KEY").cloned().unwrap_or_default()
    );
}

fn resolve_backend(vars: &HashMap<String, String>) -> String {
    let mut backend = vars
        .get("BACKEND_URL")
        .filter(|url| !url.is_empty())
        .cloned()
        .unwrap_or_else(|| DEFAULT_BACKEND.to_string());
    if let Some(port) = vars.get("BACKEND_PORT") {
        backend = with_port(&backend, port);
    }
    backend
}

fn parse(text: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        vars.insert(key.to_string(), unquote(value.trim()).to_string());
    }
    vars
}

fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn with_port(url: &str, port: &str) -> String {
    if port.parse::<u16>().map(|value| value > 0) != Ok(true) {
        return url.to_string();
    }
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let authority_start = scheme_end + 3;
    let authority_end = url[authority_start..]
        .find('/')
        .map(|index| authority_start + index)
        .unwrap_or(url.len());
    let authority = &url[authority_start..authority_end];
    let host = if let Some(close) = authority.rfind(']') {
        &authority[..close + 1]
    } else {
        match authority.rfind(':') {
            Some(index) => &authority[..index],
            None => authority,
        }
    };
    format!(
        "{}://{}:{}{}",
        &url[..scheme_end],
        host,
        port,
        &url[authority_end..]
    )
}
