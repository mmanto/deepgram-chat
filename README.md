# Deepgram Chat 🎙️

App de chat con interfaz de mensajería de dos paneles hecha con **Rust +
Dioxus 0.7**, desplegable en **Android**, que habla con el **proxy de voz de
gestion.ar** por WebSocket:

| Acción | Servicio usado |
|---|---|
| **Modo conversación** — diálogo de voz con el agente del tenant | `WS /ws/voice/channel/{id}` de gestion.ar, que proxea el **Voice Agent API** de Deepgram (STT + LLM + TTS) |

La app **no** tiene API key de Deepgram ni de ningún LLM: el backend de
gestion.ar es dueño de la key, del prompt, de las tools y de la voz. La app se
identifica con la **API key del tenant** (`tk_live_…`), pide un ticket efímero
y conecta el WebSocket (ver ADR-034/ADR-035 en `gestion.ar`).

> La voz (la tuya y la del agente) se **oye en vivo** y no se guarda en el
> historial local: ahí solo queda el texto transcrito. El backend sí persiste
> cada turno en `conversations`/`messages` del tenant.

> Las voces Aura se eligen en el canal (`voice_config.voice_model`): en español
> existen `aura-2-celeste-es`, `aura-2-estrella-es`, `aura-2-nestor-es` y
> `aura-2-sirio-es`. El catálogo está en
> https://developers.deepgram.com/docs/tts-models

## Interfaz

Diseño de mensajería en **dos paneles** (claro, azul y blanco):

- **Panel izquierdo:** la **única conversación** del tenant (nombre del bot e
  iniciales que devuelve el backend); buscador que filtra las burbujas visibles
  (sin tocar el historial) y el perfil del ciudadano con el estado de la
  conexión.
- **Panel derecho:** cabecera con el estado en vivo (*te escucho…*,
  *pensando…*, *hablando…*, *sin micrófono*), burbujas del usuario a la derecha
  y del agente a la izquierda, píldora de fecha, puntos animados mientras el
  agente piensa y una franja ámbar para los avisos.
- **Selector Voz | Texto** en la cabecera del chat (visible también en móvil):
  elige el transporte (WebSocket de Deepgram o `POST /api/public/voice/text`).
  Cambiar de modo cierra la sesión del otro modo y abre la del elegido; el
  historial no se toca (lo reanuda el backend por `device_id`). La elección se
  guarda en `localStorage` (`modo_interaccion`).
- **Barra inferior (modo Voz):** el botón del micrófono (apagado por defecto,
  con pulso cuando está abierto), el estado de la conversación y un botón
  circular para **interrumpir** la voz del agente (solo se activa mientras
  habla).
- **Composer (modo Texto):** campo de texto y botón de enviar en lugar del
  micrófono; el mensaje se envía con `Enter` o con el botón.
- **Móvil (< 768 px):** se muestra un panel cada vez; la flecha de la cabecera
  vuelve a la lista.

## Cómo funciona la conversación

La app tiene dos modos sobre la **misma** sesión del proxy: **Texto** (por
defecto, no pide micrófono) y **Voz**. Al abrirla conecta el agente sin abrir el
micrófono; en modo Voz lo activas con el botón de la barra inferior. Una vez
abierto, el **VAD del propio agente** cierra cada turno, así se habla con
naturalidad, como en un diálogo entre personas.

Al arrancar, la app hace `POST /api/public/voice/session` con
`Authorization: Bearer tk_live_…` (la lee de `.env` o del modal de
Configuración). El backend resuelve **tenant → canal de voz**, devuelve el
nombre del bot, el saludo y un **ticket de 60 s**, y la app abre
`WS /ws/voice/channel/{id}?ticket=…&device_id=…` (módulo `src/agent.rs`).

El proxy manda `meta` (identidad), conecta con Deepgram, y reenvía el
`Settings` que armó el servidor (escucha **nova-3** en español, prompt/tools
del bot y voz del canal). Al recibir `SettingsApplied` se habilita el botón del
micrófono (antes de eso el agente no acepta audio); la captura empieza cuando
lo pulsas. El frame `Settings` del cliente se ignora: la configuración es del
servidor.

Mientras el botón está activo, el micrófono se captura en el webview con
`getUserMedia` + `ScriptProcessorNode` (bloques de 128 ms), **en continuo** y
también el silencio; `CONV_DRAIN` lo remuestrea a PCM 16 kHz (el dispositivo
puede ignorar el `sampleRate` pedido) y una bomba lo manda al agente cada
100 ms. No hay VAD propio ni silencio añadido: el agente decide el fin de turno
al detectar la pausa, igual que el starter de FastAPI que funciona.

La voz del agente llega en tramas binarias PCM de 24 kHz y se reproduce **en
streaming**: cada trama se programa en el reloj del `AudioContext`
(`window.dgAudioPush`) con 150 ms de colchón, de modo que los trozos suenan
pegados y el primero se oye en cuanto llega (no se espera a `AgentAudioDone`).

### Barge-in (interrumpir al agente)

Cuando el proxy reenvía `UserStartedSpeaking`, la UI corta lo que ya tenía
programado (`window.dgAudioStop`) y descarta las tramas en vuelo: puedes hablar
encima del agente y él deja de sonar. `getUserMedia` usa `echoCancellation`,
`noiseSuppression` y `autoGainControl` para que la voz del altavoz no vuelva por
el micrófono.

### Modo Texto (sin Deepgram)

En modo **Texto** el cliente **no abre el WebSocket**: cada mensaje va a
`POST /api/public/voice/text` con la API key del tenant y el `device_id`
(módulo `src/texto.rs`). El backend corre el mismo agente del bot (prompt,
tools, RAG e historial) con `AgentService.run_turn`, sin STT, sin TTS y sin
minutos de agente; responde la respuesta completa en JSON (ADR-040 de
gestion.ar). Por eso:

- se **ahorra** la sesión de Deepgram y no hay audio que descartar;
- funciona con el permiso de micrófono denegado o sin dispositivo de audio;
- la burbuja del usuario se pinta de forma local (el endpoint devuelve sólo la
  respuesta) y el texto del agente cuando llega el JSON.

Al pasar a Texto se apaga el micrófono si estaba abierto, se corta la voz
programada (`window.dgAudioStop`) y se cierra el socket del agente; al volver a
Voz se pide un ticket nuevo. El modo se guarda en `localStorage`
(`modo_interaccion` = `"voz"` | `"texto"`) y se restaura al arrancar (por
defecto, Texto).

El backend/proxy responde con:

- `meta` → nombre del bot y del tenant (y saludo), para la cabecera.
- `ConversationText` (`role: "user"`) → tu mensaje transcrito del micrófono
  (la burbuja lleva el icono de micrófono; en modo Texto el turno no pasa por
  este socket).
- `ConversationText` (`role: "assistant"`) → respuesta del LLM (burbuja del agente).
- Tramas binarias → la voz del agente (24 kHz), reproducida en streaming;
  no queda en el historial.
- `UserStartedSpeaking` / `AgentThinking` / `AgentStartedSpeaking` /
  `AgentAudioDone` → estado de la conversación (subtítulo y barra).
- `Error` / `Warning` → se registran en el log (stderr/logcat), no en el chat.

Si la conexión se cae, la app reconecta sola y lo registra en el log (el chat
queda limpio: solo el diálogo); el micrófono conserva su estado y el audio se
descarta mientras no hay agente.

> La arquitectura es productor/consumidor: la tarea de tokio posee el socket y
> habla con la UI por dos canales (`std::sync::mpsc` de eventos y
> `tokio::sync::mpsc` de comandos), así nunca bloqueas el hilo de la interfaz.

## 1. Requisitos

- Rust estable (`rustup` desde https://rustup.rs)
- [Android Studio](https://developer.android.com/studio) con **SDK** y **NDK** instalados
- **Java 21** (`/usr/lib/jvm/java-21-openjdk`). El wrapper de Gradle de `dx`
  (9.1) no arranca con el JDK 26 del sistema.
- Dioxus CLI 0.7 (la misma versión que el `dioxus` del proyecto):

```bash
cargo install dioxus-cli --version "^0.7"   # o el `dx` 0.7 ya instalado
dx --version                                # dioxus 0.7.10
```

- Un backend de **gestion.ar** con un canal `voice` activo y la
  `DEEPGRAM_API_KEY` configurada en el servidor.
- La **API key del tenant** (`tk_live_…`), emitida por el super_admin
  (`POST /api/admin/tenants/{id}/api-key`). No hace falta ninguna key de
  Deepgram ni de LLM en el dispositivo.

## 2. Ejecutar en Android (desarrollo)

```bash
cd deepgram_chat
dx serve --platform android
```

`dx serve` a secas también funciona: deduce la plataforma de las features del
`Cargo.toml` (`default = ["desktop"]`).

## 3. Generar el APK

Para instalar en un emulador (x86_64) o en un dispositivo (arm64):

```bash
JAVA_HOME=/usr/lib/jvm/java-21-openjdk dx build --platform android
JAVA_HOME=/usr/lib/jvm/java-21-openjdk dx build --platform android --target aarch64-linux-android
```

> **Si el móvil dice «app no instalada» / «no compatible»**, casi siempre es la
> **ABI**: el primer comando compila para el triple del host (x86_64) y ese APK
> solo vale para el emulador. Compruébalo:
>
> ```bash
> "$ANDROID_HOME"/build-tools/*/aapt2 dump badging <apk> | grep native-code   # → native-code: 'arm64-v8a'
> adb shell getprop ro.product.cpu.abi                                       # → arm64-v8a
> ```
>
> Si el móvil es de 32 bits (raro hoy), compila con `--target armv7-linux-androideabi`.

`dx build` ejecuta `assembleDebug` y deja el APK instalable en
`$CARGO_TARGET_DIR/dx/deepgram-chat/debug/android/app/app/build/outputs/apk/debug/app-debug.apk`:

```bash
adb install -r "$CARGO_TARGET_DIR/dx/deepgram-chat/debug/android/app/app/build/outputs/apk/debug/app-debug.apk"
```

Para publicar en Play Store, `dx bundle` genera un **.aab** (release, con
`bundleRelease`) en `.../outputs/bundle/release/`:

```bash
JAVA_HOME=/usr/lib/jvm/java-21-openjdk dx bundle --platform android
```

> **Un `.aab` no se instala en el dispositivo**: es el formato de publicación de
> Play Store y `adb install` lo rechaza (*"filename doesn't end .apk or .apex"*).
> Para instalar en un móvil o emulador usa el **APK de depuración** de
> `dx build` (va firmado con la clave de depuración, por eso instala), o sube el
> `.aab` a Play. El `.aab` que produce `dx bundle` sale **sin firmar** (no tiene
> `META-INF/*.RSA`): lo firma Play al subirlo.
>
> Si necesitas repartir un **APK de release**, declara la firma y compila en
> release: `assemble_android` usa `assembleRelease` cuando hay configuración de
> firma (`dioxus-cli/src/build/android.rs`).
>
> ```toml
> # el nombre nuevo de esta sección es [android.signing]
> [bundle.android]
> jks_file = "release.keystore"
> jks_password = "…"
> key_alias = "upload"
> key_password = "…"
> ```
>
> ```bash
> JAVA_HOME=/usr/lib/jvm/java-21-openjdk dx build --platform android --release
> ```

> La arquitectura la decide el triple de compilación, que por defecto es el del
> host (x86_64 aquí). Para un móvil real añade `--target aarch64-linux-android`;
> cuál necesita el tuyo lo dice `adb shell getprop ro.product.cpu.abi`
> (`arm64-v8a` en los móviles actuales, `x86_64` solo en el emulador).
>
> **El APK acumula ABIs:** `dx` no limpia `app/src/main/jniLibs`, así que cada
> `dx build --target <triple>` **añade** su `libmain.so` sin borrar los
> anteriores. Un build x86_64 seguido de uno arm64 deja un APK universal
> (`native-code: 'arm64-v8a' 'x86_64'`, 169 MB en debug porque las `.so` van con
> `debug_info`): vale para móvil **y** emulador, pero pesa el doble. Para una
> sola ABI borra antes la carpeta sobrante (no hay `dx clean`):
>
> ```bash
> JNI="$CARGO_TARGET_DIR/dx/deepgram-chat/debug/android/app/app/src/main/jniLibs"
> rm -rf "$JNI/x86_64"        # deja solo arm64-v8a
> ```
>
> Comprobación del APK resultante (ABIs, permisos, firma):
>
> ```bash
> "$ANDROID_HOME"/build-tools/*/aapt2 dump badging <apk> | grep -E "native-code|uses-permission"
> "$ANDROID_HOME"/build-tools/*/apksigner verify --print-certs -v <apk>
> ```
>
> **`dx` necesita Java 21**: su wrapper de Gradle (9.1) no arranca con el JDK 26.
> Y ojo con `adb install`: hay que desinstalar la versión anterior si cambias el
> `identifier` (Android la trata como otra app).

Para comprobar los permisos que acabaron dentro del APK:

```bash
"$ANDROID_HOME"/build-tools/*/aapt2 dump permissions <ruta-al-apk>
```

## 4. Uso

1. Configurá el **`.env`** de la raíz:

```bash
BACKEND_URL=http://127.0.0.1:8000
# BACKEND_PORT=8000   # opcional: sobrescribe el puerto de BACKEND_URL
TENANT_KEY=tk_live_…  # opcional: si falta, la app la pide en ⚙️
```

`build.rs` lo lee al compilar y lo deja en el binario (también en el APK de
Android). En escritorio, un `.env` de runtime (directorio actual o carpeta de
datos) lo sobrescribe sin recompilar. Si `TENANT_KEY` está vacía, la app abre el
modal ⚙️ y guarda la key en `localStorage`.

2. **API key del tenant**: te la entrega el administrador de gestion.ar una
   sola vez (`POST /api/admin/tenants/{id}/api-key`). Con ella la app resuelve
   el tenant y su canal de voz. Una key inválida, revocada o de un tenant
   suspendido devuelve `401` y se muestra en la franja de aviso.
3. **Conversación:** la app pide el ticket, conecta el proxy y habilita el
   botón del micrófono al recibir `SettingsApplied`; pulsa el micrófono para
   hablar. Una vez activo, deja que el VAD del agente cierre cada turno. El
   subtítulo muestra «te escucho…», «pensando…» o «hablando…» y la barra
   indica el estado; puedes interrumpir al agente hablando encima. La voz se
   oye en streaming; en el historial local solo quedan los textos transcritos.
   Con el selector en **Texto** escribes en el composer y no se reproduce
   audio ni se abre el socket de voz (el turno va por HTTP); el modo se
   recuerda entre arranques y por defecto es Texto.

### Identidad y herramientas

- La app genera un `device_id` (UUID) la primera vez y lo persiste en
  `localStorage`; viaja en el bootstrap y en el WS. El backend lo usa como
  `clients.external_id` (`source='voice'`) para reanudar el historial de esa
  conversación.
- El **prompt, las tools y la voz** los define el bot/canal de gestion.ar. La
  app ya no tiene catálogo de plugins ni ejecuta function calling: las tools
  corren en el proxy (calificación por semáforo, fuentes públicas y RAG).
- Al reconectar, el backend arma `agent.context.messages` con los últimos
  turnos guardados en `conversations`/`messages`.

Con el móvil por USB:

```bash
adb reverse tcp:8000 tcp:8000   # el 127.0.0.1 del teléfono llega al PC
```

`reqwest` es código nativo (no pasa por el WebView), así que el
`network_security_config` de `dx` no le afecta; en producción usa **HTTPS**
(la `TENANT_KEY` no debe viajar por HTTP fuera de la LAN de desarrollo).

## 5. Permisos de Android

La app graba con `getUserMedia`,
que en el WebView exige el permiso `RECORD_AUDIO` (wry lo pide en
`onPermissionRequest`; sin la declaración, Android lo deniega). Se declara en `Dioxus.toml` y `dx` lo inyecta en el
manifest generado en cada build:

```toml
[permissions.microphone]
description = "Hablar con el agente de voz de Deepgram"

# OJO: `RustWebChromeClient.onPermissionRequest` (generado por `dx`) pide
# `[MODIFY_AUDIO_SETTINGS, RECORD_AUDIO]` y solo llama a `request.grant()` si
# TODOS vienen concedidos. Un permiso que no esté en el manifest se resuelve
# como `false` sin diálogo, así que sin esta entrada `getUserMedia` falla
# aunque el usuario acepte el diálogo del micrófono.
[android.permissions]
"android.permission.MODIFY_AUDIO_SETTINGS" = { description = "Grabar audio en las llamadas al WebView" }
```

Comprobado con `aapt2 dump permissions` sobre el APK:

```
uses-permission: name='android.permission.RECORD_AUDIO'
uses-permission: name='android.permission.MODIFY_AUDIO_SETTINGS'
uses-permission: name='android.permission.INTERNET'
```

Y de punta a punta en un emulador API 36 con instalación limpia el diálogo
«Allow DeepgramChat to record audio?» aparece y, al aceptarlo,
`navigator.mediaDevices.getUserMedia({audio:true})` resuelve con una pista viva.
La sonda se lanza desde el DevTools del WebView (wry lo habilita en builds
debug) sin tocar el código:

```bash
adb forward tcp:9222 localabstract:webview_devtools_remote_$(adb shell pidof com.tuusuario.deepgramchat)
curl -s http://127.0.0.1:9222/json   # → webSocketDebuggerUrl → Runtime.evaluate
```

> Si el usuario deniega el diálogo dos veces, Android 11+ lo convierte en
> denegación permanente: hay que concederlo a mano en Ajustes → Apps →
> Deepgram Chat → Permisos → Micrófono. La barra muestra «🎤 Sin micrófono —
> revisa el permiso y pulsa Guardar» y la app deja de reconectar (sin audio
> el agente cerraría por timeout) hasta que se reintente desde ⚙️.

> En Dioxus 0.6 esto no era posible: el manifest se regeneraba desde una
> plantilla fija sin permisos y había que parchearlo a mano tras cada build.

## 6. Notas técnicas

- **TLS:** `tokio-tungstenite` se configura con `rustls-tls-webpki-roots` y se
  fuerza el proveedor **`ring`** (`rustls = { features = ["ring"] }`), más
  fiable al compilar para el NDK de Android que `aws-lc-rs`.
- **Protocolo del proxy:** la app **no** envía `Settings` ni conoce el de
  Deepgram. El proxy manda `meta` (identidad), reenvía el `Settings` que armó
  el servidor y `SettingsApplied` habilita el botón del micrófono. Los textos
  llegan como `ConversationText` con `role` (`user`/`assistant`). Los frames
  `Settings`/`UpdateThink`/`UpdatePrompt`/`UpdateSpeak` del cliente se ignoran.
  El contrato de Deepgram vive en `gestion.ar`
  (`app/services/voice_session_service.py`, ADR-034).
- **`dioxus::document::eval`:** el script se ejecuta como cuerpo de una función
  `async`, así que el valor que quieres leer en Rust hay que devolverlo con
  `return` (una expresión suelta o una IIFE resuelve `undefined` y el `eval`
  falla con "Failed to stringify result").
- **Barras del sistema (edge-to-edge):** con `targetSdk = 35` Android 15+ pinta
  la ventana a pantalla completa bajo la barra de estado y la de navegación, de
  modo que la cabecera quedaba pegada al reloj/batería y la barra de escritura
  debajo de la píldora de gestos. Se arregla con `android/MainActivity.kt`,
  declarada en `Dioxus.toml` con `[application] android_main_activity` (`dx`
  reescribe el `MainActivity.kt` de `dev.dioxus.main` en cada build):
  `setDecorFitsSystemWindows(false)` + padding por insets (`systemBars`,
  `displayCutout` e `ime`), el hueco pintado con el mismo `#202c33` de
  `.header`/`.inputbar` e iconos claros en ambas barras. Dos detalles que
  costaron una iteración, ambos comprobados en el emulador:
  - el padding va en el **contenedor** (`android.R.id.content`), no en el
    `WebView`: Chromium pinta su contenido sobre las bounds completas del View
    e ignora su padding (la captura no cambiaba ni un píxel);
  - `onWebViewCreate` se llama con `parent == null` (el WebView todavía no está
    en la jerarquía), así que el contenedor se resuelve con
    `window.decorView.findViewById(android.R.id.content)`.
- **Micrófono y reproducción en streaming:** `document::eval` ejecuta sus
  scripts en el mismo `window`, así que `AUDIO_JS` instala
  `window.dgAudioPush`/`dgAudioStop` una vez al arrancar. El micrófono deja
  sus bloques en `window.dgMic.cola` (sin VAD propio, con `echoCancellation`)
  y Rust los drena cada 100 ms con `CONV_DRAIN`, que remuestrea a 16 kHz
  conservando la muestra y la fase fraccional para no meter clics entre
  bloques. La voz del agente se programa contra `AudioContext.currentTime`
  con 150 ms de colchón (el mismo esquema del starter de FastAPI), así que no
  hay huecos entre tramas; con el socket caído el audio del micro se descarta.
- **LLM del agente:** lo elige el backend (`VOICE_THINK`: gestionado por
  Deepgram o BYO deepseek/claude). La app no tiene proveedor, endpoint, modelo
  ni key de LLM.
- **Avisos del agente:** `Warning`/`Error` conservan su `code`
  (`THINK_REQUEST_FAILED`, `FAILED_TO_THINK`, …) además de la descripción; la
  UI los muestra en la franja `.aviso` y en el subtítulo, y el texto completo
  queda en stderr/logcat. Los errores del bootstrap (`401`/`409`) también van
  a esa franja.
- **Ajustes por usuario:** sólo la `TENANT_KEY` (si no vino en `.env`) y el
  `device_id` viven en el `localStorage` del webview. En Android esa carpeta
  la gestiona el sistema, así que sobrevive a las actualizaciones; en
  escritorio la app fija `$XDG_DATA_HOME/deepgram-chat`, porque `dx` nombra el
  binario de desarrollo con un hash (`deepgram-chat-4d87e9c5`) y WebKitGTK
  derivaba de ese nombre dónde guardar los datos, de modo que cada rebuild
  dejaba los ajustes huérfanos.
- **Migración 0.6 → 0.7:** el permiso de micrófono se declara en `Dioxus.toml`
  en vez de parchear el manifest generado. (El único API que cambió en este
  código fue `FormData::files()`; al quitar las notas de voz ya no se usa el
  selector de archivos.)

## Estructura

```
deepgram_chat/
├── .env              # BACKEND_URL + TENANT_KEY (build.rs los fija al compilar)
├── build.rs          # Lee .env y exporta DEEPGRAM_BACKEND_URL/DEEPGRAM_TENANT_KEY
├── Cargo.toml        # Dependencias (dioxus 0.7, tokio-tungstenite, reqwest…)
├── Dioxus.toml       # Nombre, identificador, permisos (micrófono) y MainActivity propia
├── android/
│   └── MainActivity.kt   # Insets: separación con las barras del sistema
└── src/
    ├── main.rs       # UI de chat (dos paneles) + estado + captura de micrófono (JS)
    ├── config.rs     # Backend/key/URLs y lectura del .env de runtime
    └── agent.rs      # Cliente WebSocket del proxy de voz de gestion.ar
```
