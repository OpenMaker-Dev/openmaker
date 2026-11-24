package dev.openmaker.go

import android.content.Context
import android.net.Uri
import android.os.StatFs
import android.provider.OpenableColumns
import java.io.File
import org.json.JSONArray

/** A file the Launch Center lists: on this device when [host] is empty. */
data class Entry(val name: String, val host: String = "", val port: Int = DEFAULT_PORT) {
    val local
        get() = host.isEmpty()

    val subtitle
        get() = if (local) "On this device" else "$host:$port"

    internal fun json(): JSONArray = JSONArray().put(name).put(host).put(port)

    companion object {
        const val DEFAULT_PORT = 5003
    }
}

/**
 * The apps on this device, Favorites, Recent and the hosts' trusted
 * certificate authorities. File references are kept by name: the documents
 * folder's absolute path can change with an update.
 */
class Library(context: Context) {
    private val prefs = context.getSharedPreferences("library", Context.MODE_PRIVATE)
    private val resolver = context.contentResolver
    val directory = File(context.filesDir, "Documents").apply { mkdirs() }

    fun localFiles(): List<Entry> =
        directory
            .listFiles()
            .orEmpty()
            .filter { it.isFile && it.extension.equals("fmp12", ignoreCase = true) }
            .sortedBy { it.name.lowercase() }
            .map { Entry(it.name) }

    fun path(entry: Entry): String = File(directory, entry.name).path

    var favorites: List<Entry>
        get() = entries(FAVORITES)
        set(value) = store(FAVORITES, value)

    var recent: List<Entry>
        get() = entries(RECENT)
        set(value) = store(RECENT, value.distinct().take(MAX_RECENT))

    var lastHost: String
        get() = prefs.getString("lastHost", "").orEmpty()
        set(value) = prefs.edit().putString("lastHost", value).apply()

    var lastPort: Int
        get() = prefs.getInt("lastPort", Entry.DEFAULT_PORT)
        set(value) = prefs.edit().putInt("lastPort", value).apply()

    /** The PEM certificate authority trusted for one host and port only. */
    fun certificate(host: String, port: Int): String = prefs.getString("ca:$host:$port", "").orEmpty()

    fun trust(host: String, port: Int, pem: String) {
        prefs.edit().putString("ca:$host:$port", pem).apply()
    }

    /**
     * Copy a picked file into the documents folder under its own name. The
     * copy is written aside and then linked in, so a partly written file is
     * never listed, and linking refuses a name already there.
     */
    fun import(uri: Uri): Entry {
        val name =
            resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
                if (it.moveToFirst()) it.getString(0) else null
            } ?: error("The document has no name.")
        require(name == File(name).name && name.endsWith(".fmp12", ignoreCase = true)) {
            "Choose an .fmp12 file."
        }
        val destination = File(directory, name)
        require(!destination.exists()) { "A file named $name is already in My Apps." }
        val temporary = File.createTempFile(".import-", null, directory)
        try {
            val input = resolver.openInputStream(uri) ?: error("Cannot read this document.")
            // A provider can stream without end: the copy stops short of
            // filling the device.
            val room = StatFs(directory.path).availableBytes - SPARE_BYTES
            input.use { source ->
                temporary.outputStream().use { target ->
                    val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
                    var copied = 0L
                    while (true) {
                        val read = source.read(buffer)
                        if (read < 0) break
                        copied += read
                        check(copied <= room) { "There is not enough space on this device for $name." }
                        target.write(buffer, 0, read)
                    }
                }
            }
            android.system.Os.link(temporary.path, destination.path)
        } finally {
            temporary.delete()
        }
        return Entry(name)
    }

    /** Delete a file on this device and every reference to it. */
    fun delete(entry: Entry) {
        require(entry.local)
        File(directory, entry.name).delete()
        favorites = favorites - entry
        recent = recent - entry
    }

    private fun entries(key: String): List<Entry> {
        val list = JSONArray(prefs.getString(key, "[]"))
        return (0 until list.length()).map {
            val item = list.getJSONArray(it)
            Entry(item.getString(0), item.getString(1), item.getInt(2))
        }
    }

    private fun store(key: String, values: List<Entry>) {
        prefs.edit().putString(key, JSONArray(values.map { it.json() }).toString()).apply()
    }

    private companion object {
        const val FAVORITES = "favorites"
        const val RECENT = "recent"
        const val MAX_RECENT = 30
        // Room an import leaves on the device for the files it opens.
        const val SPARE_BYTES = 256L shl 20
    }
}
