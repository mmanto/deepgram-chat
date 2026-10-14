//! Cliente para las APIs REST de Deepgram:
//! - /v1/listen  (Speech-to-Text, modelos Nova)
//! - /v1/speak   (Text-to-Speech, voces Aura)

use base64::Engine as _;
use reqwest::Client;
use serde::Deserialize;

const BASE: &str = "https://api.deepgram.com/v1";

/// Voz Aura en español por defecto (la usan el TTS REST y el Voice Agent).
///
/// El catálogo de Aura 2 no tiene `aura-2-thalia-es` (thalia solo existe en
/// inglés): las voces `-es` son, entre otras, `aura-2-celeste-es`,
/// `aura-2-estrella-es`, `aura-2-nestor-es` y `aura-2-sirio-es`.
/// https://developers.deepgram.com/docs/tts-models
pub const VOZ_ES: &str = "aura-2-celeste-es";

#[derive(Deserialize)]
struct ListenResponse {
    results: Results,
}
#[derive(Deserialize)]
struct Results {
    channels: Vec<Channel>,
}
#[derive(Deserialize)]
struct Channel {
    alternatives: Vec<Alternative>,
}
#[derive(Deserialize)]
struct Alternative {
    transcript: String,
}

/// Transcribe audio intentando varios Content-Type comunes en Android
/// (el webview no siempre expone el MIME real del archivo grabado).
pub async fn transcribir(api_key: &str, audio: Vec<u8>) -> Result<String, String> {
    for mime in ["audio/webm", "audio/mp4", "audio/mpeg", "audio/wav"] {
        if let Ok(texto) = intentar_transcripcion(api_key, &audio, mime).await {
            if !texto.trim().is_empty() {
                return Ok(texto);
            }
        }
    }
    Err("Deepgram no pudo transcribir el audio. Verifica tu API key.".into())
}

/// Transcribe un WAV linear16 mono (p. ej. la nota de voz grabada dentro de
/// la app, que no pasa por el grabador del sistema).
pub async fn transcribir_wav(api_key: &str, wav: Vec<u8>) -> Result<String, String> {
    let texto = intentar_transcripcion(api_key, &wav, "audio/wav").await?;
    if texto.trim().is_empty() {
        return Err("Deepgram no devolvió transcripción (¿grabación en silencio?).".into());
    }
    Ok(texto)
}

async fn intentar_transcripcion(api_key: &str, audio: &[u8], mime: &str) -> Result<String, String> {
    let url = format!("{BASE}/listen?model=nova-3&language=es&punctuate=true&smart_format=true");
    let res = Client::new()
        .post(&url)
        .header("Authorization", format!("Token {api_key}"))
        .header("Content-Type", mime)
        .body(audio.to_vec())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status()));
    }
    let data: ListenResponse = res.json().await.map_err(|e| e.to_string())?;
    data.results
        .channels
        .first()
        .and_then(|c| c.alternatives.first())
        .map(|a| a.transcript.clone())
        .ok_or_else(|| "Sin transcripción".to_string())
}

/// Sintetiza texto a audio MP3 con una voz Aura (`VOZ_ES` por defecto).
pub async fn hablar(api_key: &str, texto: &str, voz: &str) -> Result<Vec<u8>, String> {
    let url = format!("{BASE}/speak?model={voz}");
    let res = Client::new()
        .post(&url)
        .header("Authorization", format!("Token {api_key}"))
        .json(&serde_json::json!({ "text": texto }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status()));
    }
    Ok(res.bytes().await.map_err(|e| e.to_string())?.to_vec())
}

/// Convierte bytes MP3 en un data URL reproducible en el webview.
pub fn data_url_mpeg(bytes: &[u8]) -> String {
    format!(
        "data:audio/mpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Envuelve PCM linear16 mono en una cabecera WAV (RIFF).
///
/// Lo usan tanto la reproducción en el webview como el envío a `/v1/listen`,
/// de modo que la voz del agente (24 kHz) y la nota grabada (16 kHz) comparten
/// el mismo formato de envoltura.
pub fn pcm16_a_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let mut wav = Vec::with_capacity(44 + pcm.len());
    let n = pcm.len() as u32;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + n).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // tamaño del chunk fmt
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM sin comprimir
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate (16 bits)
    wav.extend_from_slice(&2u16.to_le_bytes()); // alineación de bloque
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits por muestra
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&n.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// Data URL de un PCM linear16 reproducible con un `<audio>` del webview.
pub fn data_url_wav(pcm: &[u8], sample_rate: u32) -> String {
    format!(
        "data:audio/wav;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(pcm16_a_wav(pcm, sample_rate))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;

    #[test]
    fn wav_es_lineal16_mono_a_la_frecuencia_pedida() {
        let pcm = vec![7u8; 480]; // 240 muestras de 16 bits
        let url = data_url_wav(&pcm, 24000);
        let wav = B64
            .decode(url.strip_prefix("data:audio/wav;base64,").unwrap())
            .unwrap();

        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1); // PCM
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1); // mono
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24000);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 24000 * 2);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16); // bits
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 480);
        assert_eq!(&wav[44..], &pcm[..]);
    }
}
