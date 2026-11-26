package dev.openmaker.go

import android.content.Context
import android.content.SharedPreferences
import android.net.Uri
import android.os.StatFs
import android.provider.OpenableColumns
import java.io.File
import dev.openmaker.go.engine.HostChange
import dev.openmaker.go.engine.ListedHost
import dev.openmaker.go.engine.SavedHosts
import dev.openmaker.go.engine.distinctHosts
import dev.openmaker.go.engine.formerHosts
import dev.openmaker.go.engine.hostKey
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

    /** As the engine takes it. */
    fun listed() = ListedHost(address, port.toUShort(), name)

    fun isHost(address: String, port: Int) = key == hostKey(address, port.toUShort())

    fun isHost(other: SavedHost) = key == other.key

    companion object {
        fun of(listed: ListedHost) = SavedHost(listed.address, listed.port.toInt(), listed.name)
    }
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
     * The Hosts list, each host once ([distinctHosts]). Before it was kept,
     * the Hosts tab remembered the one host it last browsed and the
     * certificate authorities it was told to trust: that host is the list's
     * first, followed by those hosts ([formerHosts]).
     */
    val hosts: List<SavedHost>
        get() {
            val stored =
                prefs.getString(HOSTS, null)
                    ?: return formerHosts(
                            prefs.getString(LAST_HOST, "").orEmpty(),
                            prefs.getInt(LAST_PORT, Entry.DEFAULT_PORT).toString(),
                            certificates(),
                        )
                        .map(SavedHost::of)
            val list = JSONArray(stored)
            val listed =
                (0 until list.length()).map {
                    val item = list.getJSONArray(it)
                    ListedHost(item.getString(0), item.getInt(1).toUShort(), item.getString(2))
                }
            return distinctHosts(listed).map(SavedHost::of)
        }

    /**
     * Save Add Host or Edit Host's [change], by the engine's rules
     * ([dev.openmaker.go.engine.saveHost]): the saved host, or a
     * [GoException] saying what to fix, or that the host was changed in
     * another window since the editor opened. The list and the certificate
     * authorities change together.
     */
    fun saveHost(change: HostChange): SavedHost {
        val before = certificates()
        val hostsBefore = hosts
        val saved = dev.openmaker.go.engine.saveHost(SavedHosts(hostsBefore.map { it.listed() }, before), change)
        val edit =
            prefs.edit().putHosts(saved.saved.hosts.map(SavedHost::of)).putCertificates(before, saved.saved.certificates)
        // A host that left the list (moved, or merged into another) loses the certificate permitted for it.
        val after = saved.saved.hosts.map { hostKey(it.address, it.port) }
        for (old in hostsBefore + listOfNotNull(change.replaces?.let(SavedHost::of))) {
            if (old.key !in after) edit.unpermit(old.address, old.port)
        }
        edit.apply()
        return SavedHost.of(saved.host)
    }

    /**
     * Remove [host] from the list, with the certificate authorities trusted
     * for it and the certificate permitted for it.
     */
    fun removeHost(host: SavedHost) {
        val before = certificates()
        val saved = dev.openmaker.go.engine.removeHost(SavedHosts(hosts.map { it.listed() }, before), host.listed())
        prefs.edit()
            .putHosts(saved.hosts.map(SavedHost::of))
            .putCertificates(before, saved.certificates)
            .unpermit(host.address, host.port)
            .apply()
    }

    /**
     * The SHA-256 fingerprint of the server certificate the user permitted
     * for one host and port although it cannot be verified (Always
     * Permit), under any spelling of its address ([hostKey]); empty for
     * none.
     */
    fun permitted(host: String, port: Int): String =
        prefs.getString(pinKey(host, port), null)
            ?: pins(host, port).sortedBy { it.key }.firstNotNullOfOrNull { it.value as? String }.orEmpty()

    /** Always Permit: connect to the host's certificate with [fingerprint] from now on. */
    fun permit(host: String, port: Int, fingerprint: String) {
        prefs.edit().unpermit(host, port).putString(pinKey(host, port), fingerprint).apply()
    }

    /** Forget the certificate permitted for a host and port. */
    fun forgetPermitted(host: String, port: Int) {
        prefs.edit().unpermit(host, port).apply()
    }

    /**
     * The PEM certificate authorities Edit Host shows for one host and port
     * only, under any spelling of its address ([trustedCertificate]).
     */
    fun certificate(host: String, port: Int): String = trustedCertificate(certificates(), host, port.toUShort())

    /**
     * The certificate authorities a connection to one host and port trusts
     * ([dev.openmaker.go.engine.connectCertificate]): every one saved for
     * it, or a [GoException] saying why none can be used.
     */
    fun connectCertificate(host: String, port: Int): String =
        dev.openmaker.go.engine.connectCertificate(certificates(), host, port.toUShort())

    /** The saved certificate authorities, by `address:port` as typed. */
    private fun certificates(): Map<String, String> =
        prefs.all.entries
            .filter { it.key.startsWith(CA_PREFIX) }
            .mapNotNull { entry -> (entry.value as? String)?.let { entry.key.removePrefix(CA_PREFIX) to it } }
            .toMap()

    private fun pinKey(host: String, port: Int) = "$PIN_PREFIX$host:$port"

    /** The permits saved for [host] and [port] under any spelling of the address. */
    private fun pins(host: String, port: Int): List<Map.Entry<String, *>> {
        val key = hostKey(host, port.toUShort())
        return prefs.all.entries.filter { entry ->
            val stored = entry.key.removePrefix(PIN_PREFIX).takeIf { entry.key.startsWith(PIN_PREFIX) }
            val colon = stored?.lastIndexOf(':') ?: -1
            val storedPort = stored?.substring(colon + 1)?.toUShortOrNull()
            stored != null && colon > 0 && storedPort != null && hostKey(stored.substring(0, colon), storedPort) == key
        }
    }

    private fun SharedPreferences.Editor.unpermit(host: String, port: Int): SharedPreferences.Editor = apply {
        for (entry in pins(host, port)) remove(entry.key)
    }

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
        const val PIN_PREFIX = "permitted:"
        const val MAX_RECENT = 30
        // Room an import leaves on the device for the files it opens.
        const val SPARE_BYTES = 256L shl 20
    }
}
