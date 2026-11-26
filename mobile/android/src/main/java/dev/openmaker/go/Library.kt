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
 * A host of the Hosts list: its address and port, and the name it is
 * listed by, empty for its location. The same address in another case is
 * the same host.
 */
data class SavedHost(val address: String, val port: Int = Entry.DEFAULT_PORT, val name: String = "") {
    /** The address, with the port when it is not FileMaker's (an IPv6 address then in brackets). */
    val location
        get() =
            when {
                port == Entry.DEFAULT_PORT -> address
                ':' in address -> "[$address]:$port"
                else -> "$address:$port"
            }

    val label
        get() = name.ifEmpty { location }

    fun isHost(address: String, port: Int) = this.address.equals(address, ignoreCase = true) && this.port == port

    fun isHost(other: SavedHost) = isHost(other.address, other.port)
}

/**
 * The apps on this device, Favorites, Recent, the Hosts list and the hosts'
 * trusted certificate authorities. File references are kept by name: the
 * documents folder's absolute path can change with an update.
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

    /**
     * The Hosts list. Before it was kept, the Hosts tab remembered the one
     * host it last browsed: that host is the list's first.
     */
    var hosts: List<SavedHost>
        get() {
            val stored = prefs.getString(HOSTS, null)
            if (stored == null) {
                val last = prefs.getString(LAST_HOST, "").orEmpty().trim()
                return if (last.isEmpty()) emptyList()
                else listOf(SavedHost(last, prefs.getInt(LAST_PORT, Entry.DEFAULT_PORT)))
            }
            val list = JSONArray(stored)
            return (0 until list.length()).map {
                val item = list.getJSONArray(it)
                SavedHost(item.getString(0), item.getInt(1), item.getString(2))
            }
        }
        private set(value) {
            val list = JSONArray(value.map { JSONArray().put(it.address).put(it.port).put(it.name) })
            prefs.edit().putString(HOSTS, list.toString()).remove(LAST_HOST).remove(LAST_PORT).apply()
        }

    /**
     * Add [host], or with [replacing] put it in that host's place, with
     * [certificate] as the certificate authority trusted for it. A host
     * already in the list at the same address and port is that host: it is
     * not listed twice. A host moved to another address or port takes its
     * certificate authority along.
     */
    fun saveHost(replacing: SavedHost?, host: SavedHost, certificate: String) {
        val list = hosts.toMutableList()
        // Where the host goes: the replaced host's place, else its own.
        val at =
            replacing?.let { old -> list.indexOfFirst { it.isHost(old) } }?.takeIf { it >= 0 }
                ?: list.indexOfFirst { it.isHost(host) }.takeIf { it >= 0 }
                ?: list.size.also { list += host }
        list[at] = host
        hosts = list.filterIndexed { i, other -> i == at || !other.isHost(host) }
        // Certificate authorities are kept by the address as typed.
        if (replacing != null && (replacing.address != host.address || replacing.port != host.port)) {
            trust(replacing.address, replacing.port, "")
        }
        trust(host.address, host.port, certificate)
    }

    /** Remove [host] from the list, with the certificate authority trusted for it. */
    fun removeHost(host: SavedHost) {
        hosts = hosts.filterNot { it.isHost(host) }
        trust(host.address, host.port, "")
    }

    /** The PEM certificate authority trusted for one host and port only. */
    fun certificate(host: String, port: Int): String = prefs.getString("ca:$host:$port", "").orEmpty()

    /** Save [pem] as the host's certificate authority; an empty one removes it. */
    private fun trust(host: String, port: Int, pem: String) {
        val edit = prefs.edit()
        if (pem.isBlank()) edit.remove("ca:$host:$port") else edit.putString("ca:$host:$port", pem)
        edit.apply()
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
        const val HOSTS = "hosts"
        const val LAST_HOST = "lastHost"
        const val LAST_PORT = "lastPort"
        const val MAX_RECENT = 30
        // Room an import leaves on the device for the files it opens.
        const val SPARE_BYTES = 256L shl 20
    }
}
