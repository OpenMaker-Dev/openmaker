package dev.openmaker.go

import dev.openmaker.go.canvas.TextureStore
import dev.openmaker.go.engine.Batch
import dev.openmaker.go.engine.Document
import dev.openmaker.go.engine.DocumentObserver
import dev.openmaker.go.engine.DocumentState
import dev.openmaker.go.engine.Element
import dev.openmaker.go.engine.Frame
import dev.openmaker.go.engine.Input
import dev.openmaker.go.engine.Menu
import dev.openmaker.go.engine.Viewport
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/** Tells whichever canvas shows the document that a frame waits. */
class FrameSignal : DocumentObserver {
    @Volatile var listener: (() -> Unit)? = null

    // Called on the document's own thread.
    override fun frameReady() {
        listener?.invoke()
    }
}

/** What the last frame drew, to draw again when the canvas is redrawn. */
class Geometry(
    val viewport: Viewport,
    val vertices: ByteArray,
    val indices: ByteArray,
    val batches: List<Batch>,
)

/** Something the file asked the app to do. */
sealed interface Effect {
    data class Copy(val text: String) : Effect

    data class OpenUrl(val url: String) : Effect
}

/**
 * An open file. It outlives the activity and the canvas, so a recreated
 * canvas, whose OpenGL context is new, draws the last frame again from
 * [geometry] and [textures]: the engine sends each texture only once.
 */
class DocumentSession(val entry: Entry, private val document: Document, val signal: FrameSignal) {
    private val _state = MutableStateFlow<DocumentState?>(null)
    val state: StateFlow<DocumentState?> = _state
    private val _menus = MutableStateFlow<List<Menu>>(emptyList())
    val menus: StateFlow<List<Menu>> = _menus
    private val _elements = MutableStateFlow<List<Element>>(emptyList())
    val elements: StateFlow<List<Element>> = _elements
    val effects = MutableSharedFlow<Effect>(extraBufferCapacity = 16, onBufferOverflow = BufferOverflow.DROP_OLDEST)

    val textures = TextureStore()
    @Volatile var geometry: Geometry? = null
        private set
    // The textures the last frame released. Its geometry, drawn again until
    // the next frame (a resize, a new OpenGL context), may still use them.
    private var released: List<ULong> = emptyList()
    // A canvas or a control can still reach the document after the app let
    // go of it: what it sends then goes nowhere.
    @Volatile private var closed = false

    fun send(input: Input) {
        if (!closed) engine { document.send(input) }
    }

    /**
     * Take the waiting frame, if any: its textures go to [textures], its
     * geometry to [geometry], and the rest to the flows the chrome reads.
     * Called on the canvas's rendering thread.
     */
    fun takeFrame(): Frame? {
        if (closed) return null
        val frame = engine { document.takeFrame() } ?: return null
        textures.free(released)
        released = frame.freed
        textures.apply(frame.textures)
        geometry = Geometry(frame.viewport, frame.vertices, frame.indices, frame.batches)
        _state.value = frame.state
        frame.menus?.let { _menus.value = it }
        frame.elements?.let { _elements.value = it }
        frame.copied?.let { effects.tryEmit(Effect.Copy(it)) }
        frame.openUrls.forEach { effects.tryEmit(Effect.OpenUrl(it)) }
        return frame
    }

    /**
     * Let go of the engine. A file still open is closed by the engine as
     * Close File closes it, on its own thread, unless a close was just
     * refused (a trigger, or changes only the user can let go of).
     */
    fun release() {
        closed = true
        signal.listener = null
        document.destroy()
    }

    /** A call that lost the race with [release] does nothing. */
    private fun <T> engine(call: () -> T): T? =
        try {
            call()
        } catch (_: IllegalStateException) {
            null
        }
}
