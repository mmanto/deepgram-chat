//! Configuración del cliente: backend de gestion.ar y API key del tenant.
//!
//! Los valores se embeben al compilar desde `.env` (`build.rs`), así que el
//! APK de Android arranca configurado. En escritorio, un `.env` de runtime
//! (cwd o carpeta de datos) puede sobrescribirlos sin recompilar; la key
//! también se puede cargar desde el modal de Configuración, que la persiste
//! en `localStorage`.
//!
//! La app **no** habla con Deepgram: conecta al proxy de gestion.ar con un
//! ticket efímero. La `DEEPGRAM_API_KEY`, el prompt y las tools viven en el
//! servidor (ADR-034/ADR-035).

use std::path::PathBuf;

/// Backend embebido en build (`BACKEND_URL` del `.env`).
const BACKEND_URL_BUILD: &str = env!("DEEPGRAM_BACKEND_URL");
/// Key embebida en build (`TENANT_KEY` del `.env`); vacía = se pide en la UI.
const TENANT_KEY_BUILD: &str = env!("DEEPGRAM_TENANT_KEY");

/// URL base del backend, sin barra final.
pub fn backend_url() -> String {
    let url = runtime_var("BACKEND_URL").unwrap_or_else(|| BACKEND_URL_BUILD.to_string());
    url.trim().trim_end_matches('/').to_string()
}

/// API key del tenant (`tk_live_…`), o cadena vacía si no está configurada.
pub fn tenant_key() -> String {
    runtime_var("TENANT_KEY")
        .unwrap_or_else(|| TENANT_KEY_BUILD.to_string())
        .trim()
        .to_string()
}

/// `POST /api/public/voice/session` — bootstrap con `Authorization: Bearer`.
pub fn session_url() -> String {
    format!("{}/api/public/voice/session", backend_url())
}

/// `POST /api/public/voice/text` — turno escrito del canal de voz (modo Texto).
/// Corre el mismo agente sin abrir sesión de Deepgram (ADR-040).
pub fn texto_url() -> String {
    format!("{}/api/public/voice/text", backend_url())
}

/// URL del WebSocket del proxy a partir del `ws_path` que devolvió el backend.
pub fn ws_url(ws_path: &str, ticket: &str, device_id: &str) -> String {
    let base = backend_url();
    let ws_base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base
    };
    let path = if ws_path.starts_with('/') {
        ws_path.to_string()
    } else {
        format!("/{ws_path}")
    };
    format!(
        "{}{}?ticket={}&device_id={}",
        ws_base.trim_end_matches('/'),
        path,
        encode(ticket),
        encode(device_id)
    )
}

/// Encoding mínimo de query string (RFC 3986 unreserved).
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `.env` de runtime: el del directorio actual o el de la carpeta de datos.
fn env_file() -> Option<PathBuf> {
    let actual = PathBuf::from(".env");
    if actual.exists() {
        return Some(actual);
    }
    let datos = data_dir().join(".env");
    if datos.exists() {
        return Some(datos);
    }
    None
}

/// `$XDG_DATA_HOME/deepgram-chat` o `~/.local/share/deepgram-chat`.
fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("deepgram-chat")
}

fn runtime_var(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(env_file()?).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_query() {
        assert_eq!(encode("a.b-c_d~e"), "a.b-c_d~e");
        assert_eq!(encode("a&b c"), "a%26b%20c");
    }
}
