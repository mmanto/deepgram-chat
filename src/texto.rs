//! Turno escrito del canal de voz (modo Texto), sin Deepgram (ADR-040).
//!
//! El WebSocket de `agent.rs` existe para transportar PCM hacia Deepgram Agent.
//! El texto va por `POST /api/public/voice/text`: el backend corre el mismo
//! agente del bot (prompt, tools, RAG e historial del `device_id`) y devuelve
//! la respuesta completa. En modo Texto el cliente no abre ningún socket, así
//! que no hay minutos de agente ni TTS que descartar.

use std::time::Duration;

use crate::causas;

/// Una respuesta del endpoint de texto; `reply` nunca viene vacía.
pub struct Respuesta {
    pub reply: String,
}

/// Envía un mensaje escrito y espera la respuesta del agente.
pub async fn enviar(
    url: &str,
    key: &str,
    device: &str,
    contenido: &str,
) -> Result<Respuesta, String> {
    // Un turno con tools + LLM puede tardar; el bootstrap HTTP usa 12 s, pero
    // acá se espera la generación completa.
    let cliente = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| format!("No se pudo crear el cliente HTTP: {e}"))?;
    let response = cliente
        .post(url)
        .bearer_auth(key)
        .json(&serde_json::json!({ "device_id": device, "content": contenido }))
        .send()
        .await
        .map_err(|e| format!("No se pudo contactar el backend: {}", causas(&e)))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let body: String = body.chars().take(300).collect();
        return Err(format!("Backend HTTP {status}: {body}"));
    }

    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Respuesta inválida del backend: {e}"))?;
    let reply = value["reply"].as_str().unwrap_or("").trim().to_string();
    if reply.is_empty() {
        return Err("El agente no devolvió texto".to_string());
    }
    Ok(Respuesta { reply })
}
