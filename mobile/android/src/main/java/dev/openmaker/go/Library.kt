package dev.openmaker.go

import android.content.Context
import android.content.SharedPreferences
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
     * already in the list at the same address and port, in any case, is
     * that host: it is not listed twice, and the entries it replaces no
     * longer trust anything at their own address. [certificate] is the
     * certificate authority trusted for it, and with it any the editor did
     * not show: Edit Host shows the edited host's, Add Host none, so those
     * saved for the host or an entry it replaces are added, never dropped.
     * The list and the certificate authorities change together.
     */
    fun saveHost(replacing: SavedHost?, host: SavedHost, certificate: String) {
        val list = hosts
        // Where the host goes: the replaced host's place, else its own.
        val at =
            replacing?.let { old -> list.indexOfFirst { it.isHost(old) } }?.takeIf { it >= 0 }
                ?: list.indexOfFirst { it.isHost(host) }.takeIf { it >= 0 }
        val replaced = list.filterIndexed { i, other -> i == at || other.isHost(host) } + listOfNotNull(replacing)
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
        val unseen = (listOf(host) + replaced).filterNot { replacing?.isHost(it) == true }
        val pem = mergedPem(listOf(certificate) + unseen.map { this.certificate(it.address, it.port) })
        val edit = prefs.edit().putHosts(updated)
        for (old in replaced) edit.trust(old.address, old.port, "")
        edit.trust(host.address, host.port, pem).apply()
    }

    /** Remove [host] from the list, with the certificate authorities trusted for it. */
    fun removeHost(host: SavedHost) {
        val (removed, kept) = hosts.partition { it.isHost(host) }
        val edit = prefs.edit().putHosts(kept)
        for (old in removed + host) edit.trust(old.address, old.port, "")
        edit.apply()
    }

    /**
     * The PEM certificate authorities trusted for one host and port only,
     * its address matched in any case, as hosts are. The old Hosts tab could
     * save one under each spelling of an address: all of them, the one
     * under this spelling first, then the others in order, each once.
     */
    fun certificate(host: String, port: Int): String {
        val key = caKey(host, port)
        return mergedPem(
            prefs.all.entries
                .filter { it.key.equals(key, ignoreCase = true) }
                .sortedWith(compareBy({ it.key != key }, { it.key }))
                .mapNotNull { it.value as? String }
        )
    }

    private fun caKey(host: String, port: Int) = "$CA_PREFIX$host:$port"

    /**
     * [pems] as one, each certificate once, in order: the engine trusts
     * every certificate in the text. Text with no PEM block is kept whole.
     */
    private fun mergedPem(pems: List<String>): String {
        val blocks = LinkedHashSet<String>()
        for (pem in pems) {
            val found = PEM_BLOCK.findAll(pem).map { it.value.trim() }.toList()
            if (found.isNotEmpty()) blocks += found else if (pem.isNotBlank()) blocks += pem.trim()
        }
        return blocks.joinToString("\n")
    }

    private fun SharedPreferences.Editor.putHosts(list: List<SavedHost>) =
        putString(HOSTS, JSONArray(list.map { JSONArray().put(it.address).put(it.port).put(it.name) }).toString())
            .remove(LAST_HOST)
            .remove(LAST_PORT)

    /**
     * Save [pem] as the host's certificate authority, in place of any kept
     * under its address in another case; an empty one removes them.
     */
    private fun SharedPreferences.Editor.trust(host: String, port: Int, pem: String): SharedPreferences.Editor {
        val key = caKey(host, port)
        for (other in prefs.all.keys) if (other.equals(key, ignoreCase = true)) remove(other)
        if (pem.isNotBlank()) putString(key, pem)
        return this
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
        const val CA_PREFIX = "ca:"
        val PEM_BLOCK = Regex("-----BEGIN [^-]+-----[\\s\\S]*?-----END [^-]+-----")
        const val MAX_RECENT = 30
        // Room an import leaves on the device for the files it opens.
        const val SPARE_BYTES = 256L shl 20
    }
}
