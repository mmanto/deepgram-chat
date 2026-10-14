//! Módulo del Deepgram Voice Agent (WebSocket).
//!
//! Conecta con `wss://agent.deepgram.com/v1/agent/converse`: envía audio
//! PCM (linear16, 16 kHz, mono) capturado con el micrófono y recibe:
//! - `ConversationText` con `role: "user"`       -> transcripción del usuario
//! - `ConversationText` con `role: "assistant"`  -> respuesta del agente
//! - tramas binarias + `AgentAudioDone` -> voz del agente (linear16, 24 kHz)
//!
//! La tarea en segundo plano posee el socket; la UI se comunica con ella
//! a través de dos canales: `eventos` (std mpsc) y `cmd` (tokio mpsc).

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::mpsc as std_mpsc;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::deepgram::VOZ_ES;

const AGENT_URL: &str = "wss://agent.deepgram.com/v1/agent/converse";
const SR_ENTRADA: u32 = 16000; // PCM que enviamos (el JS graba a 16 kHz)
const SR_SALIDA: u32 = 24000;  // PCM que devuelve el agente (Aura)
/// Silencio que se añade al final de cada turno enviado. El agente cierra el
/// turno por detección de voz (VAD); si el audio termina de golpe la
/// transcripción puede quedarse sin `speech_final` y el agente no responde.
const SILENCIO_FINAL_MS: u32 = 700;

/// Eventos que la tarea del socket emite hacia la UI.
pub enum AgentEvent {
    /// Lo que el usuario dijo (STT de Deepgram).
    TextoUsuario(String),
    /// Respuesta de texto del agente (su etapa "think"/LLM).
    TextoAgente(String),
    /// Un turno completo de voz del agente (PCM linear16 a 24 kHz).
    AudioTurno(Vec<u8>),
    /// Mensaje de error del servicio.
    Error(String),
    /// Aviso no fatal del servicio (p. ej. `SPEAK_REQUEST_FAILED`).
    Aviso(String),
    /// El socket se cerró (por el servidor o por `cerrar()`).
    Cerrado,
}

/// Órdenes que la UI envía a la tarea del socket.
pub enum AgentCommand {
    /// Audio PCM linear16 16 kHz mono.
    Audio(Vec<u8>),
    Cerrar,
}

/// Handle del agente conectado. Se guarda en un `Signal<Option<Agente>>`.
pub struct Agente {
    pub eventos: std_mpsc::Receiver<AgentEvent>,
    cmd: tokio_mpsc::UnboundedSender<AgentCommand>,
}

impl Agente {
    /// Envía un turno de audio al agente.
    ///
    /// Añade un tramo de silencio al final: el agente decide el fin de turno
    /// por VAD, y sin silencio final el turno puede quedar abierto.
    pub fn enviar_audio(&self, mut pcm: Vec<u8>) {
        let relleno = (SR_ENTRADA / 1000 * 2 * SILENCIO_FINAL_MS) as usize; // 16 bits por muestra
        pcm.resize(pcm.len() + relleno, 0);
        let _ = self.cmd.send(AgentCommand::Audio(pcm));
    }

    /// Cierra la conexión (la tarea terminará emitiendo `Cerrado`).
    pub fn cerrar(&self) {
        let _ = self.cmd.send(AgentCommand::Cerrar);
    }
}

/// Proveedor de LLM elegible en los ajustes (uno por usuario).
///
/// El Agent API solo admite `think.provider.type` ∈ {open_ai, anthropic,
/// google, groq, aws_bedrock}; cualquier servicio compatible con *chat
/// completions* de OpenAI (DeepSeek, gateways…) se declara como `open_ai` con
/// `endpoint.url` apuntando al servicio:
/// https://developers.deepgram.com/docs/voice-agent-llm-models#passing-a-custom-byo-llm-through-a-cloud-provider
pub struct Proveedor {
    /// Identificador que se guarda en los ajustes.
    pub id: &'static str,
    /// Nombre que se muestra en el selector.
    pub nombre: &'static str,
    /// Valor de `think.provider.type`.
    pub tipo: &'static str,
    /// `think.endpoint.url` por defecto (vacío = LLM gestionado por Deepgram).
    pub url: &'static str,
    /// Cabecera de autenticación que espera el endpoint.
    pub cabecera: &'static str,
    /// Modelo por defecto para ese proveedor.
    pub modelo: &'static str,
}

/// Modelo de LLM según el proveedor, con sus valores por defecto.
pub const PROVEEDORES: &[Proveedor] = &[
    Proveedor {
        id: "deepseek",
        nombre: "DeepSeek",
        tipo: "open_ai",
        url: "https://api.deepseek.com/v1/chat/completions",
        cabecera: "authorization",
        modelo: "deepseek-chat",
    },
    Proveedor {
        id: "gestionado",
        nombre: "Deepgram (gestionado)",
        tipo: "open_ai",
        url: "",
        cabecera: "authorization",
        modelo: "gpt-4o-mini",
    },
    Proveedor {
        id: "openai",
        nombre: "OpenAI",
        tipo: "open_ai",
        url: "https://api.openai.com/v1/chat/completions",
        cabecera: "authorization",
        modelo: "gpt-4o-mini",
    },
    Proveedor {
        id: "anthropic",
        nombre: "Anthropic",
        tipo: "anthropic",
        url: "https://api.anthropic.com/v1/messages",
        cabecera: "x-api-key",
        modelo: "claude-sonnet-4-5",
    },
    Proveedor {
        id: "groq",
        nombre: "Groq",
        tipo: "groq",
        url: "https://api.groq.com/openai/v1/chat/completions",
        cabecera: "authorization",
        modelo: "llama-3.3-70b-versatile",
    },
    Proveedor {
        id: "personalizado",
        nombre: "Personalizado (compatible OpenAI)",
        tipo: "open_ai",
        url: "",
        cabecera: "authorization",
        modelo: "",
    },
];

impl Proveedor {
    /// Busca un proveedor por su id. Los ids desconocidos caen en
    /// `personalizado`: el tipo y la cabecera genéricos, de modo que los
    /// valores guardados (url, modelo, key) siguen mandando.
    pub fn buscar(id: &str) -> &'static Proveedor {
        PROVEEDORES
            .iter()
            .find(|p| p.id == id)
            .or_else(|| PROVEEDORES.iter().find(|p| p.id == "personalizado"))
            .expect("PROVEEDORES incluye 'personalizado'")
    }

    /// ¿Necesita endpoint y API key propias? Solo el LLM gestionado por
    /// Deepgram funciona sin credenciales del usuario.
    pub fn requiere_key(&self) -> bool {
        self.id != "gestionado"
    }

    /// ¿Trae endpoint por defecto? `personalizado` lo escribe el usuario.
    pub fn tiene_endpoint(&self) -> bool {
        !self.url.is_empty()
    }

    /// Nombre y valor de la cabecera de autenticación del endpoint.
    fn cabecera_auth(&self, key: &str) -> (&'static str, String) {
        match self.cabecera {
            "x-api-key" => ("x-api-key", key.to_string()),
            _ => ("authorization", format!("Bearer {key}")),
        }
    }
}

/// Ajustes del LLM elegidos por el usuario (se guardan en el dispositivo).
pub struct AjustesLlm<'a> {
    /// Id del proveedor (`Proveedor::id`).
    pub proveedor: &'a str,
    /// Modelo; si va vacío se usa el del proveedor.
    pub modelo: &'a str,
    /// API key del proveedor (vacía = sin cabecera de autenticación).
    pub key: &'a str,
    /// URL del endpoint; vacía = LLM gestionado por Deepgram.
    pub url: &'a str,
}

/// Ajustes del LLM recuperados del almacenamiento del dispositivo.
pub struct AjustesGuardados {
    pub proveedor: String,
    pub url: String,
    pub modelo: String,
}

/// Interpreta lo que devolvió `localStorage` al arrancar.
///
/// Devuelve `None` cuando no hay ajustes guardados: `localStorage.getItem`
/// devuelve `''` (no `null`) para una clave inexistente, así que sin esta
/// comprobación se sobrescribirían los valores por defecto del preset (por
/// ejemplo, dejarían el endpoint de DeepSeek vacío).
pub fn ajustes_llm_guardados(almacen: &Value) -> Option<AjustesGuardados> {
    let proveedor = almacen.get("proveedor")?.as_str()?;
    if proveedor.is_empty() {
        return None;
    }
    Some(AjustesGuardados {
        proveedor: proveedor.to_string(),
        // La URL vacía es un valor legítimo (LLM gestionado por Deepgram).
        url: almacen
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        modelo: almacen
            .get("modelo")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

impl AjustesLlm<'_> {
    fn proveedor(&self) -> &'static Proveedor {
        Proveedor::buscar(self.proveedor)
    }

    fn modelo(&self) -> &str {
        let proveedor = self.proveedor();
        if self.modelo.trim().is_empty() {
            proveedor.modelo
        } else {
            self.modelo
        }
    }

    /// `think` del mensaje `Settings`: proveedor, modelo, prompt y, cuando el
    /// usuario aporta su endpoint, credenciales para él.
    fn think(&self, instrucciones: &str) -> Value {
        let proveedor = self.proveedor();
        let mut provider = json!({ "type": proveedor.tipo });
        // Sin modelo se omite el campo (el esquema solo exige `type`): así el
        // endpoint usa su modelo por defecto en vez de recibir una cadena vacía.
        if !self.modelo().is_empty() {
            provider["model"] = json!(self.modelo());
        }
        let mut think = json!({ "provider": provider, "prompt": instrucciones });
        if !self.url.trim().is_empty() {
            let mut cabeceras = serde_json::Map::new();
            if !self.key.trim().is_empty() {
                let (nombre, valor) = proveedor.cabecera_auth(self.key.trim());
                cabeceras.insert(nombre.to_string(), json!(valor));
            }
            think["endpoint"] = json!({
                "url": self.url.trim(),
                "headers": Value::Object(cabeceras),
            });
        }
        think
    }
}

/// Construye el mensaje `Settings` que abre la conversación con el agente.
///
/// Esquema (AsyncAPI): https://developers.deepgram.com/reference/voice-agent/voice-agent
/// - `type` debe ser exactamente `"Settings"`.
/// - `listen`/`think`/`speak` requieren un objeto `provider` con `type`.
/// - `audio.output.container` solo admite `none`, `wav` u `ogg`.
/// - El prompt del sistema va en `think.prompt` (no en `instructions`).
pub fn configuracion(instrucciones: &str, llm: &AjustesLlm<'_>) -> Value {
    json!({
        "type": "Settings",
        "audio": {
            "input":  { "encoding": "linear16", "sample_rate": SR_ENTRADA },
            "output": { "encoding": "linear16", "sample_rate": SR_SALIDA, "container": "none" }
        },
        "agent": {
            "listen": { "provider": {
                "type": "deepgram",
                "model": "nova-3",
                "language": "es",
                "smart_format": true
            } },
            "think": llm.think(instrucciones),
            "speak": { "provider": { "type": "deepgram", "model": VOZ_ES } }
        }
    })
}

/// Conecta con el Agent API y arranca la tarea de lectura/escritura.
///
/// - `dg_key`: API key de Deepgram (obligatoria).
/// - `instrucciones`: system prompt del agente.
/// - `llm`: proveedor/modelo/key/endpoint del LLM elegidos por el usuario.
pub async fn conectar(
    dg_key: &str,
    instrucciones: &str,
    llm: &AjustesLlm<'_>,
) -> Result<Agente, String> {
    // Asegura el proveedor TLS `ring` a nivel de proceso (más portable en Android).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut req = AGENT_URL.into_client_request().map_err(|e| e.to_string())?;
    let auth: HeaderValue = format!("Token {dg_key}")
        .try_into()
        .map_err(|_| "API key de Deepgram inválida".to_string())?;
    req.headers_mut().insert("Authorization", auth);

    let (ws, _resp) = connect_async(req).await.map_err(|e| format!("{e}"))?;
    let (mut write, mut read) = ws.split();

    let (ev_tx, ev_rx) = std_mpsc::channel::<AgentEvent>();
    let (cmd_tx, mut cmd_rx) = tokio_mpsc::unbounded_channel::<AgentCommand>();

    // --- Mensaje de configuración de la conversación ---
    let settings = configuracion(instrucciones, llm);
    write
        .send(Message::text(settings.to_string()))
        .await
        .map_err(|e| format!("No se pudo enviar la configuración: {e}"))?;

    // --- Tarea en segundo plano: multiplexa socket <-> canales ---
    tokio::spawn(async move {
        let mut pcm_agente: Vec<u8> = Vec::new();
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(AgentCommand::Audio(pcm)) => {
                            if write.send(Message::binary(pcm)).await.is_err() {
                                break;
                            }
                        }
                        // Cierre explícito o canal de comandos caído.
                        Some(AgentCommand::Cerrar) | None => break,
                    }
                }
                msg = read.next() => {
                    match msg {
                        // Voz del agente: acumular hasta el fin del turno.
                        Some(Ok(Message::Binary(data))) => pcm_agente.extend_from_slice(&data),
                        Some(Ok(Message::Text(txt))) => {
                            if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                                manejar_json(&v, &ev_tx, &mut pcm_agente);
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

fn manejar_json(v: &Value, ev_tx: &std_mpsc::Sender<AgentEvent>, pcm_agente: &mut Vec<u8>) {
    let tipo = v.get("type").and_then(Value::as_str).unwrap_or("");
    match tipo {
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
        "AgentAudioDone" => {
            if !pcm_agente.is_empty() {
                let _ = ev_tx.send(AgentEvent::AudioTurno(std::mem::take(pcm_agente)));
            }
        }
        "Error" => {
            let e = v
                .get("description")
                .or(v.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("error desconocido");
            let _ = ev_tx.send(AgentEvent::Error(e.to_string()));
        }
        "Warning" => {
            let w = v
                .get("description")
                .or(v.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("aviso desconocido");
            let _ = ev_tx.send(AgentEvent::Aviso(w.to_string()));
        }
        // "SettingsApplied", "AgentThinking", "UserStartedSpeaking", etc.: se ignoran.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    //! El contrato con el Voice Agent vive en el JSON que enviamos y en los
    //! `type`/`role` que interpretamos: si se rompe, la conversación se queda
    //! muda sin dar error. https://developers.deepgram.com/reference/voice-agent/voice-agent
    use super::*;

    #[test]
    fn settings_cumple_el_esquema_del_agente() {
        let llm = AjustesLlm {
            proveedor: "gestionado",
            modelo: "gpt-4o-mini",
            key: "",
            url: "",
        };
        let c = configuracion("sé breve", &llm);
        assert_eq!(c["type"], "Settings");
        assert_eq!(c["audio"]["input"]["encoding"], "linear16");
        assert_eq!(c["audio"]["input"]["sample_rate"], SR_ENTRADA);
        assert_eq!(c["audio"]["output"]["sample_rate"], SR_SALIDA);
        // El esquema solo admite none/wav/ogg como contenedor de salida.
        assert_eq!(c["audio"]["output"]["container"], "none");
        assert_eq!(c["agent"]["listen"]["provider"]["type"], "deepgram");
        assert_eq!(c["agent"]["listen"]["provider"]["model"], "nova-3");
        assert_eq!(c["agent"]["listen"]["provider"]["language"], "es");
        assert_eq!(c["agent"]["think"]["provider"]["type"], "open_ai");
        assert_eq!(c["agent"]["think"]["provider"]["model"], "gpt-4o-mini");
        assert_eq!(c["agent"]["think"]["prompt"], "sé breve");
        assert_eq!(c["agent"]["speak"]["provider"]["type"], "deepgram");
        assert_eq!(c["agent"]["speak"]["provider"]["model"], VOZ_ES);
        // Sin URL propia el LLM lo gestiona Deepgram: no hay endpoint.
        assert!(c["agent"]["think"]["endpoint"].is_null());
    }

    #[test]
    fn settings_deepseek_usa_endpoint_compatible_openai() {
        let preset = Proveedor::buscar("deepseek");
        let llm = AjustesLlm {
            proveedor: preset.id,
            modelo: preset.modelo,
            key: "sk-deepseek",
            url: preset.url,
        };
        let think = &configuracion("x", &llm)["agent"]["think"];

        // DeepSeek es compatible con chat completions ⇒ provider.type = open_ai.
        assert_eq!(think["provider"]["type"], "open_ai");
        assert_eq!(think["provider"]["model"], "deepseek-chat");
        assert_eq!(
            think["endpoint"]["url"],
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            think["endpoint"]["headers"]["authorization"],
            "Bearer sk-deepseek"
        );
    }

    #[test]
    fn settings_anthropic_autentica_con_x_api_key() {
        let preset = Proveedor::buscar("anthropic");
        let llm = AjustesLlm {
            proveedor: preset.id,
            modelo: preset.modelo,
            key: "sk-ant",
            url: preset.url,
        };
        let think = &configuracion("x", &llm)["agent"]["think"];
        assert_eq!(think["provider"]["type"], "anthropic");
        assert_eq!(think["endpoint"]["headers"]["x-api-key"], "sk-ant");
        assert!(think["endpoint"]["headers"]["authorization"].is_null());
    }

    #[test]
    fn settings_respeta_el_modelo_y_la_url_del_usuario() {
        // Endpoint propio arbitrario (gateway compatible OpenAI) y modelo vacío
        // ⇒ se omite `model` (el esquema solo exige `type`) en vez de mandar "".
        let llm = AjustesLlm {
            proveedor: "personalizado",
            modelo: "  ",
            key: "k",
            url: "https://gateway.interno/v1/chat/completions",
        };
        let think = &configuracion("x", &llm)["agent"]["think"];
        assert_eq!(think["provider"]["type"], "open_ai");
        assert!(think["provider"]["model"].is_null());
        assert_eq!(
            think["endpoint"]["url"],
            "https://gateway.interno/v1/chat/completions"
        );

        // Un id desconocido (ajustes guardados por una versión anterior) no
        // rompe: cae en el tipo/cabecera genéricos y respeta los valores del
        // usuario.
        let llm = AjustesLlm {
            proveedor: "proveedor-que-ya-no-existe",
            modelo: "mi-modelo",
            key: "k",
            url: "https://otro/v1/chat/completions",
        };
        let think = &configuracion("x", &llm)["agent"]["think"];
        assert_eq!(think["provider"]["type"], "open_ai");
        assert_eq!(think["provider"]["model"], "mi-modelo");
        assert_eq!(think["endpoint"]["headers"]["authorization"], "Bearer k");
    }

    #[test]
    fn settings_sin_key_omite_la_cabecera_de_autenticacion() {
        let llm = AjustesLlm {
            proveedor: "deepseek",
            modelo: "deepseek-reasoner",
            key: "",
            url: Proveedor::buscar("deepseek").url,
        };
        let endpoint = &configuracion("x", &llm)["agent"]["think"]["endpoint"];
        assert_eq!(endpoint["url"], "https://api.deepseek.com/v1/chat/completions");
        assert_eq!(endpoint["headers"], json!({}));
    }

    #[test]
    fn presets_de_proveedor_son_coherentes() {
        const TIPOS: [&str; 5] = ["open_ai", "anthropic", "google", "groq", "aws_bedrock"];
        let mut ids = Vec::new();
        for p in PROVEEDORES {
            assert!(!ids.contains(&p.id), "id duplicado: {}", p.id);
            ids.push(p.id);
            assert!(!p.nombre.is_empty(), "{} sin nombre", p.id);
            assert!(TIPOS.contains(&p.tipo), "{}: provider.type inválido", p.id);
            // Un preset con endpoint debe traer url https y modelo por defecto;
            // sin endpoint (gestionado, personalizado) los pone el usuario.
            assert!(p.url.is_empty() || p.url.starts_with("https://"), "{}: url no https", p.id);
            assert!(p.url.is_empty() || !p.modelo.is_empty(), "{} sin modelo", p.id);
        }

        let deepseek = Proveedor::buscar("deepseek");
        assert_eq!(deepseek.url, "https://api.deepseek.com/v1/chat/completions");
        assert_eq!(deepseek.modelo, "deepseek-chat");
        assert!(deepseek.requiere_key() && deepseek.tiene_endpoint());

        // Solo el gestionado funciona sin credenciales propias.
        assert!(!Proveedor::buscar("gestionado").requiere_key());
        assert!(!Proveedor::buscar("personalizado").tiene_endpoint());
        assert!(Proveedor::buscar("personalizado").requiere_key());
    }

    #[test]
    fn ajustes_guardados_solo_se_aplican_si_existen() {
        // Primera ejecución: localStorage devuelve '' en cada clave y los
        // valores por defecto del preset deben sobrevivir.
        let vacio = json!({"dg": "", "llm": "", "proveedor": "", "url": "", "modelo": ""});
        assert!(ajustes_llm_guardados(&vacio).is_none());
        // Claves ausentes (otra versión de la app) tampoco rompen.
        assert!(ajustes_llm_guardados(&json!({"dg": "x"})).is_none());

        // Con ajustes guardados mandan los valores del usuario, incluida una
        // URL vacía (LLM gestionado por Deepgram).
        let guardado = json!({
            "proveedor": "deepseek",
            "url": "https://api.deepseek.com/v1/chat/completions",
            "modelo": "deepseek-chat"
        });
        let g = ajustes_llm_guardados(&guardado).expect("hay proveedor guardado");
        assert_eq!(g.proveedor, "deepseek");
        assert_eq!(g.url, "https://api.deepseek.com/v1/chat/completions");
        assert_eq!(g.modelo, "deepseek-chat");

        let gestionado = json!({"proveedor": "gestionado", "url": "", "modelo": "gpt-4o-mini"});
        let g = ajustes_llm_guardados(&gestionado).expect("hay proveedor guardado");
        assert_eq!(g.proveedor, "gestionado");
        assert!(g.url.is_empty());
    }

    #[test]
    fn parser_traduce_los_eventos_del_protocolo() {
        let (tx, rx) = std_mpsc::channel();
        let mut pcm = Vec::new();

        manejar_json(
            &json!({"type": "ConversationText", "role": "user", "content": "hola"}),
            &tx,
            &mut pcm,
        );
        manejar_json(
            &json!({"type": "ConversationText", "role": "assistant", "content": "buenas"}),
            &tx,
            &mut pcm,
        );
        assert!(matches!(rx.recv(), Ok(AgentEvent::TextoUsuario(t)) if t == "hola"));
        assert!(matches!(rx.recv(), Ok(AgentEvent::TextoAgente(t)) if t == "buenas"));

        pcm.extend_from_slice(&[1, 2, 3, 4]);
        manejar_json(&json!({"type": "AgentAudioDone"}), &tx, &mut pcm);
        assert!(matches!(rx.recv(), Ok(AgentEvent::AudioTurno(a)) if a == vec![1, 2, 3, 4]));
        assert!(pcm.is_empty(), "el buffer se vacía al cerrar el turno de voz");

        manejar_json(
            &json!({"type": "Warning", "description": "SPEAK_REQUEST_FAILED", "code": "X"}),
            &tx,
            &mut pcm,
        );
        assert!(matches!(rx.recv(), Ok(AgentEvent::Aviso(a)) if a == "SPEAK_REQUEST_FAILED"));
    }

    #[test]
    fn enviar_audio_cierra_el_turno_con_silencio() {
        let (_ev_tx, ev_rx) = std_mpsc::channel();
        let (cmd_tx, mut cmd_rx) = tokio_mpsc::unbounded_channel();
        let agente = Agente {
            eventos: ev_rx,
            cmd: cmd_tx,
        };

        agente.enviar_audio(vec![1, 2]);

        let AgentCommand::Audio(pcm) = cmd_rx.try_recv().expect("turno enviado") else {
            panic!("se esperaba AgentCommand::Audio");
        };
        let relleno = (SR_ENTRADA / 1000 * 2 * SILENCIO_FINAL_MS) as usize;
        assert_eq!(pcm.len(), 2 + relleno);
        assert!(pcm[2..].iter().all(|b| *b == 0));
    }
}
