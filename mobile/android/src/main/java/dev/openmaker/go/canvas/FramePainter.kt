package dev.openmaker.go.canvas

import android.opengl.GLES30.*
import android.opengl.GLSurfaceView
import android.os.SystemClock
import android.util.Log
import dev.openmaker.go.DocumentSession
import dev.openmaker.go.Geometry
import dev.openmaker.go.engine.Frame
import dev.openmaker.go.engine.TextureUpdate
import dev.openmaker.go.engine.TextureWrap
import java.nio.ByteBuffer
import java.nio.ByteOrder
import javax.microedition.khronos.egl.EGLConfig
import javax.microedition.khronos.opengles.GL10

/**
 * Draws the engine's frames as egui's own painters do (egui-wgpu's shader):
 * positions in points, scaled to the canvas; vertex colors and textures as
 * premultiplied sRGB, multiplied and blended without converting to linear
 * light; one draw call per batch, clipped to its rectangle.
 */
class FramePainter(
    private val session: DocumentSession,
    /** Log frames that miss a display refresh (debug builds). */
    private val logTimings: Boolean,
    private val surfaceReady: (width: Int, height: Int, maxTextureSide: Int) -> Unit,
) : GLSurfaceView.Renderer {
    private var program = 0
    private var screen = 0
    private val buffers = IntArray(2)
    private var vertexBuffer: ByteBuffer = ByteBuffer.allocateDirect(0)
    private var indexBuffer: ByteBuffer = ByteBuffer.allocateDirect(0)
    private val glTextures = HashMap<ULong, Int>()
    private var maxTextureSide = 2048
    private var surfaceHeight = 0

    override fun onSurfaceCreated(unused: GL10?, config: EGLConfig?) {
        program = link(VERTEX_SHADER, FRAGMENT_SHADER)
        screen = glGetUniformLocation(program, "uScreen")
        glGenBuffers(2, buffers, 0)
        val max = IntArray(1)
        glGetIntegerv(GL_MAX_TEXTURE_SIZE, max, 0)
        maxTextureSide = max[0]
        // A new context: what the old one held is gone.
        glTextures.clear()
        session.textures.forEach { id, texture -> upload(id, texture) }
    }

    override fun onSurfaceChanged(unused: GL10?, width: Int, height: Int) {
        glViewport(0, 0, width, height)
        surfaceHeight = height
        surfaceReady(width, height, maxTextureSide)
    }

    override fun onDrawFrame(unused: GL10?) {
        val started = SystemClock.elapsedRealtimeNanos()
        val frame = session.takeFrame()
        if (frame != null) {
            // OpenGL keeps what the session keeps: it lets go of a texture
            // once the geometry that may draw it is replaced.
            glTextures.keys.filter { it !in session.textures }.forEach { id ->
                glTextures.remove(id)?.let { glDeleteTextures(1, intArrayOf(it), 0) }
            }
            frame.textures.forEach(::apply)
        }
        glClearColor(1f, 1f, 1f, 1f)
        glClear(GL_COLOR_BUFFER_BIT)
        session.geometry?.let(::draw)
        frame?.let { log(it, SystemClock.elapsedRealtimeNanos() - started) }
    }

    private fun draw(geometry: Geometry) {
        val viewport = geometry.viewport
        vertexBuffer = fill(vertexBuffer, geometry.vertices)
        indexBuffer = fill(indexBuffer, geometry.indices)
        glUseProgram(program)
        glUniform2f(screen, viewport.width, viewport.height)
        glBindBuffer(GL_ARRAY_BUFFER, buffers[0])
        glBufferData(GL_ARRAY_BUFFER, geometry.vertices.size, vertexBuffer, GL_STREAM_DRAW)
        glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, buffers[1])
        glBufferData(GL_ELEMENT_ARRAY_BUFFER, geometry.indices.size, indexBuffer, GL_STREAM_DRAW)
        // x, y, u, v as floats, then the color as four bytes: 20 bytes.
        glEnableVertexAttribArray(0)
        glVertexAttribPointer(0, 2, GL_FLOAT, false, VERTEX_BYTES, 0)
        glEnableVertexAttribArray(1)
        glVertexAttribPointer(1, 2, GL_FLOAT, false, VERTEX_BYTES, 8)
        glEnableVertexAttribArray(2)
        glVertexAttribPointer(2, 4, GL_UNSIGNED_BYTE, true, VERTEX_BYTES, 16)
        glEnable(GL_BLEND)
        glBlendEquation(GL_FUNC_ADD)
        glBlendFuncSeparate(GL_ONE, GL_ONE_MINUS_SRC_ALPHA, GL_ONE_MINUS_DST_ALPHA, GL_ONE)
        glEnable(GL_SCISSOR_TEST)
        glActiveTexture(GL_TEXTURE0)
        for (batch in geometry.batches) {
            val texture = glTextures[batch.texture] ?: continue
            glBindTexture(GL_TEXTURE_2D, texture)
            // Clip rectangles count from the top; OpenGL's from the bottom.
            val height = batch.clipHeight.toInt()
            glScissor(batch.clipX.toInt(), surfaceHeight - batch.clipY.toInt() - height, batch.clipWidth.toInt(), height)
            glDrawElements(GL_TRIANGLES, batch.indexCount.toInt(), GL_UNSIGNED_INT, batch.firstIndex.toInt() * 4)
        }
        glDisable(GL_SCISSOR_TEST)
    }

    private fun apply(update: TextureUpdate) {
        if (!update.partial) {
            glTextures.remove(update.id)?.let { glDeleteTextures(1, intArrayOf(it), 0) }
            val texture = session.textures.texture(update.id) ?: return
            upload(update.id, texture)
            return
        }
        val name = glTextures[update.id] ?: return
        glBindTexture(GL_TEXTURE_2D, name)
        glPixelStorei(GL_UNPACK_ALIGNMENT, 1)
        glTexSubImage2D(
            GL_TEXTURE_2D, 0, update.x.toInt(), update.y.toInt(), update.width.toInt(), update.height.toInt(),
            GL_RGBA, GL_UNSIGNED_BYTE, ByteBuffer.wrap(update.pixels),
        )
    }

    private fun upload(id: ULong, texture: TextureStore.Texture) {
        val name = IntArray(1)
        glGenTextures(1, name, 0)
        glBindTexture(GL_TEXTURE_2D, name[0])
        glPixelStorei(GL_UNPACK_ALIGNMENT, 1)
        glTexImage2D(
            GL_TEXTURE_2D, 0, GL_RGBA8, texture.width, texture.height, 0,
            GL_RGBA, GL_UNSIGNED_BYTE, ByteBuffer.wrap(texture.pixels),
        )
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, if (texture.magnifyLinear) GL_LINEAR else GL_NEAREST)
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, if (texture.minifyLinear) GL_LINEAR else GL_NEAREST)
        val wrap =
            when (texture.wrap) {
                TextureWrap.CLAMP -> GL_CLAMP_TO_EDGE
                TextureWrap.REPEAT -> GL_REPEAT
                TextureWrap.MIRROR -> GL_MIRRORED_REPEAT
            }
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, wrap)
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, wrap)
        glTextures[id] = name[0]
    }

    private fun log(frame: Frame, nanos: Long) {
        val timings = frame.timings
        val engine = (timings.pass + timings.tessellate + timings.pack).toLong()
        val paint = nanos / 1000
        val slow = logTimings && engine + paint > SLOW_FRAME_MICROS
        // Every frame with `adb shell setprop log.tag.OpenMakerGo DEBUG`.
        if (!slow && !Log.isLoggable(TAG, Log.DEBUG)) return
        val line =
            "frame: pass ${timings.pass} us, tessellate ${timings.tessellate} us, pack ${timings.pack} us, " +
                "paint $paint us, ${frame.vertices.size / VERTEX_BYTES} vertices, ${frame.batches.size} batches"
        if (slow) Log.w(TAG, "slow $line") else Log.d(TAG, line)
    }

    private fun fill(buffer: ByteBuffer, bytes: ByteArray): ByteBuffer {
        val target =
            if (buffer.capacity() >= bytes.size) buffer
            else ByteBuffer.allocateDirect(bytes.size * 3 / 2).order(ByteOrder.nativeOrder())
        target.clear()
        target.put(bytes)
        target.flip()
        return target
    }

    private fun link(vertex: String, fragment: String): Int {
        fun compile(type: Int, source: String): Int {
            val shader = glCreateShader(type)
            glShaderSource(shader, source)
            glCompileShader(shader)
            val ok = IntArray(1)
            glGetShaderiv(shader, GL_COMPILE_STATUS, ok, 0)
            check(ok[0] != 0) { glGetShaderInfoLog(shader) }
            return shader
        }
        val program = glCreateProgram()
        glAttachShader(program, compile(GL_VERTEX_SHADER, vertex))
        glAttachShader(program, compile(GL_FRAGMENT_SHADER, fragment))
        glBindAttribLocation(program, 0, "aPos")
        glBindAttribLocation(program, 1, "aUv")
        glBindAttribLocation(program, 2, "aColor")
        glLinkProgram(program)
        val ok = IntArray(1)
        glGetProgramiv(program, GL_LINK_STATUS, ok, 0)
        check(ok[0] != 0) { glGetProgramInfoLog(program) }
        glUseProgram(program)
        glUniform1i(glGetUniformLocation(program, "uTexture"), 0)
        return program
    }

    private companion object {
        const val TAG = "OpenMakerGo"
        const val VERTEX_BYTES = 20
        // A frame that misses a 60 Hz display's refresh.
        const val SLOW_FRAME_MICROS = 16_000L

        const val VERTEX_SHADER =
            """#version 300 es
in vec2 aPos;
in vec2 aUv;
in vec4 aColor;
uniform vec2 uScreen;
out vec2 vUv;
out vec4 vColor;
void main() {
    gl_Position = vec4(2.0 * aPos.x / uScreen.x - 1.0, 1.0 - 2.0 * aPos.y / uScreen.y, 0.0, 1.0);
    vUv = aUv;
    vColor = aColor;
}
"""

        // High precision: a texture coordinate in a large atlas needs it.
        const val FRAGMENT_SHADER =
            """#version 300 es
precision highp float;
uniform sampler2D uTexture;
in vec2 vUv;
in vec4 vColor;
out vec4 outColor;
void main() {
    outColor = vColor * texture(uTexture, vUv);
}
"""
    }
}
