package dev.openmaker.go.canvas

import android.graphics.Rect
import android.os.Bundle
import android.view.View
import androidx.core.view.ViewCompat
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat
import androidx.customview.widget.ExploreByTouchHelper
import dev.openmaker.go.engine.Element
import dev.openmaker.go.engine.ElementKind
import dev.openmaker.go.engine.Input
import kotlin.math.roundToInt

/**
 * TalkBack's view of the layout: the visible fields, labels and buttons the
 * engine reports while a screen reader runs, each activated through the
 * engine as a tap on it would be.
 */
internal class CanvasAccessibility(private val view: View, private val send: (Input) -> Unit) :
    ExploreByTouchHelper(view) {
    private var shown = emptyList<Element>()
    // ExploreByTouchHelper's ids are Ints; the engine's are 64-bit.
    private var nodes = emptyMap<Int, Element>()
    private var ids = emptyMap<ULong, Int>()
    private var nextId = 1

    init {
        ViewCompat.setAccessibilityDelegate(view, this)
    }

    fun present(elements: List<Element>) {
        if (shown == elements) return
        val mapped = elements.associate { it.id to (ids[it.id] ?: nextId++) }
        ids = mapped
        nodes = elements.associateBy { mapped.getValue(it.id) }
        shown = elements
        invalidateRoot()
    }

    // The layout scrolls under TalkBack's scroll gestures, a page at a time,
    // as a pan of the canvas does.
    override fun onPopulateNodeForHost(node: AccessibilityNodeInfoCompat) {
        node.isScrollable = true
        node.addAction(AccessibilityNodeInfoCompat.AccessibilityActionCompat.ACTION_SCROLL_FORWARD)
        node.addAction(AccessibilityNodeInfoCompat.AccessibilityActionCompat.ACTION_SCROLL_BACKWARD)
    }

    override fun performAccessibilityAction(host: View, action: Int, args: Bundle?): Boolean {
        val forward = action == AccessibilityNodeInfoCompat.ACTION_SCROLL_FORWARD
        if (!forward && action != AccessibilityNodeInfoCompat.ACTION_SCROLL_BACKWARD) {
            return super.performAccessibilityAction(host, action, args)
        }
        val density = view.resources.displayMetrics.density
        val (width, height) = view.width / density to view.height / density
        // The content follows the pan: forward brings what is below up.
        val page = height * PAGE * if (forward) -1 else 1
        send(Input.PanBegan(width / 2, height / 2))
        send(Input.PanMoved(0f, page))
        send(Input.PanEnded(0f, 0f))
        return true
    }

    override fun getVirtualViewAt(x: Float, y: Float): Int {
        val density = view.resources.displayMetrics.density
        val (px, py) = x / density to y / density
        val item =
            shown.lastOrNull { px >= it.left && px < it.left + it.width && py >= it.top && py < it.top + it.height }
                ?: return INVALID_ID
        return ids.getValue(item.id)
    }

    override fun getVisibleVirtualViews(out: MutableList<Int>) {
        out.addAll(nodes.keys)
    }

    override fun onPopulateNodeForVirtualView(id: Int, info: AccessibilityNodeInfoCompat) {
        val item = nodes[id]
        if (item == null) {
            // Gone since TalkBack asked: an empty node, with the bounds the
            // helper requires.
            info.contentDescription = ""
            @Suppress("DEPRECATION") info.setBoundsInParent(Rect())
            return
        }
        info.contentDescription = listOf(item.label, item.value).filter(String::isNotEmpty).joinToString(", ")
        info.className =
            when (item.kind) {
                ElementKind.BUTTON -> "android.widget.Button"
                ElementKind.FIELD -> "android.widget.EditText"
                ElementKind.LABEL -> "android.widget.TextView"
            }
        info.isEnabled = item.enabled || item.kind == ElementKind.LABEL
        info.isClickable = item.enabled
        if (item.enabled) info.addAction(AccessibilityNodeInfoCompat.ACTION_CLICK)
        val density = view.resources.displayMetrics.density
        val bounds =
            Rect(
                (item.left * density).roundToInt(),
                (item.top * density).roundToInt(),
                ((item.left + item.width) * density).roundToInt(),
                ((item.top + item.height) * density).roundToInt(),
            )
        // In the canvas: ExploreByTouchHelper requires these, and works out
        // the screen's from them.
        @Suppress("DEPRECATION") info.setBoundsInParent(bounds)
    }

    override fun onPerformActionForVirtualView(id: Int, action: Int, arguments: Bundle?): Boolean {
        val item = nodes[id] ?: return false
        if (action != AccessibilityNodeInfoCompat.ACTION_CLICK || !item.enabled) return false
        send(Input.Activate(item.id))
        return true
    }

    private companion object {
        /** How much of the canvas's height a screen reader's scroll moves. */
        const val PAGE = 0.8f
    }
}
