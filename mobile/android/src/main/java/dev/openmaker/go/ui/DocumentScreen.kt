package dev.openmaker.go.ui

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.net.Uri
import android.view.accessibility.AccessibilityManager
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.openmaker.go.DocumentSession
import dev.openmaker.go.Effect
import dev.openmaker.go.canvas.CanvasView
import dev.openmaker.go.engine.Command
import dev.openmaker.go.engine.DocumentMode
import dev.openmaker.go.engine.Input
import dev.openmaker.go.engine.Key

/**
 * An open file: the layout, drawn by the engine, between FileMaker Go's
 * bars, whose controls and menus the engine's state and FileMaker's own
 * menus drive.
 */
@Composable
fun DocumentScreen(session: DocumentSession, onClosed: (failure: String?) -> Unit) {
    val state by session.state.collectAsStateWithLifecycle()
    val menus by session.menus.collectAsStateWithLifecycle()
    val elements by session.elements.collectAsStateWithLifecycle()
    var canvas by remember { mutableStateOf<CanvasView?>(null) }
    // A command of the chrome takes what is typed as committed, never a
    // candidate the input method is still composing.
    val send: (Input) -> Unit = { input ->
        canvas?.commitComposition()
        session.send(input)
    }

    // Closed by Close File, a script or the host, or stopped by a failure.
    LaunchedEffect(state?.closed) { state?.takeIf { it.closed }?.let { onClosed(it.failure) } }
    Effects(session)
    ScreenReader(session)
    CanvasLifecycle(canvas)
    // A tap that leaves a field taking typing shows the keyboard again, if
    // the user put it away with Back.
    LaunchedEffect(canvas, state?.keyboard, state?.fieldTaps) { canvas?.showKeyboard(state?.keyboard == true) }
    LaunchedEffect(canvas, state?.caret) { canvas?.moveCaret(state?.caret) }
    // Return performs the find in Find mode, unless a dialog over it (a
    // script's) takes Return for its default button.
    LaunchedEffect(canvas, state?.mode, state?.canPerformFind) {
        canvas?.finding = state?.mode == DocumentMode.FIND && state?.canPerformFind == true
    }
    LaunchedEffect(canvas, elements) { canvas?.present(elements) }
    // The layout keeps the focus, so a hardware keyboard's Tab and arrows
    // reach the engine also when no field takes typing.
    LaunchedEffect(canvas) { canvas?.requestFocus() }
    // Back closes the file, as the Files button does; over one of the
    // file's dialogs it is Escape, which cancels the dialog.
    BackHandler {
        val shown = state
        if (shown?.canClose == true) send(Input.Run(Command.Close(shown.file)))
        else send(Input.Press(Key.ESCAPE, false, false))
    }

    Column(
        Modifier.fillMaxSize().background(MaterialTheme.colorScheme.surface).systemBarsPadding().imePadding()
    ) {
        DocumentTopBar(state, menus, send)
        AndroidView(
            factory = { CanvasView(it, session).also { view -> canvas = view } },
            modifier = Modifier.weight(1f).fillMaxWidth(),
        )
        if (state?.keyboard == true) EditingBar(state, send) else RecordBar(state, menus, send)
    }
}

/** What the file asked for: its copied text goes to the clipboard, its URLs open. */
@Composable
private fun Effects(session: DocumentSession) {
    val context = LocalContext.current
    LaunchedEffect(session) {
        session.effects.collect { effect ->
            when (effect) {
                is Effect.Copy ->
                    context
                        .getSystemService(ClipboardManager::class.java)
                        .setPrimaryClip(ClipData.newPlainText("FileMaker", effect.text))
                is Effect.OpenUrl -> {
                    val uri = Uri.parse(effect.url)
                    // Android refuses to hand another app a file: URL
                    // (FileUriExposedException), and the app's files are
                    // its own: such a URL is not opened.
                    if (!uri.scheme.equals("file", ignoreCase = true)) {
                        try {
                            context.startActivity(Intent(Intent.ACTION_VIEW, uri))
                        } catch (_: ActivityNotFoundException) {
                            // FileMaker Go shows nothing when no app opens a URL.
                        } catch (_: RuntimeException) {
                            // Nor when the system refuses it (a URI it will not
                            // expose, an app that may not be started).
                        }
                    }
                }
            }
        }
    }
}

/** Accessibility elements are made only while TalkBack explores the screen. */
@Composable
private fun ScreenReader(session: DocumentSession) {
    val manager = LocalContext.current.getSystemService(AccessibilityManager::class.java)
    DisposableEffect(session, manager) {
        val listener =
            AccessibilityManager.TouchExplorationStateChangeListener { session.send(Input.Accessibility(it)) }
        session.send(Input.Accessibility(manager.isTouchExplorationEnabled))
        manager.addTouchExplorationStateChangeListener(listener)
        onDispose { manager.removeTouchExplorationStateChangeListener(listener) }
    }
}

/** A GLSurfaceView pauses and resumes its rendering thread with the activity. */
@Composable
private fun CanvasLifecycle(canvas: CanvasView?) {
    val owner = LocalLifecycleOwner.current
    DisposableEffect(owner, canvas) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_RESUME -> canvas?.onResume()
                Lifecycle.Event.ON_PAUSE -> canvas?.onPause()
                else -> Unit
            }
        }
        owner.lifecycle.addObserver(observer)
        onDispose { owner.lifecycle.removeObserver(observer) }
    }
}
