mod agent;
mod deepgram;

use base64::Engine as _;
use chrono::Local;
use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
struct Mensaje {
    id: usize,
    mio: bool,
    texto: String,
    hora: String,
    audio: Option<String>, // data URL reproducible
}

fn ahora() -> String {
    Local::now().format("%H:%M").to_string()
}

/// Helper para empujar un mensaje al chat.
fn push(
    mensajes: &mut Signal<Vec<Mensaje>>,
    ids: &mut Signal<usize>,
    mio: bool,
    texto: String,
    audio: Option<String>,
) {
    let id = *ids.read();
    ids.set(id + 1);
    mensajes.write().push(Mensaje {
        id,
        mio,
        texto,
        hora: ahora(),
        audio,
    });
}

fn main() {
    // En escritorio se fija la carpeta de datos del webview: `dx` nombra el
    // binario de desarrollo con un hash (`deepgram-chat-1fca35bc`) y WebKitGTK
    // deriva de ese nombre dónde guarda localStorage, así que sin fijarla los
    // ajustes de cada usuario se perdían en cada rebuild.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    dioxus::LaunchBuilder::desktop()
        .with_cfg(config_escritorio())
        .launch(App);

    // En Android/iOS la carpeta la gestiona el sistema (y sobrevive a las
    // actualizaciones de la app), así que no hace falta tocar nada.
    #[cfg(any(target_os = "android", target_os = "ios"))]
    dioxus::launch(App);
}

/// Configuración del renderer de escritorio con una carpeta de datos estable.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn config_escritorio() -> dioxus::desktop::Config {
    let config = dioxus::desktop::Config::new();
    #[cfg(unix)]
    let config = {
        let carpeta = carpeta_de_datos();
        if std::fs::create_dir_all(&carpeta).is_ok() {
            config.with_data_directory(carpeta)
        } else {
            config
        }
    };
    config
}

/// `$XDG_DATA_HOME/deepgram-chat` o `~/.local/share/deepgram-chat`.
#[cfg(all(unix, not(any(target_os = "android", target_os = "ios"))))]
fn carpeta_de_datos() -> std::path::PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("deepgram-chat")
}

const INSTRUCCIONES: &str = "Eres un asistente amable y breve dentro de una app de chat de voz. Responde siempre en español, en pocas frases, como en una conversación por WhatsApp.";

/// JS que graba del micrófono a PCM 16 kHz y define `window.dgStop()`.
/// Devuelve "ok" o "error: ..." (la key de micrófono ya es obligatoria).
///
/// Dioxus envuelve el script en una función `async`, así que el valor de
/// retorno debe devolverse con `return` (si no, `eval` falla con
/// "Failed to stringify result").
const REC_START: &str = r#"
try {
  if (!navigator.mediaDevices?.getUserMedia) return "error: getUserMedia no disponible";
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const ctx = new AudioContext({ sampleRate: 16000 });
  const entrada = ctx.createMediaStreamSource(stream);
  const proc = ctx.createScriptProcessor(4096, 1, 1);
  const mudo = ctx.createGain();
  mudo.gain.value = 0; // evita devolver el micrófono al altavoz (acople)
  const trozos = [];
  proc.onaudioprocess = (e) => trozos.push(new Float32Array(e.inputBuffer.getChannelData(0)));
  entrada.connect(proc);
  proc.connect(mudo);
  mudo.connect(ctx.destination);
  window.dgStop = () => {
    const rate = ctx.sampleRate;
    proc.onaudioprocess = null;
    stream.getTracks().forEach((t) => t.stop());
    entrada.disconnect();
    proc.disconnect();
    mudo.disconnect();
    ctx.close();
    let total = 0;
    for (const t of trozos) total += t.length;
    const muestras = new Float32Array(total);
    let off = 0;
    for (const t of trozos) { muestras.set(t, off); off += t.length; }
    // El dispositivo puede ignorar `sampleRate`: remuestreamos a 16 kHz.
    const paso = rate / 16000;
    const n = total === 0 ? 0 : Math.max(1, Math.round(total / paso));
    const pcm = new Int16Array(n);
    for (let i = 0; i < n; i++) {
      const x = i * paso;
      const i0 = Math.min(Math.floor(x), total - 1);
      const i1 = Math.min(i0 + 1, total - 1);
      const s = muestras[i0] + (muestras[i1] - muestras[i0]) * (x - i0);
      const v = Math.max(-1, Math.min(1, s));
      pcm[i] = v < 0 ? v * 0x8000 : v * 0x7fff;
    }
    const bytes = new Uint8Array(pcm.buffer);
    let bin = "";
    for (let i = 0; i < bytes.length; i += 32768) {
      bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 32768));
    }
    return btoa(bin);
  };
  return "ok";
} catch (err) { return "error: " + err; }
"#;

/// JS que detiene la grabación y devuelve el audio en base64.
const REC_STOP: &str = r#"return window.dgStop ? window.dgStop() : "";"#;

#[component]
fn App() -> Element {
    let mut mensajes = use_signal(Vec::<Mensaje>::new);
    let mut entrada = use_signal(String::new);
    let mut api_key = use_signal(String::new);
    let mut llm_key = use_signal(String::new);
    // Ajustes del LLM (por usuario): proveedor, endpoint y modelo. El
    // predeterminado es DeepSeek, que es compatible con la API de chat
    // completions de OpenAI y por eso entra por `endpoint.url`.
    let preset = agent::Proveedor::buscar("deepseek");
    let mut proveedor_llm = use_signal(|| preset.id.to_string());
    let mut llm_url = use_signal(|| preset.url.to_string());
    let mut modelo_llm = use_signal(|| preset.modelo.to_string());
    let mut ocupado = use_signal(|| false);
    let mut mostrar_ajustes = use_signal(|| false);
    let mut ids = use_signal(|| 0usize);
    let mut modo_agente = use_signal(|| false);
    let mut agente = use_signal(|| None::<agent::Agente>);
    let mut grabando = use_signal(|| false);
    let mut grabando_nota = use_signal(|| false);

    // Cargar ajustes guardados en el almacenamiento del webview.
    use_future(move || async move {
        let js = r#"return {
            dg: localStorage.getItem('dg_key') ?? '',
            llm: localStorage.getItem('llm_key') ?? '',
            proveedor: localStorage.getItem('llm_proveedor') ?? '',
            url: localStorage.getItem('llm_url') ?? '',
            modelo: localStorage.getItem('llm_modelo') ?? ''
        };"#;
        if let Ok(v) = dioxus::document::eval(js).await {
            if let Some(k) = v["dg"].as_str() {
                if !k.is_empty() {
                    api_key.set(k.to_string());
                }
            }
            if let Some(k) = v["llm"].as_str() {
                if !k.is_empty() {
                    llm_key.set(k.to_string());
                }
            }
            // Los ajustes del LLM se aplican solo si el usuario ya los guardó
            // alguna vez (hay proveedor); si no, se mantienen los del preset.
            if let Some(g) = agent::ajustes_llm_guardados(&v) {
                proveedor_llm.set(g.proveedor);
                llm_url.set(g.url);
                modelo_llm.set(g.modelo);
            }
        }
    });

    // Bomba de eventos del agente: drena el canal y actualiza la UI.
    use_future(move || async move {
        loop {
            let ev = {
                let g = agente.read();
                g.as_ref().and_then(|a| a.eventos.try_recv().ok())
            };
            match ev {
                Some(agent::AgentEvent::TextoUsuario(t)) => {
                    push(&mut mensajes, &mut ids, true, format!("🎤 {t}"), None);
                }
                Some(agent::AgentEvent::TextoAgente(t)) => {
                    push(&mut mensajes, &mut ids, false, t, None);
                }
                Some(agent::AgentEvent::AudioTurno(pcm)) => {
                    let url = deepgram::data_url_wav(&pcm, 24000);
                    push(&mut mensajes, &mut ids, false, "🔊 Voz del agente".to_string(), Some(url));
                }
                Some(agent::AgentEvent::Error(e)) => {
                    push(&mut mensajes, &mut ids, false, format!("⚠️ Agente: {e}"), None);
                }
                Some(agent::AgentEvent::Aviso(a)) => {
                    push(
                        &mut mensajes,
                        &mut ids,
                        false,
                        format!("⚠️ Aviso del agente: {a}"),
                        None,
                    );
                }
                Some(agent::AgentEvent::Cerrado) => {
                    let estaba_activo = *modo_agente.read();
                    agente.set(None);
                    modo_agente.set(false);
                    grabando.set(false);
                    if estaba_activo {
                        push(&mut mensajes, &mut ids, false, "🤖 Agente desconectado.".to_string(), None);
                    }
                }
                None => {}
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });

    // Enviar texto -> Deepgram lo "dice" con una voz Aura (TTS).
    let enviar_texto = move |_| {
        let texto = entrada.read().trim().to_string();
        if texto.is_empty() || *ocupado.read() {
            return;
        }
        entrada.set(String::new());
        push(&mut mensajes, &mut ids, true, texto.clone(), None);
        let key = api_key.read().clone();
        ocupado.set(true);
        spawn(async move {
            let audio = if key.is_empty() {
                None
            } else {
                deepgram::hablar(&key, &texto, deepgram::VOZ_ES)
                    .await
                    .map(|b| deepgram::data_url_mpeg(&b))
                    .ok()
            };
            let texto_bot = if key.is_empty() {
                "⚠️ Configura tu API key de Deepgram (⚙️) para escuchar la respuesta."
            } else {
                "🔊 Aquí tienes tu mensaje hablado por la voz Aura de Deepgram."
            };
            push(&mut mensajes, &mut ids, false, texto_bot.to_string(), audio);
            ocupado.set(false);
        });
    };

    // Adjuntar/grabar nota de voz -> Deepgram la transcribe (Nova STT).
    let adjuntar_audio = move |evt: Event<FormData>| {
        if *ocupado.read() {
            return;
        }
        let key = api_key.read().clone();
        if key.is_empty() {
            mostrar_ajustes.set(true);
            return;
        }
        // En Dioxus 0.7 `files()` devuelve `Vec<FileData>` (antes un `FileEngine`).
        let Some(archivo) = evt.files().into_iter().next() else {
            return;
        };
        ocupado.set(true);
        spawn(async move {
            let bytes = match archivo.read_bytes().await {
                Ok(bytes) => bytes.to_vec(),
                Err(e) => {
                    push(
                        &mut mensajes,
                        &mut ids,
                        false,
                        format!("⚠️ No se pudo leer el archivo de audio: {e}"),
                        None,
                    );
                    ocupado.set(false);
                    return;
                }
            };
            let nota = format!(
                "data:audio/webm;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            );
            push(&mut mensajes, &mut ids, true, "🎙️ Nota de voz".to_string(), Some(nota));
            match deepgram::transcribir(&key, bytes).await {
                Ok(transcripcion) => {
                    let audio = deepgram::hablar(&key, &transcripcion, deepgram::VOZ_ES)
                        .await
                        .map(|b| deepgram::data_url_mpeg(&b))
                        .ok();
                    push(
                        &mut mensajes,
                        &mut ids,
                        false,
                        format!("📝 Transcripción (nova-3): {transcripcion}"),
                        audio,
                    );
                }
                Err(e) => {
                    push(&mut mensajes, &mut ids, false, format!("⚠️ Error: {e}"), None);
                }
            }
            ocupado.set(false);
        });
    };

    // Nota de voz grabada dentro de la app (🎙️ en móvil).
    //
    // En Android/iOS no se usa `<input type=file capture="microphone">`: el
    // intent del grabador del sistema no siempre existe y el botón queda
    // inoperante. Se captura con `getUserMedia` (el mismo PCM 16 kHz del modo
    // agente) y se manda a `/v1/listen` envuelto en WAV.
    let toggle_nota = move |_| {
        if *ocupado.read() {
            return;
        }
        let key = api_key.read().clone();
        if key.is_empty() {
            mostrar_ajustes.set(true);
            return;
        }
        if !*grabando_nota.read() {
            grabando_nota.set(true);
            spawn(async move {
                match dioxus::document::eval(REC_START).await {
                    Ok(v) if v.as_str() == Some("ok") => {}
                    _ => {
                        grabando_nota.set(false);
                        push(
                            &mut mensajes,
                            &mut ids,
                            false,
                            "⚠️ No se pudo acceder al micrófono. Concede el permiso de micrófono a la app.".to_string(),
                            None,
                        );
                    }
                }
            });
            return;
        }
        // Detener y enviar: el JS devuelve PCM linear16 16 kHz en base64.
        grabando_nota.set(false);
        ocupado.set(true);
        spawn(async move {
            let b64 = dioxus::document::eval(REC_STOP)
                .await
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let pcm = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .unwrap_or_default();
            if pcm.is_empty() {
                push(
                    &mut mensajes,
                    &mut ids,
                    false,
                    "⚠️ No se grabó audio. Prueba de nuevo.".to_string(),
                    None,
                );
                ocupado.set(false);
                return;
            }
            push(
                &mut mensajes,
                &mut ids,
                true,
                "🎙️ Nota de voz".to_string(),
                Some(deepgram::data_url_wav(&pcm, 16000)),
            );
            match deepgram::transcribir_wav(&key, deepgram::pcm16_a_wav(&pcm, 16000)).await {
                Ok(transcripcion) => {
                    let audio = deepgram::hablar(&key, &transcripcion, deepgram::VOZ_ES)
                        .await
                        .map(|b| deepgram::data_url_mpeg(&b))
                        .ok();
                    push(
                        &mut mensajes,
                        &mut ids,
                        false,
                        format!("📝 Transcripción (nova-3): {transcripcion}"),
                        audio,
                    );
                }
                Err(e) => {
                    push(&mut mensajes, &mut ids, false, format!("⚠️ Error: {e}"), None);
                }
            }
            ocupado.set(false);
        });
    };

    // Conectar / desconectar el agente de voz (🤖).
    let toggle_agente = move |_| {
        if *modo_agente.read() {
            if let Some(a) = agente.read().as_ref() {
                a.cerrar();
            }
            agente.set(None);
            modo_agente.set(false);
            grabando.set(false);
            push(&mut mensajes, &mut ids, false, "🤖 Agente desconectado.".to_string(), None);
            return;
        }
        let key = api_key.read().clone();
        if key.is_empty() {
            mostrar_ajustes.set(true);
            return;
        }
        let proveedor = proveedor_llm.read().clone();
        let preset = agent::Proveedor::buscar(&proveedor);
        let url = llm_url.read().trim().to_string();
        let lk = llm_key.read().clone();
        let modelo = modelo_llm.read().clone();
        // Un proveedor propio necesita endpoint y key: si faltan, se abren los
        // ajustes con un mensaje claro en vez de dejar que falle el agente.
        let falta = if preset.requiere_key() && url.is_empty() {
            Some("la URL del endpoint")
        } else if preset.requiere_key() && lk.trim().is_empty() {
            Some("la API key del LLM")
        } else {
            None
        };
        if let Some(que_falta) = falta {
            mostrar_ajustes.set(true);
            push(
                &mut mensajes,
                &mut ids,
                false,
                format!("⚠️ Para {} configura {que_falta} (⚙️).", preset.nombre),
                None,
            );
            return;
        }
        modo_agente.set(true);
        push(
            &mut mensajes,
            &mut ids,
            false,
            format!("🤖 Conectando agente de voz ({})…", preset.nombre),
            None,
        );
        spawn(async move {
            let llm = agent::AjustesLlm {
                proveedor: &proveedor,
                modelo: &modelo,
                key: &lk,
                url: &url,
            };
            match agent::conectar(&key, INSTRUCCIONES, &llm).await {
                Ok(a) => {
                    agente.set(Some(a));
                    push(
                        &mut mensajes,
                        &mut ids,
                        false,
                        "🤖 Agente conectado. Pulsa 🎙️, habla y pulsa ⏹ para enviar.".to_string(),
                        None,
                    );
                }
                Err(e) => {
                    modo_agente.set(false);
                    push(
                        &mut mensajes,
                        &mut ids,
                        false,
                        format!("⚠️ No se pudo conectar el agente: {e}"),
                        None,
                    );
                }
            }
        });
    };

    // Iniciar / detener la grabación del micrófono (solo en modo agente).
    let toggle_grabacion = move |_| {
        if !*grabando.read() {
            grabando.set(true);
            spawn(async move {
                match dioxus::document::eval(REC_START).await {
                    Ok(v) if v.as_str() == Some("ok") => {}
                    _ => {
                        grabando.set(false);
                        push(
                            &mut mensajes,
                            &mut ids,
                            false,
                            "⚠️ No se pudo acceder al micrófono. Concede el permiso RECORD_AUDIO en ajustes de Android.".to_string(),
                            None,
                        );
                    }
                }
            });
        } else {
            grabando.set(false);
            spawn(async move {
                if let Ok(v) = dioxus::document::eval(REC_STOP).await {
                    if let Some(b64) = v.as_str() {
                        if let Ok(pcm) = base64::engine::general_purpose::STANDARD.decode(b64) {
                            if !pcm.is_empty() {
                                let g = agente.read();
                                if let Some(a) = g.as_ref() {
                                    a.enviar_audio(pcm);
                                }
                                drop(g);
                                push(
                                    &mut mensajes,
                                    &mut ids,
                                    true,
                                    "🎙️ Audio enviado al agente…".to_string(),
                                    None,
                                );
                            }
                        }
                    }
                }
            });
        }
    };

    // Las notas de voz se graban dentro de la app solo donde `getUserMedia`
    // funciona de forma fiable (Android/iOS); en escritorio se usa el selector.
    let movil = cfg!(any(target_os = "android", target_os = "ios"));

    rsx! {
        style { "{CSS}" }
        div { class: "app",
            // Encabezado estilo WhatsApp
            div { class: "header",
                div { class: "avatar", "🎙️" }
                div { class: "contacto",
                    div { class: "nombre", "Deepgram" }
                    div { class: "estado",
                        if *ocupado.read() {
                            "escribiendo…"
                        } else if *modo_agente.read() {
                            "agente de voz activo"
                        } else {
                            "en línea · nova-3 STT · Aura TTS"
                        }
                    }
                }
                button {
                    class: if *modo_agente.read() { "iconbtn activo" } else { "iconbtn" },
                    onclick: toggle_agente,
                    "🤖"
                }
                button { class: "iconbtn", onclick: move |_| mostrar_ajustes.set(true), "⚙️" }
            }

            // Lista de mensajes
            div { class: "mensajes",
                for m in mensajes.read().iter() {
                    div { class: if m.mio { "fila mia" } else { "fila" },
                        div { class: if m.mio { "burbuja mia" } else { "burbuja" },
                            { m.texto.clone() }
                            if let Some(a) = &m.audio {
                                audio { class: "audio", controls: true, src: "{a}" }
                            }
                            div { class: "meta", "{m.hora}", if m.mio { " ✓✓" } }
                        }
                    }
                }
            }

            // Barra de entrada (cambia en modo agente)
            div { class: "inputbar",
                if *modo_agente.read() {
                    button {
                        class: if *grabando.read() { "iconbtn rec" } else { "iconbtn" },
                        onclick: toggle_grabacion,
                        if *grabando.read() { "⏹" } else { "🎙️" }
                    }
                    div { class: "campo agente-info",
                        if *grabando.read() {
                            "🔴 Grabando… habla y pulsa ⏹ para enviar"
                        } else if agente.read().is_some() {
                            "🤖 Agente de voz conectado — usa el micrófono"
                        } else {
                            "Conectando agente…"
                        }
                    }
                } else if movil {
                    // Móvil: graba dentro de la app (no depende del grabador
                    // del sistema, que en muchos Android no existe).
                    button {
                        class: if *grabando_nota.read() { "iconbtn rec" } else { "iconbtn" },
                        onclick: toggle_nota,
                        if *grabando_nota.read() { "⏹" } else { "🎙️" }
                    }
                    input {
                        class: "campo",
                        placeholder: if *grabando_nota.read() { "🔴 Grabando… pulsa ⏹ para enviar" } else { "Mensaje" },
                        value: "{entrada}",
                        oninput: move |e| entrada.set(e.value())
                    }
                    button { class: "iconbtn enviar", onclick: enviar_texto, "➤" }
                } else {
                    // Escritorio: el selector de archivos del sistema (incluye
                    // grabadoras si las hay) funciona bien aquí.
                    label { class: "iconbtn",
                        "🎙️"
                        input {
                            class: "oculto",
                            r#type: "file",
                            accept: "audio/*",
                            "capture": "microphone",
                            onchange: adjuntar_audio
                        }
                    }
                    input {
                        class: "campo",
                        placeholder: "Mensaje",
                        value: "{entrada}",
                        oninput: move |e| entrada.set(e.value())
                    }
                    button { class: "iconbtn enviar", onclick: enviar_texto, "➤" }
                }
            }

            // Modal de configuración
            if *mostrar_ajustes.read() {
                div { class: "modal",
                    div { class: "modal-content",
                        h3 { "Configuración" }
                        p { class: "hint", "API key de Deepgram (gratis en console.deepgram.com)" }
                        input {
                            class: "campo",
                            placeholder: "API Key de Deepgram",
                            value: "{api_key}",
                            oninput: move |e| api_key.set(e.value())
                        }
                        p { class: "hint", "Agente de voz (🤖): proveedor del LLM" }
                        select {
                            class: "campo",
                            value: "{proveedor_llm}",
                            onchange: move |e| {
                                // Elegir proveedor rellena endpoint y modelo con
                                // sus valores por defecto (editables después).
                                let preset = agent::Proveedor::buscar(&e.value());
                                proveedor_llm.set(preset.id.to_string());
                                llm_url.set(preset.url.to_string());
                                modelo_llm.set(preset.modelo.to_string());
                            },
                            for p in agent::PROVEEDORES {
                                option { value: "{p.id}", "{p.nombre}" }
                            }
                        }
                        p { class: "hint",
                            match agent::Proveedor::buscar(&proveedor_llm.read()) {
                                p if p.requiere_key() && !p.tiene_endpoint() => {
                                    "Escribe la URL del endpoint (compatible OpenAI) y tu API key.".to_string()
                                }
                                p if p.requiere_key() => format!(
                                    "{} usa tu propia API key en ese endpoint.",
                                    p.nombre,
                                ),
                                p => format!("{} no necesita key: LLM gestionado por Deepgram.", p.nombre),
                            }
                        }
                        input {
                            class: "campo",
                            placeholder: "API key del LLM",
                            value: "{llm_key}",
                            oninput: move |e| llm_key.set(e.value())
                        }
                        input {
                            class: "campo",
                            placeholder: "Endpoint del LLM (p. ej. https://api.deepseek.com/v1/chat/completions)",
                            value: "{llm_url}",
                            oninput: move |e| llm_url.set(e.value())
                        }
                        input {
                            class: "campo",
                            placeholder: "Modelo (p. ej. deepseek-chat)",
                            value: "{modelo_llm}",
                            oninput: move |e| modelo_llm.set(e.value())
                        }
                        button { class: "btn", onclick: move |_| {
                            let k = api_key.read().clone();
                            let lk = llm_key.read().clone();
                            let mdl = modelo_llm.read().clone();
                            let prov = proveedor_llm.read().clone();
                            let url = llm_url.read().clone();
                            let js = format!(
                                "localStorage.setItem('dg_key', {dg}); localStorage.setItem('llm_key', {llm}); localStorage.setItem('llm_modelo', {mod}); localStorage.setItem('llm_proveedor', {prov}); localStorage.setItem('llm_url', {url});",
                                dg = serde_json::to_string(&k).unwrap_or_default(),
                                llm = serde_json::to_string(&lk).unwrap_or_default(),
                                mod = serde_json::to_string(&mdl).unwrap_or_default(),
                                prov = serde_json::to_string(&prov).unwrap_or_default(),
                                url = serde_json::to_string(&url).unwrap_or_default(),
                            );
                            spawn(async move { let _ = dioxus::document::eval(&js).await; });
                            mostrar_ajustes.set(false);
                        }, "Guardar" }
                        button { class: "btn sec", onclick: move |_| mostrar_ajustes.set(false), "Cerrar" }
                    }
                }
            }
        }
    }
}

const CSS: &str = r#"
* { box-sizing: border-box; margin: 0; padding: 0; font-family: system-ui, sans-serif; }
html, body, #main { height: 100%; }
.app { display: flex; flex-direction: column; height: 100dvh; background: #0b141a; color: #e9edef; }
.header { display: flex; align-items: center; gap: 10px; padding: 10px 12px; background: #202c33; }
.avatar { width: 40px; height: 40px; border-radius: 50%; background: #00a884;
          display: flex; align-items: center; justify-content: center; font-size: 20px; }
.contacto { flex: 1; }
.nombre { font-weight: 600; }
.estado { font-size: 12px; color: #8696a0; }
.mensajes { flex: 1; overflow-y: auto; padding: 12px; display: flex;
            flex-direction: column; gap: 6px; background: #0b141a; }
.fila { display: flex; }
.fila.mia { justify-content: flex-end; }
.burbuja { max-width: 80%; padding: 8px 10px; border-radius: 10px;
           background: #202c33; font-size: 15px; word-break: break-word; }
.burbuja.mia { background: #005c4b; }
.meta { text-align: right; font-size: 11px; color: #8696a0; margin-top: 4px; }
.audio { width: 230px; height: 36px; margin-top: 6px; display: block; }
.inputbar { display: flex; align-items: center; gap: 8px; padding: 8px; background: #202c33; }
.campo { flex: 1; background: #2a3942; border: none; border-radius: 20px;
         padding: 10px 14px; color: #e9edef; font-size: 15px; outline: none; }
select.campo { appearance: none; color: #e9edef; }
.agente-info { display: flex; align-items: center; font-size: 13px; color: #8696a0; }
.iconbtn { background: none; border: none; font-size: 22px; color: #8696a0;
           cursor: pointer; position: relative; padding: 6px; }
.iconbtn.activo { color: #00a884; }
.iconbtn.rec { color: #ef4444; }
.enviar { color: #00a884; }
.oculto { display: none; }
.modal { position: fixed; inset: 0; background: rgba(0,0,0,.6);
         display: flex; align-items: center; justify-content: center; z-index: 10; }
.modal-content { background: #202c33; padding: 20px; border-radius: 12px;
                 width: 90%; max-width: 380px; display: flex; flex-direction: column; gap: 10px; }
.hint { font-size: 12px; color: #8696a0; }
.btn { background: #00a884; border: none; border-radius: 8px; padding: 10px;
       color: #fff; font-size: 15px; cursor: pointer; }
.btn.sec { background: #2a3942; }
"#;
