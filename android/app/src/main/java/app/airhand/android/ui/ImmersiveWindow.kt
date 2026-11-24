package app.airhand.android.ui

import android.os.Build
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
val LocalSessionWindow = staticCompositionLocalOf<Window?> { null }

fun Window.hideSessionSystemBars() {
    WindowCompat.getInsetsController(this, decorView).apply {
        systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        hide(WindowInsetsCompat.Type.systemBars())
    }
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
        // 旧系统在 Dialog 移除时先按 Window 属性选取系统栏状态，再通知 View 恢复焦点。
        // 将同一请求写到 Window，避免只设置 DecorView、等待下次遍历才同步的间隙。
        // 不加 FLAG_FULLSCREEN，保留旧系统 adjustResize / IME 的布局处理。
        @Suppress("DEPRECATION")
        val flags = decorView.systemUiVisibility
        val attrs = attributes
        @Suppress("DEPRECATION")
        if (attrs.systemUiVisibility != flags) {
            attrs.systemUiVisibility = flags
            attributes = attrs
        }
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
    val sessionWindow = LocalSessionWindow.current
    DisposableEffect(window, sessionWindow) {
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

            override fun onViewDetachedFromWindow(v: View) {
                sessionWindow?.hideSessionSystemBars()
                unwatchFocus()
            }
        }
        decor.addOnAttachStateChangeListener(attachment)
        if (decor.isAttachedToWindow) watchFocus()
        sessionWindow?.hideSessionSystemBars()
        window.hideSessionSystemBars()
        onDispose {
            // 在销毁流程中重申主窗口状态；不只依赖稍后的恢复焦点回调。
            sessionWindow?.hideSessionSystemBars()
            decor.removeOnAttachStateChangeListener(attachment)
            unwatchFocus()
        }
    }
}
