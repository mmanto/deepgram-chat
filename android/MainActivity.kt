package dev.dioxus.main

import android.os.Bundle
import android.view.View
import android.webkit.WebView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

// Debe coincidir con `[bundle] identifier` de Dioxus.toml: Logger.kt usa
// BuildConfig.DEBUG. Este archivo sustituye a la plantilla de `dx`.
typealias BuildConfig = com.tuusuario.deepgramchat.BuildConfig

/**
 * MainActivity propia (se declara en Dioxus.toml: `[application]
 * android_main_activity`) para separar la UI de la app de los menús del
 * sistema.
 *
 * Con `targetSdk = 35` Android 15+ ignora `fitsSystemWindows` y pinta la
 * ventana a pantalla completa bajo la barra de estado y la de navegación, así
 * que la cabecera (🤖/⚙️) quedaba pegada al reloj/batería y la barra de
 * escritura debajo de la píldora de gestos. Aquí se reserva ese espacio en
 * cualquier versión de Android: el hueco se pinta con el color de la cabecera
 * y de la barra de escritura (#202c33) y las barras llevan iconos claros.
 */
class MainActivity : WryActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Los insets los aplicamos nosotros —incluido el del teclado—, así que
        // el sistema no tiene que redimensionar ni ajustar nada.
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            // Sobre #202c33 los iconos tienen que ser claros (no "light bars").
            isAppearanceLightStatusBars = false
            isAppearanceLightNavigationBars = false
        }
    }

    /** `WryActivity` lo llama al crear el WebView, aún sin padre en la jerarquía. */
    override fun onWebViewCreate(webView: WebView) {
        webView.setBackgroundColor(FONDO_BARRAS)
        // El padding va en el contenedor, no en el WebView: Chromium pinta su
        // contenido sobre las bounds completas del View e ignora su padding.
        val contenedor: View = webView.parent as? View
            ?: window.decorView.findViewById(android.R.id.content)
            ?: return
        contenedor.setBackgroundColor(FONDO_BARRAS)
        ViewCompat.setOnApplyWindowInsetsListener(contenedor) { view, insets ->
            val barras = insets.getInsets(
                WindowInsetsCompat.Type.systemBars()
                    or WindowInsetsCompat.Type.displayCutout()
                    or WindowInsetsCompat.Type.ime(),
            )
            view.setPadding(barras.left, barras.top, barras.right, barras.bottom)
            insets
        }
        ViewCompat.requestApplyInsets(contenedor)
    }

    private companion object {
        /** #202c33, el color de `.header` y `.inputbar` en `src/main.rs`. */
        const val FONDO_BARRAS: Int = 0xFF202C33.toInt()
    }
}
