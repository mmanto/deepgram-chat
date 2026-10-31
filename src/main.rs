mod agent;
mod config;

use base64::Engine as _;
use chrono::Local;
use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
struct Message {
    id: usize,
    mine: bool,
    text: String,
    time: String,
}

/// Fila del panel izquierdo: el contacto y su último mensaje.
struct ContactRow {
    id: String,
    name: String,
    initials: String,
    preview: String,
    time: String,
    active: bool,
    online: bool,
}

fn now() -> String {
    Local::now().format("%H:%M").to_string()
}

/// Recorta un aviso para la barra de estado (el completo va al log).
fn truncate(text: &str, max: usize) -> String {
    let mut short: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        short.push('…');
    }
    short
}

/// Empuja un mensaje al historial del contacto.
fn push(
    histories: &mut Signal<Vec<Vec<Message>>>,
    contact_index: usize,
    ids: &mut Signal<usize>,
    mine: bool,
    text: String,
) {
    let id = *ids.read();
    ids.set(id + 1);
    let mut messages = histories.write();
    messages[contact_index].push(Message {
        id,
        mine,
        text,
        time: now(),
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

/// Reproductor de la voz del agente: programa cada trama PCM en el reloj del
/// `AudioContext` (no crea un `<audio>` por turno). Así los trozos suenan
/// pegados —sin el silencio audible en cada corte— y el primer trozo se oye
/// en cuanto llega, sin esperar a `AgentAudioDone`. Es el mismo esquema del
/// starter de FastAPI que funciona.
const AUDIO_JS: &str = r#"
window.dgAudio = { ctx: null, t: 0, fuentes: new Set(), silenciado: false };
window.dgAudioPush = function (b64) {
  const st = window.dgAudio;
  // Turno interrumpido por el usuario: se descarta lo que siga llegando
  // hasta que el agente empiece otro turno (o termine el actual).
  if (st.silenciado) return "ok";
  if (!st.ctx) st.ctx = new AudioContext();
  const ctx = st.ctx;
  if (ctx.state === "suspended") ctx.resume();
  const bin = atob(b64);
  const n = bin.length >> 1;
  if (n === 0) return "ok";
  const buf = ctx.createBuffer(1, n, 24000);
  const ch = buf.getChannelData(0);
  for (let i = 0; i < n; i++) {
    const v = bin.charCodeAt(2 * i) | (bin.charCodeAt(2 * i + 1) << 8);
    ch[i] = (v >= 0x8000 ? v - 0x10000 : v) / 32768;
  }
  // Colchón de 150 ms: absorbe el jitter de red/TTS sin dejar huecos entre
  // trozos. Si el reloj se quedó atrás (turno nuevo) se reengancha aquí.
  if (st.t <= ctx.currentTime) st.t = ctx.currentTime + 0.15;
  const src = ctx.createBufferSource();
  src.buffer = buf;
  src.connect(ctx.destination);
  src.start(st.t);
  st.t += buf.duration;
  st.fuentes.add(src);
  src.onended = () => {
    st.fuentes.delete(src);
    if (st.fuentes.size === 0) st.t = 0; // el siguiente turno empieza limpio
  };
  return "ok";
};
// Barge-in: el usuario habló, se tira lo ya programado del turno anterior.
window.dgAudioStop = function () {
  const st = window.dgAudio;
  for (const s of st.fuentes) { try { s.stop(); } catch (_) {} }
  st.fuentes.clear();
  st.t = 0;
  return "ok";
};
// Botón "interrumpir": corta lo programado y descarta el resto del turno.
// Se vuelve a habilitar cuando el agente termina o empieza otro turno.
window.dgAudioSilenciar = function (v) {
  const st = window.dgAudio;
  if (!st) return "ok";
  st.silenciado = !!v;
  if (st.silenciado) {
    for (const s of st.fuentes) { try { s.stop(); } catch (_) {} }
    st.fuentes.clear();
    st.t = 0;
  }
  return "ok";
};
"#;

/// Encola un trozo de PCM del agente (linear16 24 kHz) y devuelve el JS que
/// hay que ejecutar para reproducirlo en orden.
fn reproducir_pcm(pcm: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(pcm);
    format!(
        "window.dgAudioPush({src}); return 'ok';",
        src = serde_json::to_string(&b64).unwrap_or_default(),
    )
}

/// Captura continua del micrófono (sin VAD propio).
///
/// Acumula los bloques `Float32Array` del `ScriptProcessor` en
/// `window.dgMic.cola`; Rust los drena cada ~100 ms con `CONV_DRAIN` y los
/// manda al agente, que cierra cada turno con **su** VAD. `echoCancellation`
/// evita que la voz del agente vuelva por el micrófono.
const CONV_START: &str = r#"
try {
  if (window.dgMic) return "ok";
  if (!navigator.mediaDevices?.getUserMedia) return "error: getUserMedia no disponible";
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: {
      channelCount: 1,
      sampleRate: 16000,
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
    },
  });
  const ctx = new AudioContext({ sampleRate: 16000 });
  if (ctx.state === "suspended") await ctx.resume();
  const entrada = ctx.createMediaStreamSource(stream);
  const proc = ctx.createScriptProcessor(2048, 1, 1); // 128 ms a 16 kHz
  const mudo = ctx.createGain();
  mudo.gain.value = 0; // no devolver el micrófono al altavoz (acople)
  entrada.connect(proc);
  proc.connect(mudo);
  mudo.connect(ctx.destination);

  const mic = {
    ctx,
    stream,
    proc,
    entrada,
    mudo,
    tasa: ctx.sampleRate,
    cola: new Float32Array(0),
    pos: 0,
  };
  const tope = Math.round(mic.tasa * 5); // 5 s: si nadie drena, no crece sin fin
  proc.onaudioprocess = (e) => {
    const datos = e.inputBuffer.getChannelData(0);
    let cola = new Float32Array(mic.cola.length + datos.length);
    cola.set(mic.cola);
    cola.set(datos, mic.cola.length);
    if (cola.length > tope) {
      cola = cola.slice(cola.length - tope);
      mic.pos = 0; // se perdió el ancla de interpolación: no pasa nada grave
    }
    mic.cola = cola;
  };
  window.dgMic = mic;
  return "ok";
} catch (err) { return "error: " + err; }
"#;

/// Drena lo capturado desde la última llamada: remuestrea a 16 kHz (el
/// dispositivo puede ignorar el `sampleRate` pedido) y lo devuelve en base64.
/// Conserva la última muestra y la fase fraccional para que los bloques
/// consecutivos empalmen sin clics.
const CONV_DRAIN: &str = r#"
const mic = window.dgMic;
if (!mic || mic.cola.length < 2) return "";
const cola = mic.cola;
const paso = mic.tasa / 16000;
const n = Math.floor((cola.length - 1 - mic.pos) / paso);
if (n <= 0) return "";
const pcm = new Int16Array(n);
for (let i = 0; i < n; i++) {
  const x = mic.pos + i * paso;
  const i0 = Math.floor(x);
  const f = x - i0;
  const s = cola[i0] + (cola[i0 + 1] - cola[i0]) * f;
  const v = Math.max(-1, Math.min(1, s));
  pcm[i] = v < 0 ? v * 0x8000 : v * 0x7fff;
}
const fin = mic.pos + n * paso;
const corte = Math.min(Math.floor(fin), cola.length - 1);
mic.pos = fin - corte;
mic.cola = cola.slice(corte);
const bytes = new Uint8Array(pcm.buffer);
let bin = "";
for (let i = 0; i < bytes.length; i += 32768) {
  bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 32768));
}
return btoa(bin);
"#;

/// Iniciales del nombre del bot para el avatar ("Municipio de Bolívar" → "MB").
fn initials(name: &str) -> String {
    let mut letras: Vec<char> = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect();
    if letras.is_empty() {
        letras = vec!['A', 'I'];
    }
    letras
        .iter()
        .flat_map(|c| c.to_uppercase())
        .collect::<String>()
}

/// Cadena completa de causas de un error. `Display` de `reqwest::Error` solo
/// muestra "error sending request for url (…)", que oculta si falló el DNS, el
/// TCP, el TLS o el runtime de Tokio.
fn causas(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut fuente = e.source();
    while let Some(causa) = fuente {
        out.push_str(" ← ");
        out.push_str(&causa.to_string());
        fuente = causa.source();
    }
    out
}

/// Fallo del bootstrap de sesión. `reintentable` separa los fallos de red o de
/// servidor transitorios (se reintentan con backoff) de una respuesta
/// definitiva (400/401/403: reintentar no arregla nada).
struct FalloSesion {
    mensaje: String,
    reintentable: bool,
}

/// Espera antes del siguiente intento: 1,5 s, 3 s, 6 s, 12 s, 24 s y techo de
/// 30 s. Sin esto, un backend caído recibe ~30 sesiones por minuto y el
/// resolver DNS de Android acaba devolviendo `EAI_NODATA`.
fn espera_reintento_ms(fallos: u32) -> u64 {
    1_500u64
        .saturating_mul(1u64 << fallos.min(5))
        .min(30_000)
}

/// Programa un reintento de `start_agent`: sube el backoff y despierta al
/// watcher de reintentos.
fn programar_reintento(mut intentos: Signal<u32>, mut reintento: Signal<(u64, u64)>) {
    let fallos = *intentos.peek();
    intentos.set(fallos.saturating_add(1));
    let espera = espera_reintento_ms(fallos);
    let id = reintento.peek().0;
    reintento.set((id.wrapping_add(1), espera));
    eprintln!(
        "[deepgram-chat] reconexión programada en {espera} ms (fallo {})",
        fallos + 1
    );
}

/// `POST /api/public/voice/session` con la API key del tenant.
///
/// Devuelve el JSON con `tenant`, `bot`, `channel`, `greeting`, `ws_path`,
/// `ticket` y `device_id`. La key va en el header (nunca en la URL).
async fn abrir_sesion(key: &str, device: &str) -> Result<serde_json::Value, FalloSesion> {
    // Sin timeout, una resolución DNS colgada deja `conectando` en true y la
    // app no vuelve a intentar nunca (visto tras desbloquear el móvil).
    let cliente = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .map_err(|e| FalloSesion {
            mensaje: format!("No se pudo crear el cliente HTTP: {e}"),
            reintentable: true,
        })?;
    let response = cliente
        .post(config::session_url())
        .bearer_auth(key)
        .json(&serde_json::json!({ "device_id": device }))
        .send()
        .await
        .map_err(|e| FalloSesion {
            mensaje: format!("No se pudo contactar el backend: {}", causas(&e)),
            reintentable: true,
        })?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let body: String = body.chars().take(300).collect();
        return Err(FalloSesion {
            mensaje: format!("Backend HTTP {status}: {body}"),
            reintentable: status.is_server_error()
                || status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status == reqwest::StatusCode::REQUEST_TIMEOUT,
        });
    }
    response.json().await.map_err(|e| FalloSesion {
        mensaje: format!("Respuesta inválida del backend: {e}"),
        reintentable: false,
    })
}

#[component]
fn App() -> Element {
    // Historial de la conversación (en memoria, solo para la UI). El backend
    // reanuda el contexto desde la DB al abrir la sesión.
    let mut histories = use_signal(|| vec![Vec::<Message>::new()]);
    // Generación de conexión: invalida una sesión que seguía conectando.
    let mut generation = use_signal(|| 0u64);
    // API key del tenant: build-time, `.env` de runtime o modal de Configuración.
    let mut tenant_key = use_signal(config::tenant_key);
    // Identidad estable del dispositivo (se genera en el webview y se persiste).
    let mut device_id = use_signal(String::new);
    // Identidad que devuelve el backend al resolver la key.
    let mut bot_name = use_signal(|| "Agente".to_string());
    let mut tenant_name = use_signal(String::new);
    let mut bot_desc = use_signal(|| "Asistente de voz".to_string());
    let mut mostrar_ajustes = use_signal(|| false);
    let mut ids = use_signal(|| 0usize);
    let mut agente = use_signal(|| None::<agent::Agente>);
    // True mientras el bootstrap HTTP o `agent::conectar` están en curso.
    let mut conectando = use_signal(|| false);
    // La app solo tiene modo conversación: el micrófono queda abierto y el VAD
    // del agente cierra cada turno; `dialogo` indica que el micro está activo.
    let mut dialogo = use_signal(|| false);
    // El permiso de micrófono falló: sin audio el agente acaba cerrando el
    // socket, así que no se reconecta en bucle (se reintenta al Guardar).
    let mut sin_mic = use_signal(|| false);
    // Estados del turno en curso (solo para el subtítulo y la barra).
    let mut escuchando = use_signal(|| false);
    let mut hablando = use_signal(|| false);
    let mut pensando = use_signal(|| false);
    // Último `Warning`/`Error` del agente (p. ej. `THINK_REQUEST_FAILED`).
    let mut aviso = use_signal(String::new);
    // Filtro del buscador: solo oculta burbujas, no toca el historial.
    let mut busqueda = use_signal(String::new);
    // En móvil se ve un panel cada vez: la lista o el chat. En escritorio
    // ambos están visibles y este estado no cambia nada.
    let mut en_chat = use_signal(|| true);
    // Reconexión con backoff: `intentos` cuenta fallos seguidos y `reintento`
    // publica (id, espera_ms) para que el watcher lance un único `start_agent`.
    let mut intentos = use_signal(|| 0u32);
    let mut reintento = use_signal(|| (0u64, 0u64));

    // Pide el ticket al backend con la API key del tenant y conecta el proxy.
    // El micrófono lo abre el evento `Listo` (SettingsApplied): antes de eso
    // el agente no acepta audio.
    let start_agent: Callback<(), ()> = Callback::new(move |_| {
        if agente.read().is_some() || *conectando.read() {
            return;
        }
        let key = tenant_key.read().trim().to_string();
        if key.is_empty() {
            mostrar_ajustes.set(true);
            return;
        }
        let mut device = device_id.read().trim().to_string();
        if device.is_empty() {
            device = format!("dev-{}", chrono::Local::now().timestamp_millis());
            device_id.set(device.clone());
        }
        let session = *generation.read();
        conectando.set(true);
        aviso.set(String::new());
        eprintln!("[deepgram-chat] intento de sesión");
        spawn(async move {
            match abrir_sesion(&key, &device).await {
                Ok(value) => {
                    if *generation.read() != session {
                        return;
                    }
                    bot_name.set(
                        value["bot"]["name"].as_str().unwrap_or("Agente").to_string(),
                    );
                    tenant_name.set(
                        value["tenant"]["name"].as_str().unwrap_or("").to_string(),
                    );
                    if let Some(greeting) = value["greeting"].as_str().filter(|g| !g.is_empty()) {
                        bot_desc.set(greeting.to_string());
                    }
                    let served_device = value["device_id"]
                        .as_str()
                        .filter(|d| !d.is_empty())
                        .unwrap_or(&device);
                    device_id.set(served_device.to_string());

                    let ws_path = value["ws_path"].as_str().unwrap_or_default();
                    let ticket = value["ticket"].as_str().unwrap_or_default();
                    let url = config::ws_url(ws_path, ticket, served_device);
                    let result = agent::conectar(&url).await;
                    if *generation.read() != session {
                        return;
                    }
                    conectando.set(false);
                    match result {
                        Ok(agent) => agente.set(Some(agent)),
                        Err(error) => {
                            aviso.set(truncate(&format!("No se pudo conectar el agente: {error}"), 160));
                            eprintln!("[deepgram-chat] error: no se pudo conectar el agente: {error}");
                            // El ticket es nuevo en cada intento: un fallo del
                            // handshake (red, 5xx del proxy) se reintenta.
                            programar_reintento(intentos, reintento);
                        }
                    }
                }
                Err(fallo) => {
                    if *generation.read() != session {
                        return;
                    }
                    conectando.set(false);
                    aviso.set(truncate(&fallo.mensaje, 160));
                    eprintln!("[deepgram-chat] error de sesión: {}", fallo.mensaje);
                    if fallo.reintentable {
                        programar_reintento(intentos, reintento);
                    }
                }
            }
        });
    });

    // Watcher de reintentos: dispara `start_agent` cuando toca esperar. Es el
    // único consumidor de `reintento`, así que un fallo del POST también se
    // recupera sin depender de la bomba de eventos.
    use_future(move || async move {
        let mut visto = 0u64;
        loop {
            let (id, espera) = *reintento.peek();
            if id != visto {
                visto = id;
                // La espera se trocea para que un reintento más nuevo (p. ej.
                // volver a primer plano) la corte en vez de esperar el backoff.
                let vence =
                    std::time::Instant::now() + std::time::Duration::from_millis(espera);
                while reintento.peek().0 == id && std::time::Instant::now() < vence {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                if reintento.peek().0 == id
                    && agente.read().is_none()
                    && !*conectando.read()
                {
                    start_agent.call(());
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });

    // Al volver a primer plano se reintenta de inmediato: Android cierra los
    // sockets de una app en background y el primer POST puede fallar con
    // `EAI_NODATA` (pantalla bloqueada, doze, cambio de red).
    use_future(move || async move {
        // El eval queda pendiente a propósito: si terminara, `dioxus.send`
        // dejaría de llegar. Se envía también el estado actual al arrancar.
        let mut eval = dioxus::document::eval(
            r#"const avisar = () => dioxus.send(document.visibilityState);
               document.addEventListener('visibilitychange', avisar);
               avisar();
               await new Promise(() => {});"#,
        );
        while let Ok(estado) = eval.recv::<String>().await {
            if estado != "visible" {
                continue;
            }
            if agente.read().is_some() || *conectando.read() {
                continue;
            }
            intentos.set(0);
            let (id, _) = *reintento.peek();
            reintento.set((id.wrapping_add(1), 0));
            eprintln!("[deepgram-chat] primer plano: reintento inmediato");
        }
    });

    // Abre el micrófono (una sola vez) y deja que el VAD del agente cierre los
    // turnos. Si falla el permiso, se registra y `dialogo` vuelve a false.
    let open_mic: Callback<(), ()> = Callback::new(move |_| {
        if *dialogo.read() {
            return;
        }
        dialogo.set(true);
        spawn(async move {
            match dioxus::document::eval(CONV_START).await {
                Ok(v) if v.as_str() == Some("ok") => sin_mic.set(false),
                _ => {
                    dialogo.set(false);
                    sin_mic.set(true);
                    eprintln!(
                        "[deepgram-chat] error: no se pudo abrir el micrófono (permiso RECORD_AUDIO)"
                    );
                }
            }
        });
    });

    // Instala el reproductor en streaming del webview (los `eval` comparten
    // `window`) y carga la key/identidad guardadas en el almacenamiento.
    use_future(move || async move {
        let _ = dioxus::document::eval(AUDIO_JS).await;
        let js = r#"return {
            key: localStorage.getItem('tenant_key') ?? '',
            device: (() => {
                let d = localStorage.getItem('device_id');
                if (!d) {
                    d = (typeof crypto !== 'undefined' && crypto.randomUUID)
                        ? crypto.randomUUID()
                        : Date.now().toString(36) + Math.random().toString(36).slice(2);
                    localStorage.setItem('device_id', d);
                }
                return d;
            })()
        };"#;
        if let Ok(v) = dioxus::document::eval(js).await {
            if let Some(k) = v["key"].as_str() {
                if !k.is_empty() {
                    tenant_key.set(k.to_string());
                }
            }
            if let Some(d) = v["device"].as_str() {
                device_id.set(d.to_string());
            }
        }
        // Una vez cargada la identidad, arranca el único modo: la conversación.
        start_agent.call(());
    });

    // Bomba de eventos del agente: drena el canal y actualiza la UI.
    //
    // El audio se reproduce en streaming: los trozos que llegan entre dos
    // vueltas se juntan en un solo `eval` (y se esperan en orden, para que el
    // scheduler del webview no reciba tramas desordenadas).
    use_future(move || async move {
        loop {
            let mut audio: Vec<u8> = Vec::new();
            let mut stop_voice = false;
            let mut reactivate_voice = false;
            let mut reopen_mic = false;
            let mut reconnect = false;
            loop {
                let ev = {
                    let handle = agente.read();
                    handle.as_ref().and_then(|a| a.eventos.try_recv().ok())
                };
                let Some(ev) = ev else { break };
                match ev {
                    agent::AgentEvent::TextoUsuario(t) => {
                        // La burbuja ya lleva el icono de micrófono en su
                        // metadato: el prefijo en el texto sobraba.
                        push(&mut histories, 0, &mut ids, true, t);
                    }
                    agent::AgentEvent::TextoAgente(t) => {
                        push(&mut histories, 0, &mut ids, false, t);
                    }
                    agent::AgentEvent::Audio(pcm) => audio.extend_from_slice(&pcm),
                    agent::AgentEvent::AudioTerminado => {
                        hablando.set(false);
                        pensando.set(false);
                        reactivate_voice = true;
                    }
                    agent::AgentEvent::Listo => {
                        pensando.set(false);
                        aviso.set(String::new());
                        // Sesión real con el agente: el backoff vuelve a cero.
                        intentos.set(0);
                        reopen_mic = true;
                        reactivate_voice = true;
                    }
                    agent::AgentEvent::UsuarioHablo => {
                        // Barge-in: cortar la voz ya programada del turno.
                        escuchando.set(true);
                        hablando.set(false);
                        pensando.set(false);
                        stop_voice = true;
                        audio.clear();
                    }
                    agent::AgentEvent::AgentePensando => {
                        escuchando.set(false);
                        pensando.set(true);
                    }
                    agent::AgentEvent::AgenteHablando => {
                        escuchando.set(false);
                        pensando.set(false);
                        hablando.set(true);
                        // Un turno nuevo siempre suena: deshace una
                        // interrupción manual anterior.
                        reactivate_voice = true;
                    }
                    agent::AgentEvent::Error(e) => {
                        escuchando.set(false);
                        hablando.set(false);
                        pensando.set(false);
                        stop_voice = true;
                        audio.clear();
                        aviso.set(truncate(&e, 160));
                        eprintln!("[deepgram-chat] error del agente: {e}");
                    }
                    agent::AgentEvent::Aviso(a) => {
                        aviso.set(truncate(&a, 160));
                        eprintln!("[deepgram-chat] aviso del agente: {a}");
                    }
                    agent::AgentEvent::Meta { bot_name: name, tenant_name: tenant, greeting } => {
                        // Identidad resuelta por el backend a partir de la key:
                        // una sola conversación, sin catálogo de plugins local.
                        bot_name.set(name);
                        tenant_name.set(tenant);
                        if !greeting.is_empty() {
                            bot_desc.set(greeting);
                        }
                    }
                    agent::AgentEvent::Cerrado => {
                        agente.set(None);
                        escuchando.set(false);
                        hablando.set(false);
                        pensando.set(false);
                        stop_voice = true;
                        audio.clear();
                        reconnect = true;
                        break;
                    }
                }
            }
            if stop_voice {
                let _ = dioxus::document::eval("window.dgAudioStop(); return 'ok';").await;
            }
            if reactivate_voice {
                let _ = dioxus::document::eval(
                    "window.dgAudioSilenciar(false); return 'ok';",
                )
                .await;
            }
            if !audio.is_empty() {
                if let Err(e) = dioxus::document::eval(&reproducir_pcm(&audio)).await {
                    eprintln!("[deepgram-chat] error: no se pudo reproducir audio: {e}");
                }
            }
            if reopen_mic {
                open_mic.call(());
            }
            if reconnect {
                if *sin_mic.read() {
                    // Sin micrófono no hay audio: el agente cerraría siempre por
                    // timeout. Se reintenta cuando el usuario guarde ajustes.
                    eprintln!(
                        "[deepgram-chat] aviso: sin micrófono; no se reconecta (revisa el permiso)"
                    );
                } else {
                    // El micrófono sigue abierto: reconecta con espera
                    // exponencial (el proxy puede estar caído un rato).
                    eprintln!("[deepgram-chat] aviso: conexión caída, reconectando…");
                    programar_reintento(intentos, reintento);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });

    // Micrófono en continuo: cada 100 ms se drena lo capturado y se manda al
    // agente. El VAD de Deepgram cierra los turnos; no hay VAD propio ni
    // silencio añadido (con el socket caído el audio se descarta).
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if !*dialogo.read() {
                continue;
            }
            let Ok(v) = dioxus::document::eval(CONV_DRAIN).await else {
                continue;
            };
            let Some(b64) = v.as_str() else {
                continue;
            };
            if b64.is_empty() {
                continue;
            }
            let Ok(pcm) = base64::engine::general_purpose::STANDARD.decode(b64) else {
                continue;
            };
            if let Some(a) = agente.read().as_ref() {
                a.enviar_audio(pcm);
            }
        }
    });

    // --- Derivados de la vista ---
    // El buscador compara en minúsculas; solo oculta burbujas, no toca el
    // historial ni el agente.
    let brand = if tenant_name.read().is_empty() {
        "Voz".to_string()
    } else {
        tenant_name.read().clone()
    };
    let contact_name = bot_name.read().clone();
    let contact_initials = initials(&contact_name);
    let contact_desc = bot_desc.read().clone();
    let query = busqueda.read().trim().to_lowercase();
    let visible: Vec<Message> = {
        let messages = histories.read();
        let list = &messages[0];
        if query.is_empty() {
            list.clone()
        } else {
            list.iter()
                .filter(|message| message.text.to_lowercase().contains(&query))
                .cloned()
                .collect()
        }
    };
    let total = histories.read()[0].len();
    let en_linea = agente.read().is_some() && !*sin_mic.read();
    let estado_chat = if !aviso.read().is_empty() {
        "aviso del agente".to_string()
    } else if *escuchando.read() {
        "te escucho…".to_string()
    } else if *pensando.read() {
        "pensando…".to_string()
    } else if *hablando.read() {
        "hablando…".to_string()
    } else if *sin_mic.read() {
        "sin micrófono".to_string()
    } else if en_linea {
        "en línea".to_string()
    } else {
        "conectando…".to_string()
    };
    let estado_barra = if *escuchando.read() {
        "🔴 Te escucho… puedes hablar con naturalidad"
    } else if *pensando.read() {
        "💭 Pensando la respuesta…"
    } else if *hablando.read() {
        "🔊 Hablando — puedes interrumpir"
    } else if *sin_mic.read() {
        "🎤 Sin micrófono — revisa el permiso y pulsa Guardar"
    } else if agente.read().is_none() {
        "🎧 Conectando el agente…"
    } else {
        "🎧 Conversación activa — habla cuando quieras"
    };
    let last = histories.read()[0].last().cloned();
    let contact_rows: Vec<ContactRow> = vec![ContactRow {
        id: "agente".to_string(),
        name: contact_name.clone(),
        initials: contact_initials.clone(),
        preview: last
            .as_ref()
            .map(|message| truncate(&message.text, 34))
            .unwrap_or_else(|| "Conversación de voz".to_string()),
        time: last
            .as_ref()
            .map(|message| message.time.clone())
            .unwrap_or_else(|| "Ahora".to_string()),
        active: true,
        online: en_linea,
    }];

    // Auto-scroll al final cuando llega un mensaje o aparecen los puntos de
    // "pensando" (la maqueta usa scrollIntoView para lo mismo).
    use_effect(move || {
        let _ = histories.read()[0].len();
        let _ = pensando.read();
        spawn(async move {
            let _ = dioxus::document::eval(
                "const el = document.getElementById('mensajes'); if (el) el.scrollTop = el.scrollHeight; return 'ok';",
            )
            .await;
        });
    });

    rsx! {
        style { "{CSS}" }
        div { class: if *en_chat.read() { "app en-chat" } else { "app" },
            // Panel izquierdo: la única conversación del tenant y el perfil.
            // En móvil ocupa la pantalla cuando no se ve el chat.
            aside { class: "sidebar",
                div { class: "sidebar-head",
                    div { class: "brand",
                        div {
                            h1 { "{brand}" }
                            div { class: "sub", "Conversación" }
                        }
                        button {
                            class: "icon-btn",
                            title: "Configuración",
                            onclick: move |_| mostrar_ajustes.set(true),
                            svg {
                                view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2",
                                path {
                                    stroke_linecap: "round", stroke_linejoin: "round",
                                    d: "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"
                                }
                                circle { cx: "12", cy: "12", r: "3" }
                            }
                        }
                    }
                    div { class: "buscar",
                        svg {
                            view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2",
                            circle { cx: "11", cy: "11", r: "7" }
                            path { stroke_linecap: "round", d: "M21 21l-4.35-4.35" }
                        }
                        input {
                            placeholder: "Buscar en la conversación…",
                            value: "{busqueda}",
                            oninput: move |e| busqueda.set(e.value())
                        }
                    }
                }
                div { class: "lista",
                    for row in contact_rows.iter() {
                        button {
                            key: "{row.id}",
                            class: if row.active { "conv activa" } else { "conv" },
                            title: "Abrir la conversación",
                            onclick: move |_| en_chat.set(true),
                            span { class: "avatar-wrap",
                                span { class: "avatar", "{row.initials}" }
                                span {
                                    class: if row.online { "dot en-linea" } else { "dot" }
                                }
                            }
                            span { class: "conv-info",
                                span { class: "conv-row",
                                    span { class: "conv-nombre", "{row.name}" }
                                    span { class: "conv-hora", "{row.time}" }
                                }
                                span { class: "conv-row",
                                    span { class: "conv-vista", "{row.preview}" }
                                }
                            }
                        }
                    }
                    if !query.is_empty() {
                        p { class: "pista-busqueda", "{visible.len()} de {total} mensajes" }
                    }
                }
                div { class: "perfil",
                    div { class: "avatar", "TÚ" }
                    div { class: "perfil-info",
                        div { class: "perfil-nombre", "Ciudadano" }
                        div { class: "perfil-estado",
                            if en_linea {
                                "En línea"
                            } else if *sin_mic.read() {
                                "Sin micrófono"
                            } else {
                                "Conectando…"
                            }
                        }
                    }
                }
            }

            // Panel derecho: la conversación en vivo.
            main { class: "main",
                header { class: "chat-head",
                    div { class: "chat-id",
                        button {
                            class: "icon-btn ghost solo-movil",
                            title: "Volver a la lista",
                            onclick: move |_| en_chat.set(false),
                            svg {
                                view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2",
                                path { stroke_linecap: "round", stroke_linejoin: "round", d: "M15 19l-7-7 7-7" }
                            }
                        }
                        div { class: "avatar-wrap",
                            div { class: "avatar", "{contact_initials}" }
                            div { class: if en_linea { "dot en-linea" } else { "dot" } }
                        }
                        div { class: "chat-texto",
                            h2 { class: "chat-nombre", "{contact_name}" }
                            p { class: "chat-estado", "{estado_chat}" }
                        }
                    }
                    div { class: "chat-acciones",
                        // Solo aparece si el socket se cayó y no hay reconexión
                        // en curso; pulsarlo reintenta la conexión.
                        if agente.read().is_none() && !*conectando.read() {
                            button {
                                class: "icon-btn ghost",
                                title: "Reconectar",
                                onclick: move |_| start_agent.call(()),
                                svg {
                                    view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2",
                                    path { stroke_linecap: "round", stroke_linejoin: "round", d: "M23 4v6h-6" }
                                    path { stroke_linecap: "round", stroke_linejoin: "round", d: "M20.49 15a9 9 0 1 1-2.12-9.36L23 10" }
                                }
                            }
                        }
                        button {
                            class: "icon-btn ghost",
                            title: "Configuración",
                            onclick: move |_| mostrar_ajustes.set(true),
                            svg {
                                view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2",
                                path {
                                    stroke_linecap: "round", stroke_linejoin: "round",
                                    d: "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"
                                }
                                circle { cx: "12", cy: "12", r: "3" }
                            }
                        }
                    }
                }

                div { class: "mensajes", id: "mensajes",
                    if !visible.is_empty() {
                        div { class: "fecha", span { "Hoy" } }
                    }
                    if visible.is_empty() {
                        div { class: "vacio",
                            div { class: "avatar", "🎙️" }
                            if query.is_empty() {
                                h3 { "Habla para empezar" }
                                p { "{contact_desc}" }
                            } else {
                                h3 { "Sin resultados" }
                                p { "No hay mensajes que coincidan con «{query}»." }
                            }
                        }
                    }
                    for m in visible.iter() {
                        div { class: if m.mine { "fila mia" } else { "fila" },
                            div { class: "burbuja-wrap",
                                div { class: if m.mine { "burbuja mia" } else { "burbuja otra" },
                                    { m.text.clone() }
                                }
                                div { class: "meta",
                                    span { "{m.time}" }
                                    if m.mine {
                                        svg {
                                            class: "ico-mic",
                                            view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2",
                                            path { d: "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z" }
                                            path { d: "M19 10v2a7 7 0 0 1-14 0v-2" }
                                            path { stroke_linecap: "round", d: "M12 19v3" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if *pensando.read() {
                        div { class: "typing",
                            div { class: "burbuja otra",
                                span { class: "punto" }
                                span { class: "punto" }
                                span { class: "punto" }
                            }
                        }
                    }
                }

                // Aviso del agente (p. ej. el LLM del usuario falló y se usó el
                // respaldo gestionado). Va fuera de las burbujas: el chat no cambia.
                if !aviso.read().is_empty() {
                    div { class: "aviso",
                        span { class: "aviso-ico", "⚠️" }
                        span { "{aviso}" }
                    }
                }

                // Barra de estado: solo modo conversación, sin campo de texto
                // (el VAD del agente cierra cada turno al detectar la pausa).
                div { class: "composer",
                    div { class: "composer-row",
                        div {
                            class: if *dialogo.read() && !*sin_mic.read() { "mic-punto activo" } else { "mic-punto" },
                            title: "Micrófono abierto",
                            svg {
                                view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2",
                                path { d: "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z" }
                                path { d: "M19 10v2a7 7 0 0 1-14 0v-2" }
                                path { stroke_linecap: "round", d: "M12 19v3" }
                            }
                        }
                        div { class: "estado-pill", "{estado_barra}" }
                        button {
                            class: if *hablando.read() { "btn-redondo listo" } else { "btn-redondo" },
                            disabled: !*hablando.read(),
                            title: "Interrumpir al agente",
                            onclick: move |_| {
                                // Barge-in explícito: corta la voz ya
                                // programada y silencia lo que reste del turno.
                                spawn(async move {
                                    let _ = dioxus::document::eval(
                                        "window.dgAudioSilenciar(true); return 'ok';",
                                    )
                                    .await;
                                });
                                hablando.set(false);
                                pensando.set(false);
                            },
                            svg {
                                view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2",
                                path { d: "M11 5L6 9H2v6h4l5 4V5z" }
                                path { stroke_linecap: "round", d: "M22 9l-6 6" }
                                path { stroke_linecap: "round", d: "M16 9l6 6" }
                            }
                        }
                    }
                }
            }

            // Modal de configuración
            if *mostrar_ajustes.read() {
                div { class: "modal",
                    div { class: "modal-content",
                        h3 { "Configuración" }
                        p { class: "hint",
                            "API key del tenant (tk_live_…). Te la entrega el administrador de gestion.ar una sola vez."
                        }
                        input {
                            class: "campo",
                            placeholder: "tk_live_…",
                            value: "{tenant_key}",
                            oninput: move |e| tenant_key.set(e.value())
                        }
                        p { class: "hint", "Backend: {config::backend_url()}" }
                        button { class: "btn", onclick: move |_| {
                            let key = tenant_key.read().trim().to_string();
                            let js = format!(
                                "localStorage.setItem('tenant_key', {});",
                                serde_json::to_string(&key).unwrap_or_default(),
                            );
                            spawn(async move { let _ = dioxus::document::eval(&js).await; });
                            mostrar_ajustes.set(false);
                            en_chat.set(true);
                            sin_mic.set(false);
                            aviso.set(String::new());
                            // Reinicia la sesión para que el backend resuelva
                            // el tenant con la key nueva.
                            let next_generation = *generation.read() + 1;
                            generation.set(next_generation);
                            agente.set(None);
                            conectando.set(false);
                            start_agent.call(());
                        }, "Guardar" }
                        button { class: "btn sec", onclick: move |_| mostrar_ajustes.set(false), "Cerrar" }
                        p { class: "hint",
                            { format!(
                                "v{} · audio en streaming + respaldo LLM",
                                env!("CARGO_PKG_VERSION"),
                            ) }
                        }
                    }
                }
            }
        }
    }
}

const CSS: &str = r#"
* { box-sizing: border-box; margin: 0; padding: 0; }
:root {
  --bg: #F0F7FE;
  --surface: #FFFFFF;
  --line: #D6E8F9;
  --line-soft: #E3F0FC;
  --ink: #0F3D6B;
  --muted: #4A7BA8;
  --hint: #6FB1F1;
  --accent: #4A9AE0;
  --accent-strong: #2D7BC4;
  --accent-soft: #E3F0FC;
  --accent-softer: #B8D8F7;
  --own: #1B5A94;
  --online: #34D399;
  --warn-ink: #B45309;
  --warn-bg: #FFFBEB;
  --warn-line: #FDE68A;
}
html, body, #main { height: 100%; }
body {
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', 'Inter', Roboto, Oxygen, Ubuntu, sans-serif;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
  background: var(--bg);
  color: var(--ink);
  overflow: hidden;
}
button { font: inherit; color: inherit; background: none; border: none; cursor: pointer; }
input, select { font: inherit; }
::-webkit-scrollbar { width: 5px; height: 5px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: #CBD5E1; border-radius: 3px; }
::-webkit-scrollbar-thumb:hover { background: #94A3B8; }

.app { display: flex; width: 100%; height: 100dvh; overflow: hidden; background: var(--bg); color: var(--ink); }

/* --- Panel izquierdo --- */
.sidebar { display: flex; flex-direction: column; width: 320px; flex-shrink: 0; background: var(--surface); border-right: 1px solid var(--line); box-shadow: 0 1px 2px rgba(15, 61, 107, .04); }
.sidebar-head { padding: 20px; border-bottom: 1px solid var(--line-soft); }
.brand { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 16px; }
.brand h1 { font-size: 20px; font-weight: 600; }
.brand .sub { margin-top: 2px; font-size: 12px; color: var(--hint); }
.icon-btn { display: flex; align-items: center; justify-content: center; width: 32px; height: 32px; flex-shrink: 0; border-radius: 50%; color: var(--accent-strong); background: var(--accent-soft); transition: background .2s, color .2s; }
.icon-btn:hover { background: var(--accent-softer); }
.icon-btn.ghost { background: transparent; }
.icon-btn.ghost:hover { background: var(--accent-soft); }
.icon-btn svg { width: 16px; height: 16px; }
.buscar { position: relative; }
.buscar svg { position: absolute; top: 50%; left: 12px; width: 16px; height: 16px; transform: translateY(-50%); color: var(--hint); pointer-events: none; }
.buscar input { width: 100%; padding: 10px 14px 10px 38px; background: var(--bg); border: 1px solid var(--line); border-radius: 12px; font-size: 13px; color: var(--ink); outline: none; transition: border-color .2s, box-shadow .2s; }
.buscar input::placeholder { color: rgba(111, 177, 241, .65); }
.buscar input:focus { border-color: var(--hint); box-shadow: 0 0 0 3px rgba(111, 177, 241, .25); }

.lista { flex: 1; min-height: 0; overflow-y: auto; padding: 6px 0; }
.conv { display: flex; align-items: center; gap: 12px; width: 100%; padding: 14px 20px; text-align: left; border-left: 3px solid transparent; transition: background .2s, border-color .2s; }
.conv:hover { background: rgba(240, 247, 254, .7); }
.conv.activa { background: var(--bg); border-left-color: var(--accent); }
.avatar-wrap { position: relative; display: block; flex-shrink: 0; }
.avatar { display: flex; align-items: center; justify-content: center; width: 44px; height: 44px; border-radius: 50%; background: var(--accent-soft); color: var(--accent-strong); font-size: 14px; font-weight: 600; }
.conv.activa .avatar { background: var(--accent); color: #FFFFFF; }
.dot { position: absolute; right: 0; bottom: 0; width: 12px; height: 12px; border: 2px solid var(--surface); border-radius: 50%; background: #CBD5E1; }
.dot.en-linea { background: var(--online); }
.conv-info { display: flex; flex-direction: column; flex: 1; min-width: 0; gap: 2px; }
.conv-row { display: flex; align-items: center; justify-content: space-between; gap: 8px; min-width: 0; }
.conv-nombre { overflow: hidden; font-size: 14px; font-weight: 500; color: var(--ink); text-overflow: ellipsis; white-space: nowrap; }
.conv-hora { flex-shrink: 0; font-size: 11px; color: var(--hint); }
.conv-vista { overflow: hidden; font-size: 12px; color: var(--muted); text-overflow: ellipsis; white-space: nowrap; }
.pista-busqueda { padding: 10px 20px 4px; font-size: 11px; color: var(--hint); }
.perfil { display: flex; align-items: center; gap: 12px; padding: 16px; border-top: 1px solid var(--line-soft); }
.perfil .avatar { width: 36px; height: 36px; background: var(--own); color: #FFFFFF; font-size: 12px; }
.perfil-info { flex: 1; min-width: 0; }
.perfil-nombre { font-size: 14px; font-weight: 500; }
.perfil-estado { font-size: 11px; color: var(--hint); }

/* --- Panel derecho --- */
.main { display: flex; flex: 1; flex-direction: column; min-width: 0; background: var(--bg); }
.chat-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; height: 64px; flex-shrink: 0; padding: 0 24px; background: var(--surface); border-bottom: 1px solid var(--line); box-shadow: 0 1px 2px rgba(15, 61, 107, .04); }
.chat-id { display: flex; align-items: center; gap: 12px; min-width: 0; }
.chat-head .avatar { width: 40px; height: 40px; background: var(--accent); color: #FFFFFF; }
.chat-texto { min-width: 0; }
.chat-nombre { overflow: hidden; font-size: 14px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
.chat-estado { margin-top: 1px; font-size: 12px; color: var(--hint); }
.chat-acciones { display: flex; align-items: center; gap: 4px; }
.solo-movil { display: none; }

.mensajes { display: flex; flex: 1; flex-direction: column; gap: 10px; min-height: 0; padding: 16px 24px; overflow-y: auto; }
.fecha { display: flex; justify-content: center; margin: 2px 0 6px; }
.fecha span { padding: 5px 16px; background: var(--surface); border: 1px solid var(--line); border-radius: 999px; box-shadow: 0 1px 2px rgba(15, 61, 107, .05); font-size: 11px; font-weight: 500; color: var(--hint); }
.fila { display: flex; min-width: 0; }
.fila.mia { justify-content: flex-end; }
.burbuja-wrap { max-width: 75%; min-width: 0; }
.fila.mia .burbuja-wrap { max-width: 75%; }
.burbuja { padding: 10px 14px; border-radius: 16px; font-size: 14px; line-height: 1.45; word-break: break-word; white-space: pre-wrap; }
.burbuja.otra { background: var(--surface); color: var(--ink); border: 1px solid var(--line); border-bottom-left-radius: 6px; box-shadow: 0 1px 2px rgba(15, 61, 107, .05); }
.burbuja.mia { background: var(--own); color: #FFFFFF; border-bottom-right-radius: 6px; }
.meta { display: flex; align-items: center; gap: 4px; margin-top: 4px; font-size: 11px; color: rgba(111, 177, 241, .75); }
.fila.mia .meta { justify-content: flex-end; }
.ico-mic { width: 12px; height: 12px; color: rgba(111, 177, 241, .8); }

.typing { display: flex; justify-content: flex-start; }
.typing .burbuja { display: flex; align-items: center; gap: 6px; padding: 12px 16px; }
.punto { width: 8px; height: 8px; border-radius: 50%; background: var(--hint); animation: tecleo 1.4s infinite ease-in-out both; }
.punto:nth-child(2) { animation-delay: .15s; }
.punto:nth-child(3) { animation-delay: .3s; }
@keyframes tecleo { 0%, 80%, 100% { transform: scale(.6); opacity: .4; } 40% { transform: scale(1); opacity: 1; } }

.vacio { display: flex; flex: 1; flex-direction: column; align-items: center; justify-content: center; gap: 12px; padding: 32px 24px; text-align: center; color: var(--muted); }
.vacio .avatar { width: 64px; height: 64px; font-size: 26px; }
.vacio h3 { font-size: 15px; font-weight: 600; color: var(--ink); }
.vacio p { max-width: 260px; font-size: 13px; line-height: 1.5; }

.aviso { display: flex; align-items: flex-start; gap: 8px; flex-shrink: 0; margin: 0 24px 8px; padding: 8px 12px; background: var(--warn-bg); border: 1px solid var(--warn-line); border-radius: 10px; font-size: 12px; color: var(--warn-ink); word-break: break-word; }
.aviso-ico { flex-shrink: 0; }

.composer { flex-shrink: 0; padding: 14px 24px; background: var(--surface); border-top: 1px solid var(--line); }
.composer-row { display: flex; align-items: center; gap: 10px; }
.mic-punto { position: relative; display: flex; align-items: center; justify-content: center; width: 36px; height: 36px; flex-shrink: 0; border-radius: 50%; background: var(--accent-soft); color: var(--accent-strong); }
.mic-punto svg { width: 18px; height: 18px; }
.mic-punto.activo::after { content: ""; position: absolute; inset: -3px; border: 2px solid rgba(74, 154, 224, .35); border-radius: 50%; animation: latido 2s infinite; }
@keyframes latido { 0% { transform: scale(.92); opacity: .9; } 70%, 100% { transform: scale(1.18); opacity: 0; } }
.estado-pill { display: flex; align-items: center; flex: 1; min-width: 0; min-height: 42px; padding: 10px 16px; background: var(--bg); border: 1px solid var(--line); border-radius: 12px; font-size: 13px; color: var(--muted); }
.btn-redondo { display: flex; align-items: center; justify-content: center; width: 40px; height: 40px; flex-shrink: 0; border-radius: 50%; background: var(--accent-soft); color: var(--accent-softer); cursor: default; transition: background .2s, color .2s, box-shadow .2s; }
.btn-redondo svg { width: 18px; height: 18px; }
.btn-redondo.listo { background: var(--accent); color: #FFFFFF; box-shadow: 0 4px 12px rgba(111, 177, 241, .4); cursor: pointer; }
.btn-redondo.listo:hover { background: var(--accent-strong); }

/* --- Modal de ajustes --- */
.modal { position: fixed; inset: 0; z-index: 20; display: flex; align-items: center; justify-content: center; padding: 16px; background: rgba(15, 61, 107, .38); }
.modal-content { display: flex; flex-direction: column; gap: 10px; width: 100%; max-width: 420px; max-height: 86dvh; overflow-y: auto; padding: 22px; background: var(--surface); border-radius: 16px; box-shadow: 0 24px 60px rgba(15, 61, 107, .25); }
.modal-content h3 { margin-bottom: 2px; font-size: 17px; font-weight: 600; color: var(--ink); }
.hint { font-size: 11.5px; line-height: 1.5; color: var(--hint); }
.campo { width: 100%; padding: 10px 14px; background: var(--bg); border: 1px solid var(--line); border-radius: 10px; font-size: 13px; color: var(--ink); outline: none; transition: border-color .2s, box-shadow .2s; }
.campo::placeholder { color: rgba(111, 177, 241, .65); }
.campo:focus { border-color: var(--hint); box-shadow: 0 0 0 3px rgba(111, 177, 241, .25); }
select.campo { appearance: auto; }
.btn { width: 100%; padding: 11px 16px; background: var(--accent); border-radius: 10px; font-size: 14px; font-weight: 500; color: #FFFFFF; transition: background .2s; }
.btn:hover { background: var(--accent-strong); }
.btn.sec { background: var(--accent-soft); color: var(--accent-strong); }
.btn.sec:hover { background: var(--accent-softer); }

/* --- Móvil: un panel a la vez --- */
@media (max-width: 767px) {
  .sidebar { width: 100%; border-right: none; }
  .main { display: none; }
  .app.en-chat .sidebar { display: none; }
  .app.en-chat .main { display: flex; }
  .solo-movil { display: flex; }
  .chat-head { padding: 0 14px; }
  .mensajes { padding: 12px 14px; }
  .composer { padding: 10px 12px; }
  .aviso { margin: 0 12px 8px; }
  .burbuja-wrap, .fila.mia .burbuja-wrap { max-width: 82%; }
  .estado-pill { padding: 10px 12px; font-size: 12px; }
  .modal { padding: 12px; }
}
"#;
