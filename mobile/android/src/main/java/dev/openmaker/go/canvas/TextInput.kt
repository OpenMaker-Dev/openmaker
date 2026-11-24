package dev.openmaker.go.canvas

import android.view.KeyEvent
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.InputConnection
import dev.openmaker.go.DocumentSession
import dev.openmaker.go.engine.Input
import dev.openmaker.go.engine.Key

/**
 * The keyboard's connection to the field being edited. The field's text is
 * the engine's; this editor holds only what the input method is composing
 * (marked text), which shows in the field as egui's preedit, and is empty
 * again once text is committed.
 */
internal class TextInput(private val canvas: CanvasView, private val session: DocumentSession) :
    BaseInputConnection(canvas, true) {

    override fun setComposingText(text: CharSequence?, newCursorPosition: Int): Boolean {
        super.setComposingText(text, newCursorPosition)
        session.send(Input.Compose(text?.toString().orEmpty()))
        return true
    }

    override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
        val committed = text?.toString().orEmpty()
        val composing = composing() != null
        editable?.clear()
        when {
            committed == "\n" -> canvas.returnKey()
            committed.isNotEmpty() -> session.send(Input.Text(committed))
            // Nothing committed ends the composition.
            composing -> session.send(Input.Compose(""))
        }
        return true
    }

    override fun finishComposingText(): Boolean {
        commitComposition()
        return true
    }

    /** Send what is composed as committed text; false when nothing was. */
    fun commitComposition(): Boolean {
        val composed = composing()
        composed?.let { session.send(Input.Text(it)) }
        editable?.clear()
        return composed != null
    }

    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        // The editor is empty: what the keyboard deletes is the field's.
        repeat(beforeLength.coerceIn(0, MAX_DELETE)) { session.send(Input.Press(Key.BACKSPACE, false, false)) }
        repeat(afterLength.coerceIn(0, MAX_DELETE)) { session.send(Input.Press(Key.DELETE, false, false)) }
        return true
    }

    // Keyboards that count in code points delete through this one; the
    // inherited version would edit only this connection's own editor.
    // The engine's Backspace and Delete take a character each.
    override fun deleteSurroundingTextInCodePoints(beforeLength: Int, afterLength: Int): Boolean =
        deleteSurroundingText(beforeLength, afterLength)

    override fun sendKeyEvent(event: KeyEvent): Boolean {
        if (event.action == KeyEvent.ACTION_DOWN) canvas.key(event)
        return true
    }

    override fun performEditorAction(actionCode: Int): Boolean {
        canvas.returnKey()
        return true
    }

    override fun requestCursorUpdates(cursorUpdateMode: Int): Boolean {
        canvas.requestCaretReports(
            monitor = (cursorUpdateMode and InputConnection.CURSOR_UPDATE_MONITOR) != 0,
            now = (cursorUpdateMode and InputConnection.CURSOR_UPDATE_IMMEDIATE) != 0,
        )
        return true
    }

    private fun composing(): String? {
        val text = editable ?: return null
        val start = BaseInputConnection.getComposingSpanStart(text)
        val end = BaseInputConnection.getComposingSpanEnd(text)
        return if (start in 0 until end) text.substring(start, end) else null
    }

    private companion object {
        // A runaway delete request from a keyboard is not a user's.
        const val MAX_DELETE = 64
    }
}
