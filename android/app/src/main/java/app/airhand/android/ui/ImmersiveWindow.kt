package app.airhand.android.ui

import android.view.View
import android.view.ViewTreeObserver
import android.view.Window
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.window.DialogWindowProvider
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

/** 只在远控界面启用；首页和设置页的共用对话框保留系统栏。 */
val LocalSessionImmersive = staticCompositionLocalOf { false }

fun Window.hideSessionSystemBars() {
    WindowCompat.getInsetsController(this, decorView).apply {
        systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        hide(WindowInsetsCompat.Type.systemBars())
    }
}

/**
 * 在弹窗内容中调用，操作 Dialog 自己的 Window，不能从 LocalContext 取 Activity 的 Window。
 * 内容首次组合即提交隐藏请求，并在附着、恢复焦点时重申；不监听 Insets 的每次变化，
 * 保留用户从屏幕边缘临时滑出系统栏的行为，也不隐藏 IME。
 */
@Composable
fun SessionDialogImmersive() {
    if (!LocalSessionImmersive.current) return
    val view = LocalView.current
    val window = (view.parent as? DialogWindowProvider)?.window ?: return
    DisposableEffect(window) {
        val decor = window.decorView
        var observer: ViewTreeObserver? = null
        val focus = ViewTreeObserver.OnWindowFocusChangeListener { focused ->
            if (focused) window.hideSessionSystemBars()
        }
        fun unwatchFocus() {
            observer?.takeIf { it.isAlive }?.removeOnWindowFocusChangeListener(focus)
            observer = null
        }
        fun watchFocus() {
            unwatchFocus()
            observer = decor.viewTreeObserver.also { it.addOnWindowFocusChangeListener(focus) }
        }
        val attachment = object : View.OnAttachStateChangeListener {
            override fun onViewAttachedToWindow(v: View) {
                watchFocus()
                window.hideSessionSystemBars()
            }

            override fun onViewDetachedFromWindow(v: View) = unwatchFocus()
        }
        decor.addOnAttachStateChangeListener(attachment)
        if (decor.isAttachedToWindow) watchFocus()
        window.hideSessionSystemBars()
        onDispose {
            decor.removeOnAttachStateChangeListener(attachment)
            unwatchFocus()
        }
    }
}
