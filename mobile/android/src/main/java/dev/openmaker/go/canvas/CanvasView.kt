package dev.openmaker.go.canvas

import android.annotation.SuppressLint
import android.content.ClipboardManager
import android.content.Context
import android.content.pm.ApplicationInfo
import android.graphics.Matrix
import android.opengl.GLSurfaceView
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.VelocityTracker
import android.view.ViewConfiguration
import android.view.inputmethod.CursorAnchorInfo
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import dev.openmaker.go.DocumentSession
import dev.openmaker.go.engine.Caret
import dev.openmaker.go.engine.Command
import dev.openmaker.go.engine.Element
import dev.openmaker.go.engine.Input
import dev.openmaker.go.engine.Key
import dev.openmaker.go.engine.Viewport
import kotlin.math.abs

/**
 * The layout: the engine's frames, drawn by [FramePainter], and the
 * finger, keyboard and screen reader input that goes back to the engine.
 * Positions go to the engine in dp, the points egui lays out in.
 */
@SuppressLint("ViewConstructor")
class CanvasView(context: Context, private val session: DocumentSession) : GLSurfaceView(context) {
    private val density = resources.displayMetrics.density
    private val touchSlop = ViewConfiguration.get(context).scaledTouchSlop
    // An element's action can move the focus to another field, as a tap
    // does: the composition is committed in its own field first.
    private val accessibility =
        CanvasAccessibility(this) {
            if (it is Input.Activate) commitComposition()
            session.send(it)
        }
    private val keyboard = context.getSystemService(InputMethodManager::class.java)

    /** Find mode: Return performs the find (the keyboard shows Search). */
    var finding = false
        set(value) {
            if (field != value) {
                field = value
                keyboard.restartInput(this)
            }
        }

    private var caret: Caret? = null
    private var reportCaret = false
    /** The field takes a password: secure entry, nothing learned or suggested. */
    private var password = false
    private var input: TextInput? = null

    init {
        setEGLContextClientVersion(3)
        setPreserveEGLContextOnPause(true)
        val debuggable = (context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0
        setRenderer(FramePainter(session, debuggable, ::surfaceReady))
        renderMode = RENDERMODE_WHEN_DIRTY
        isFocusable = true
        isFocusableInTouchMode = true
        contentDescription = "FileMaker layout"
    }

    private val render: () -> Unit = ::requestRender

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        session.signal.listener = render
        // A frame that came while no canvas showed the document waits.
        requestRender()
    }

    override fun onDetachedFromWindow() {
        // A canvas that replaced this one may already listen.
        if (session.signal.listener === render) session.signal.listener = null
        // The keyboard typed into this canvas: it goes with it (a closed
        // file leaves the Launch Center, where nothing takes its text).
        if (keyboard.isActive(this)) keyboard.hideSoftInputFromWindow(windowToken, 0)
        super.onDetachedFromWindow()
    }

    /** The surface's size in pixels, on the rendering thread. */
    private fun surfaceReady(width: Int, height: Int, maxTextureSide: Int) {
        val viewport = Viewport(width / density, height / density, density, maxTextureSide.toUInt())
        session.send(Input.Resize(viewport))
    }

    /** A field takes typing, or none does: the keyboard follows. */
    fun showKeyboard(show: Boolean) {
        if (show) {
            requestFocus()
            keyboard.showSoftInput(this, 0)
        } else {
            keyboard.hideSoftInputFromWindow(windowToken, 0)
        }
    }

    /** The field's insertion point moved, perhaps into another field. */
    fun moveCaret(caret: Caret?) {
        this.caret = caret
        val secure = caret?.password == true
        if (secure != password) {
            password = secure
            keyboard.restartInput(this)
        }
        reportCaret()
    }

    /**
     * Commit what the input method is composing before a command of the
     * chrome, so that a save never takes a provisional candidate, and tell
     * the input method its composition ended.
     */
    fun commitComposition() {
        if (input?.commitComposition() == true) keyboard.restartInput(this)
    }

    fun present(elements: List<Element>) = accessibility.present(elements)

    // Touch ------------------------------------------------------------------

    private var velocity: VelocityTracker? = null
    private var downX = 0f
    private var downY = 0f
    private var lastX = 0f
    private var lastY = 0f
    private var panning = false
    // Two fingers: FileMaker Go's swipe to the previous or next record.
    private var swipeStartX = 0f
    private var swipeStartY = 0f
    private var swiping = false

    @SuppressLint("ClickableViewAccessibility") // performClick runs on a tap.
    override fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                velocity = VelocityTracker.obtain().apply { addMovement(event) }
                downX = event.x
                downY = event.y
                lastX = event.x
                lastY = event.y
                panning = false
                swiping = false
            }
            MotionEvent.ACTION_POINTER_DOWN ->
                if (event.pointerCount == 2) {
                    endPan(0f, 0f)
                    swiping = true
                    swipeStartX = (event.getX(0) + event.getX(1)) / 2
                    swipeStartY = (event.getY(0) + event.getY(1)) / 2
                }
            MotionEvent.ACTION_MOVE -> {
                velocity?.addMovement(event)
                if (swiping) return true
                if (!panning && abs(event.x - downX) + abs(event.y - downY) > touchSlop) {
                    // What is under the finger where it went down scrolls,
                    // from where the pan was recognized: the touch slop is
                    // not a jump of the content, as in Android's own lists.
                    panning = true
                    session.send(Input.PanBegan(downX / density, downY / density))
                    lastX = event.x
                    lastY = event.y
                }
                if (panning) {
                    // The content follows the finger.
                    session.send(Input.PanMoved((event.x - lastX) / density, (event.y - lastY) / density))
                    lastX = event.x
                    lastY = event.y
                }
            }
            MotionEvent.ACTION_POINTER_UP ->
                if (swiping && event.pointerCount == 2) {
                    val dx = (event.getX(0) + event.getX(1)) / 2 - swipeStartX
                    val dy = (event.getY(0) + event.getY(1)) / 2 - swipeStartY
                    if (abs(dx) > SWIPE_DP * density && abs(dx) > 2 * abs(dy)) {
                        commitComposition()
                        session.send(Input.Run(if (dx < 0) Command.Next else Command.Previous))
                    }
                }
            MotionEvent.ACTION_UP -> {
                velocity?.addMovement(event)
                when {
                    swiping -> Unit
                    panning -> {
                        val tracker = velocity
                        tracker?.computeCurrentVelocity(1000)
                        endPan((tracker?.xVelocity ?: 0f) / density, (tracker?.yVelocity ?: 0f) / density)
                    }
                    else -> {
                        performClick()
                        // A tap can move the focus to another field, which
                        // the same input connection then serves: what the
                        // input method composes is committed in its own
                        // field first, and the input method told it ended.
                        commitComposition()
                        session.send(Input.Tap(event.x / density, event.y / density))
                    }
                }
                recycle()
            }
            MotionEvent.ACTION_CANCEL -> {
                endPan(0f, 0f)
                recycle()
            }
        }
        return true
    }

    override fun performClick(): Boolean {
        super.performClick()
        requestFocus()
        return true
    }

    private fun endPan(vx: Float, vy: Float) {
        if (panning) session.send(Input.PanEnded(vx, vy))
        panning = false
    }

    private fun recycle() {
        velocity?.recycle()
        velocity = null
    }

    override fun dispatchHoverEvent(event: MotionEvent) =
        accessibility.dispatchHoverEvent(event) || super.dispatchHoverEvent(event)

    // Keys -------------------------------------------------------------------

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean = key(event) || super.onKeyDown(keyCode, event)

    /** A key from a hardware keyboard or the input method; false if the engine takes none. */
    internal fun key(event: KeyEvent): Boolean {
        val command = event.isCtrlPressed || event.isMetaPressed
        if (command && event.keyCode == KeyEvent.KEYCODE_V) {
            val clipboard = context.getSystemService(ClipboardManager::class.java)
            val text = clipboard.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(context)
            if (!text.isNullOrEmpty()) session.send(Input.Paste(text.toString()))
            return true
        }
        val key =
            when (event.keyCode) {
                KeyEvent.KEYCODE_TAB -> Key.TAB
                KeyEvent.KEYCODE_DPAD_LEFT -> Key.ARROW_LEFT
                KeyEvent.KEYCODE_DPAD_RIGHT -> Key.ARROW_RIGHT
                KeyEvent.KEYCODE_DPAD_UP -> Key.ARROW_UP
                KeyEvent.KEYCODE_DPAD_DOWN -> Key.ARROW_DOWN
                KeyEvent.KEYCODE_DEL -> Key.BACKSPACE
                KeyEvent.KEYCODE_FORWARD_DEL -> Key.DELETE
                KeyEvent.KEYCODE_ESCAPE -> Key.ESCAPE
                KeyEvent.KEYCODE_MOVE_HOME -> Key.HOME
                KeyEvent.KEYCODE_MOVE_END -> Key.END
                KeyEvent.KEYCODE_PAGE_UP -> Key.PAGE_UP
                KeyEvent.KEYCODE_PAGE_DOWN -> Key.PAGE_DOWN
                KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> {
                    returnKey()
                    return true
                }
                else -> null
            }
        if (key != null) {
            session.send(Input.Press(key, event.isShiftPressed, command))
            return true
        }
        val char = event.unicodeChar
        if (char == 0 || command) return false
        if (char and KeyCharacterMap.COMBINING_ACCENT != 0) {
            // A dead key (an accent on an international keyboard): it marks
            // the next key's letter.
            accent = char and KeyCharacterMap.COMBINING_ACCENT_MASK
            return true
        }
        val accented = accent.takeIf { it != 0 }?.let { KeyCharacterMap.getDeadChar(it, char) }
        accent = 0
        val typed = accented?.takeIf { it != 0 } ?: char
        if (!Character.isValidCodePoint(typed)) return false
        session.send(Input.Text(String(Character.toChars(typed))))
        return true
    }

    // The accent of a dead key pressed before the next key.
    private var accent = 0

    /** Return: performs the find in Find mode; a field's own Return elsewhere. */
    internal fun returnKey() {
        if (finding) {
            commitComposition()
            session.send(Input.Run(Command.PerformFind))
        } else {
            session.send(Input.Press(Key.ENTER, false, false))
        }
    }

    // Input method -----------------------------------------------------------

    override fun onCheckIsTextEditor() = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        outAttrs.inputType =
            EditorInfo.TYPE_CLASS_TEXT or
                when {
                    password -> EditorInfo.TYPE_TEXT_VARIATION_PASSWORD
                    finding -> 0
                    else -> EditorInfo.TYPE_TEXT_FLAG_MULTI_LINE
                }
        outAttrs.imeOptions =
            EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_FLAG_NO_FULLSCREEN or
                (if (password) EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING else 0) or
                if (finding) EditorInfo.IME_ACTION_SEARCH else EditorInfo.IME_ACTION_NONE
        outAttrs.initialSelStart = 0
        outAttrs.initialSelEnd = 0
        return TextInput(this, session).also { input = it }
    }

    /** The input method asks where the insertion point is: now, and as it moves. */
    internal fun requestCaretReports(monitor: Boolean, now: Boolean) {
        reportCaret = monitor
        if (now) reportCaret(force = true)
    }

    /** Where the field's insertion point is, for the input method's candidates. */
    private fun reportCaret(force: Boolean = false) {
        val caret = caret ?: return
        if (!reportCaret && !force) return
        val location = IntArray(2)
        getLocationOnScreen(location)
        val matrix = Matrix().apply { setTranslate(location[0].toFloat(), location[1].toFloat()) }
        val left = caret.x * density
        val top = caret.y * density
        val bottom = (caret.y + caret.height) * density
        val info =
            CursorAnchorInfo.Builder()
                .setMatrix(matrix)
                .setInsertionMarkerLocation(left, top, bottom, bottom, CursorAnchorInfo.FLAG_HAS_VISIBLE_REGION)
                .build()
        keyboard.updateCursorAnchorInfo(this, info)
    }

    private companion object {
        // How far two fingers travel sideways to turn to another record.
        const val SWIPE_DP = 80f
    }
}
