# Deepgram Chat 🎙️🤖

App de chat estilo WhatsApp hecha con **Rust + Dioxus 0.7**, desplegable en
**Android**, que se comunica con las APIs de **Deepgram**:

| Acción | API de Deepgram usada |
|---|---|
| Enviar una **nota de voz** (🎙️) | `POST /v1/listen` — transcripción con **Nova 3** (español); en móvil se graba dentro de la app |
| Enviar un **mensaje de texto** | `POST /v1/speak` — respuesta hablada con voz **Aura** (español, `aura-2-celeste-es`) |
| **Modo agente** (🤖) — conversación de voz con LLM | `wss://agent.deepgram.com/v1/agent/converse` — **Voice Agent API** (WebSocket): STT + LLM + TTS en tiempo real |

> Las voces Aura se eligen por `model=<voz>` y el idioma va en el nombre: en
> español existen `aura-2-celeste-es`, `aura-2-estrella-es`, `aura-2-nestor-es`,
> `aura-2-sirio-es`… pero **no** `aura-2-thalia-es` (`thalia` solo existe en
> inglés). El catálogo está en https://developers.deepgram.com/docs/tts-models

## Cómo funciona el modo agente (🤖)

1. Pulsa 🤖 en la cabecera: la app abre un WebSocket con el Agent API de
   Deepgram (módulo `src/agent.rs`) y envía un mensaje `Settings` con escucha
   **nova-3** en español, el LLM que elijas (por defecto `gpt-4o-mini`; con key
   propia se usa el `endpoint` BYO de OpenAI) y voz **aura-2-celeste-es**.
2. Pulsa 🎙️ y habla; el micrófono se captura en el webview con
   `getUserMedia` + `ScriptProcessorNode`, se **remuestrea a PCM 16 kHz** (el
   dispositivo puede ignorar el `sampleRate` pedido) y al pulsar ⏹ se envía
   como trama binaria, seguida de ~700 ms de silencio para que el VAD del
   agente cierre el turno.
3. Deepgram responde con:
   - `ConversationText` (`role: "user"`) → tu mensaje transcrito (burbuja 🎤).
   - `ConversationText` (`role: "assistant"`) → respuesta del LLM (burbuja del agente).
   - Tramas binarias + `AgentAudioDone` → la voz del agente, envuelta en un
     WAV (24 kHz) y reproducible en el chat 🔊.
   - `Error` / `Warning` → se muestran en el chat.
4. Pulsa 🤖 de nuevo para desconectar.

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

- API key de Deepgram (crédito gratis en https://console.deepgram.com)
- Para el modo 🤖: una key de LLM compatible OpenAI (p. ej. de
  https://platform.openai.com) y el modelo correspondiente.

## 2. Ejecutar en Android (desarrollo)

```bash
cd deepgram-chat
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

1. Abre la app y pulsa ⚙️ (arriba a la derecha).
2. Pega tu **API key de Deepgram**.
3. Elige el **proveedor del LLM** del agente 🤖 y rellena lo que pida:

   | Proveedor | Qué pide |
   |---|---|
   | **DeepSeek** (por defecto) | tu API key · `https://api.deepseek.com/v1/chat/completions` · `deepseek-chat` |
   | Deepgram (gestionado) | nada: no hace falta key propia |
   | OpenAI / Groq | tu API key |
   | Anthropic | tu API key (se manda como `x-api-key`) |
   | Personalizado | la URL del endpoint compatible con OpenAI, el modelo y la key |

   Al elegir un proveedor se rellenan su endpoint y su modelo por defecto, y
   puedes editarlos a mano (por ejemplo un gateway propio). Todo —claves,
   proveedor, endpoint y modelo— se guarda **por usuario en el dispositivo** y
   se recupera al abrir la app.
4. Pulsa **Guardar**. Si un proveedor necesita key y falta, al pulsar 🤖 la app
   abre los ajustes y avisa en el chat en vez de fallar en silencio.
5. **Texto:** escribe y pulsa ➤ — Deepgram te responde con un mensaje de voz (Aura).
6. **Nota de voz:** pulsa 🎙️ para grabar y ⏹ para enviar. En **Android/iOS**
   la nota se graba **dentro de la app** (`getUserMedia` → PCM 16 kHz → WAV) y
   se envía a `/v1/listen`; en **escritorio** se abre el selector de audio del
   sistema. En ambos casos la transcripción (Nova 3) se responde en voz alta.

   > No se usa `<input type=file capture="microphone">` en móvil a propósito:
   > ese control depende de que el sistema tenga una app grabadora que atienda
   > el intent, y en muchos Android (y emuladores) no existe, de modo que el
   > botón quedaba inoperante. La grabación interna usa el mismo capturador
   > (`getUserMedia` + `ScriptProcessorNode`) que el modo agente 🤖.
7. **Agente 🤖:** pulsa 🤖 para conectar, luego 🎙️ para hablar y ⏹ para
   enviar el turno. Las respuestas del agente llegan como texto y como audio.

> DeepSeek (y cualquier gateway compatible con *chat completions* de OpenAI) se
> declara como `provider.type: "open_ai"` apuntando `think.endpoint.url` a su
> servicio, que es lo que hace la app:
> https://developers.deepgram.com/docs/voice-agent-llm-models#passing-a-custom-byo-llm-through-a-cloud-provider

## 5. Permisos de Android

El **modo agente** y las **notas de voz** (en móvil) graban con `getUserMedia`,
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
> Deepgram Chat → Permisos → Micrófono. El aviso del chat («⚠️ No se pudo
> acceder al micrófono») cubre ese caso.

> En Dioxus 0.6 esto no era posible: el manifest se regeneraba desde una
> plantilla fija sin permisos y había que parchearlo a mano tras cada build.

## 6. Notas técnicas

- **TLS:** `tokio-tungstenite` se configura con `rustls-tls-webpki-roots` y se
  fuerza el proveedor **`ring`** (`rustls = { features = ["ring"] }`), más
  fiable al compilar para el NDK de Android que `aws-lc-rs`.
- **Protocolo del agente:** el mensaje inicial debe ser `{"type": "Settings", …}`
  y `listen`/`think`/`speak` exigen un objeto `provider` (`{"type": "deepgram",
  "model": …}`); el prompt del sistema va en `think.prompt` y
  `audio.output.container` solo admite `none`/`wav`/`ogg`. Los textos llegan
  como `ConversationText` con `role` (`user`/`assistant`). Esquema:
  https://developers.deepgram.com/reference/voice-agent/voice-agent
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
- **Proveedores de LLM:** la tabla de presets está en `agent::PROVEEDORES`
  (id, `provider.type`, endpoint, cabecera de autenticación y modelo por
  defecto). Añadir uno nuevo es una entrada más; el `Settings` que se envía se
  construye en `agent::configuracion` y está cubierto por tests
  (`settings_deepseek_usa_endpoint_compatible_openai`,
  `settings_anthropic_autentica_con_x_api_key`, …). Ver
  https://developers.deepgram.com/docs/voice-agent-llm-models.
- **Ajustes por usuario:** claves, proveedor, endpoint y modelo viven en el
  `localStorage` del webview (nunca en el repo). En Android esa carpeta la
  gestiona el sistema, así que sobrevive a las actualizaciones; en escritorio
  la app fija `$XDG_DATA_HOME/deepgram-chat`, porque `dx` nombra el binario de
  desarrollo con un hash (`deepgram-chat-4d87e9c5`) y WebKitGTK derivaba de ese
  nombre dónde guardar los datos, de modo que cada rebuild dejaba los ajustes
  huérfanos.
- **Migración 0.6 → 0.7:** el único API que cambió en este código fue
  `FormData::files()`, que pasó de devolver un `FileEngine` a un `Vec<FileData>`
  (`archivo.read_bytes().await` en lugar de `read_file(&nombre).await`). El
  resto (señales, `spawn`, `use_future`, `document::eval`) sigue igual, salvo
  que el permiso de micrófono ahora se declara en `Dioxus.toml` en vez de
  parchear el manifest generado.

## Estructura

```
deepgram-chat/
├── Cargo.toml        # Dependencias (dioxus 0.7, reqwest, tokio-tungstenite…)
├── Dioxus.toml       # Nombre, identificador, permisos (micrófono) y MainActivity propia
├── android/
│   └── MainActivity.kt   # Insets: separación con las barras del sistema
└── src/
    ├── main.rs       # UI estilo WhatsApp + estado + captura de micrófono (JS)
    ├── deepgram.rs   # Cliente REST: STT (/listen) y TTS (/speak)
    └── agent.rs      # Cliente WebSocket del Voice Agent (converse)
```
