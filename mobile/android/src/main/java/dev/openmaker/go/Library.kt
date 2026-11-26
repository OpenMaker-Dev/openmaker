package dev.openmaker.go

import android.content.Context
import android.content.SharedPreferences
import android.net.Uri
import android.os.StatFs
import android.provider.OpenableColumns
import java.io.File
import dev.openmaker.go.engine.Endpoint
import dev.openmaker.go.engine.forgetTrustedCertificates
import dev.openmaker.go.engine.hostKey
import dev.openmaker.go.engine.saveTrustedCertificates
import dev.openmaker.go.engine.trustedCertificate
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
 * listed by, empty for its location. Two hosts are the same host when the
 * engine says so ([hostKey]: ASCII case and an international name's
 * punycode form, as DNS compares names).
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

    val key
        get() = hostKey(address, port.toUShort())

    val endpoint
        get() = Endpoint(address, port.toUShort())

    fun isHost(address: String, port: Int) = key == hostKey(address, port.toUShort())

    fun isHost(other: SavedHost) = key == other.key
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
     * host it last browsed and the certificate authorities it was told to
     * trust: that host is the list's first, followed by those hosts.
     */
    val hosts: List<SavedHost>
        get() {
            val stored = prefs.getString(HOSTS, null) ?: return formerHosts()
            val list = JSONArray(stored)
            return (0 until list.length()).map {
                val item = list.getJSONArray(it)
                SavedHost(item.getString(0), item.getInt(1), item.getString(2))
            }
        }

    private fun formerHosts(): List<SavedHost> {
        val last = prefs.getString(LAST_HOST, "").orEmpty().trim()
        val first = if (last.isEmpty()) emptyList() else listOf(SavedHost(last, prefs.getInt(LAST_PORT, Entry.DEFAULT_PORT)))
        val trusted =
            prefs.all.keys
                .mapNotNull { key ->
                    val endpoint = key.takeIf { it.startsWith(CA_PREFIX) }?.removePrefix(CA_PREFIX)
                    val colon = endpoint?.lastIndexOf(':') ?: -1
                    val port = endpoint?.substring(colon + 1)?.toIntOrNull()
                    if (endpoint == null || colon <= 0 || port == null) null else SavedHost(endpoint.substring(0, colon), port)
                }
                .sortedBy { it.location.lowercase() }
        return (first + trusted).fold(emptyList<SavedHost>()) { list, host -> if (list.any { it.isHost(host) }) list else list + host }
    }

    /**
     * Add [host], or with [replacing] put it in that host's place. A host
     * already in the list that is the same host is not listed twice. The
     * certificate authorities trusted for it, and for the entries it
     * replaces, are the engine's to work out
     * ([saveTrustedCertificates]): [certificate] is what the editor holds,
     * [loaded] what it showed when it opened, and a save whose host's
     * authorities changed since is refused ([GoException]). The list and
     * the certificate authorities change together.
     */
    fun saveHost(replacing: SavedHost?, loaded: String, host: SavedHost, certificate: String) {
        val list = hosts
        // Where the host goes: the replaced host's place, else its own.
        val at =
            replacing?.let { old -> list.indexOfFirst { it.isHost(old) } }?.takeIf { it >= 0 }
                ?: list.indexOfFirst { it.isHost(host) }.takeIf { it >= 0 }
        val replaced = list.filterIndexed { i, other -> i == at || other.isHost(host) }
        val updated =
            list
                .mapIndexedNotNull { i, other ->
                    when {
                        i == at -> host
                        other.isHost(host) -> null
                        else -> other
                    }
                }
                .let { if (at == null) it + host else it }
        val before = certificates()
        val after =
            saveTrustedCertificates(
                before,
                replacing?.endpoint,
                loaded,
                host.endpoint,
                certificate,
                replaced.map { it.endpoint },
            )
        prefs.edit().putHosts(updated).putCertificates(before, after).apply()
    }

    /** Remove [host] from the list, with the certificate authorities trusted for it. */
    fun removeHost(host: SavedHost) {
        val (removed, kept) = hosts.partition { it.isHost(host) }
        val before = certificates()
        val after = forgetTrustedCertificates(before, (removed + host).map { it.endpoint })
        prefs.edit().putHosts(kept).putCertificates(before, after).apply()
    }

    /**
     * The PEM certificate authorities trusted for one host and port only,
     * under any spelling of its address ([trustedCertificate]).
     */
    fun certificate(host: String, port: Int): String = trustedCertificate(certificates(), host, port.toUShort())

    /** The saved certificate authorities, by `address:port` as typed. */
    private fun certificates(): Map<String, String> =
        prefs.all.entries
            .filter { it.key.startsWith(CA_PREFIX) }
            .mapNotNull { entry -> (entry.value as? String)?.let { entry.key.removePrefix(CA_PREFIX) to it } }
            .toMap()

    /** Write the certificate authorities [after] in place of [before]. */
    private fun SharedPreferences.Editor.putCertificates(before: Map<String, String>, after: Map<String, String>) =
        apply {
            for (key in before.keys - after.keys) remove(CA_PREFIX + key)
            for ((key, pem) in after) if (before[key] != pem) putString(CA_PREFIX + key, pem)
        }

    private fun SharedPreferences.Editor.putHosts(list: List<SavedHost>) =
        putString(HOSTS, JSONArray(list.map { JSONArray().put(it.address).put(it.port).put(it.name) }).toString())
            .remove(LAST_HOST)
            .remove(LAST_PORT)

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
        const val CA_PREFIX = "ca:"
        const val MAX_RECENT = 30
        // Room an import leaves on the device for the files it opens.
        const val SPARE_BYTES = 256L shl 20
    }
}
