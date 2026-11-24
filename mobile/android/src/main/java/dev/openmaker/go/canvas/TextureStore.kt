package dev.openmaker.go.canvas

import android.util.Log
import dev.openmaker.go.engine.TextureUpdate
import dev.openmaker.go.engine.TextureWrap

/**
 * A copy of every texture the engine sent. The engine sends a texture once
 * and then only its changes, while Android can take a canvas's OpenGL
 * context away (a recreated activity, a lost surface): a new context
 * uploads these.
 */
class TextureStore {
    class Texture(
        val width: Int,
        val height: Int,
        val pixels: ByteArray,
        val magnifyLinear: Boolean,
        val minifyLinear: Boolean,
        val wrap: TextureWrap,
    )

    private val textures = HashMap<ULong, Texture>()

    @Synchronized
    fun apply(updates: List<TextureUpdate>) {
        for (update in updates) {
            if (!update.partial) {
                textures[update.id] =
                    Texture(
                        update.width.toInt(),
                        update.height.toInt(),
                        update.pixels.copyOf(),
                        update.magnifyLinear,
                        update.minifyLinear,
                        update.wrap,
                    )
                continue
            }
            val texture = textures[update.id]
            val (x, y, w, h) = listOf(update.x, update.y, update.width, update.height).map { it.toInt() }
            if (texture == null || x + w > texture.width || y + h > texture.height || update.pixels.size < w * h * 4) {
                Log.w(TAG, "ignored a texture change outside texture ${update.id}")
                continue
            }
            for (row in 0 until h) {
                System.arraycopy(update.pixels, row * w * 4, texture.pixels, ((y + row) * texture.width + x) * 4, w * 4)
            }
        }
    }

    @Synchronized
    fun texture(id: ULong): Texture? = textures[id]

    @Synchronized
    operator fun contains(id: ULong): Boolean = id in textures

    @Synchronized
    fun free(ids: List<ULong>) {
        ids.forEach(textures::remove)
    }

    @Synchronized
    fun forEach(action: (ULong, Texture) -> Unit) {
        textures.forEach(action)
    }

    private companion object {
        const val TAG = "OpenMakerGo"
    }
}
