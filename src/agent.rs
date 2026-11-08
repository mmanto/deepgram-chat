//! Cliente del proxy de voz de gestion.ar (`/ws/voice/channel/{id}`).
//!
//! La app ya **no** habla con Deepgram ni envía `Settings`: el backend es
//! dueño de la prompt, las tools y la API key. El protocolo hacia la UI es el
//! mismo que antes (audio PCM 16 kHz de ida, 24 kHz de vuelta y los eventos
//! de turno), más `meta` con el nombre del bot/tenant.
//!
//! La tarea en segundo plano posee el socket; la UI se comunica con ella por
//! dos canales: `eventos` (std mpsc) y `cmd` (tokio mpsc).

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::mpsc as std_mpsc;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

/// Eventos que la tarea del socket emite hacia la UI.
pub enum AgentEvent {
    /// Lo que el usuario dijo (transcripción del agente).
    TextoUsuario(String),
    /// Respuesta de texto del agente.
    TextoAgente(String),
    /// Trozo de voz del agente (PCM linear16 a 24 kHz).
    Audio(Vec<u8>),
    /// El agente terminó de enviar la voz del turno.
    AudioTerminado,
    /// La configuración del servidor quedó aplicada: ya se puede mandar audio.
    Listo,
    /// El usuario empezó a hablar (barge-in: cortar la voz programada).
    UsuarioHablo,
    /// El agente está pensando la respuesta.
    AgentePensando,
    /// El agente empezó a hablar.
    AgenteHablando,
    /// Identidad del tenant/bot que resolvió la API key.
    Meta {
        bot_name: String,
        tenant_name: String,
        greeting: String,
    },
    /// Mensaje de error del servicio.
    Error(String),
    /// Aviso no fatal del servicio.
    Aviso(String),
    /// El socket se cerró (por el servidor o por un error de transporte).
    Cerrado,
}

/// Órdenes que la UI envía a la tarea del socket.
pub enum AgentCommand {
    /// Audio PCM linear16 16 kHz mono.
    Audio(Vec<u8>),
}

/// Handle del agente conectado. Se guarda en un `Signal<Option<Agente>>`.
pub struct Agente {
    pub eventos: std_mpsc::Receiver<AgentEvent>,
    cmd: tokio_mpsc::UnboundedSender<AgentCommand>,
}

impl Agente {
    /// Envía un trozo de micrófono (PCM linear16 16 kHz mono).
    pub fn enviar_audio(&self, pcm: Vec<u8>) {
        let _ = self.cmd.send(AgentCommand::Audio(pcm));
    }

}

/// Conecta al proxy con el ticket ya incluido en `url`.
///
/// No se envía `Settings`: el proxy lo hace al recibir el `Welcome` de
/// Deepgram. Antes de `SettingsApplied` el audio se descarta.
pub async fn conectar(url: &str) -> Result<Agente, String> {
    // Asegura el proveedor TLS `ring` a nivel de proceso (más portable en Android).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let req = url.into_client_request().map_err(|e| e.to_string())?;
    let (ws, _resp) = connect_async(req).await.map_err(|e| format!("{e}"))?;
    let (mut write, mut read) = ws.split();

    let (ev_tx, ev_rx) = std_mpsc::channel::<AgentEvent>();
    let (cmd_tx, mut cmd_rx) = tokio_mpsc::unbounded_channel::<AgentCommand>();

    tokio::spawn(async move {
        // `SettingsApplied` recibido: la sesión con el agente existe y admite
        // audio. Antes de eso se descarta en vez de romper el socket.
        let mut sesion_lista = false;
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(AgentCommand::Audio(pcm)) => {
                            if sesion_lista && write.send(Message::binary(pcm)).await.is_err() {
                                let _ = ev_tx.send(AgentEvent::Cerrado);
                                break;
                            }
                        }
                        // Canal de comandos caído (la UI soltó el handle).
                        None => break,
                    }
                }
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Binary(data))) => {
                            let _ = ev_tx.send(AgentEvent::Audio(data.to_vec()));
                        }
                        Some(Ok(Message::Text(txt))) => {
                            let Ok(v) = serde_json::from_str::<Value>(&txt) else {
                                continue;
                            };
                            match v.get("type").and_then(Value::as_str) {
                                Some("SettingsApplied") => {
                                    sesion_lista = true;
                                    let _ = ev_tx.send(AgentEvent::Listo);
                                }
                                _ => manejar_json(&v, &ev_tx),
                            }
                        }
                        // Los ping/pong los contesta tungstenite: no cierran la sesión.
                        Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Frame(_))) => {}
                        // Cierre, error de transporte o desconexión del servidor.
                        _ => {
                            let _ = ev_tx.send(AgentEvent::Cerrado);
                            break;
                        }
                    }
                }
            }
        }
        let _ = write.close().await;
    });

    Ok(Agente {
        eventos: ev_rx,
        cmd: cmd_tx,
    })
}

/// Antepone el `code` del protocolo (p. ej. `THINK_REQUEST_FAILED`) para que
/// el aviso sea identificable en la UI y en el log.
fn con_codigo(v: &Value, texto: &str) -> String {
    match v.get("code").and_then(Value::as_str) {
        Some(codigo) if !codigo.is_empty() => format!("{codigo}: {texto}"),
        _ => texto.to_string(),
    }
}

fn manejar_json(v: &Value, ev_tx: &std_mpsc::Sender<AgentEvent>) {
    let tipo = v.get("type").and_then(Value::as_str).unwrap_or("");
    match tipo {
        // Identidad que resolvió el proxy a partir de la API key del tenant.
        "meta" => {
            let _ = ev_tx.send(AgentEvent::Meta {
                bot_name: v
                    .get("bot_name")
                    .and_then(Value::as_str)
                    .unwrap_or("Agente")
                    .to_string(),
                tenant_name: v
                    .get("tenant_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                greeting: v
                    .get("greeting")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
        // Turno de conversación: `role` = "user" (STT) o "assistant" (LLM).
        "ConversationText" => {
            let texto = v.get("content").and_then(Value::as_str).unwrap_or("");
            if texto.is_empty() {
                return;
            }
            match v.get("role").and_then(Value::as_str) {
                Some("user") => {
                    let _ = ev_tx.send(AgentEvent::TextoUsuario(texto.to_string()));
                }
                Some("assistant") => {
                    let _ = ev_tx.send(AgentEvent::TextoAgente(texto.to_string()));
                }
                _ => {}
            }
        }
        // Barge-in: la UI corta la voz que ya tenía programada.
        "UserStartedSpeaking" => {
            let _ = ev_tx.send(AgentEvent::UsuarioHablo);
        }
        "AgentThinking" => {
            let _ = ev_tx.send(AgentEvent::AgentePensando);
        }
        "AgentStartedSpeaking" => {
            let _ = ev_tx.send(AgentEvent::AgenteHablando);
        }
        "AgentAudioDone" => {
            let _ = ev_tx.send(AgentEvent::AudioTerminado);
        }
        // El proxy propio envía `"error"` en minúsculas; Deepgram, `"Error"`.
        "Error" | "error" => {
            let e = v
                .get("description")
                .or(v.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("error desconocido");
            let _ = ev_tx.send(AgentEvent::Error(con_codigo(v, e)));
        }
        "Warning" | "warning" => {
            let w = v
                .get("description")
                .or(v.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("aviso desconocido");
            let _ = ev_tx.send(AgentEvent::Aviso(con_codigo(v, w)));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_error_minusculas_del_proxy_llega_a_la_ui() {
        // Mensaje exacto de `voice_router.py` cuando se cae el upstream.
        let v: Value = serde_json::from_str(
            r#"{"type":"error","message":"Se cortó la conexión con el agente de voz"}"#,
        )
        .unwrap();
        let (tx, rx) = std_mpsc::channel();
        manejar_json(&v, &tx);
        match rx.try_recv() {
            Ok(AgentEvent::Error(msg)) => {
                assert_eq!(msg, "Se cortó la conexión con el agente de voz")
            }
            Ok(_) => panic!("evento inesperado"),
            Err(e) => panic!("sin evento: {e}"),
        }
    }
}
