package dev.openmaker.go

import android.app.Application
import android.net.Uri
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.openmaker.go.engine.Document
import dev.openmaker.go.engine.GoException
import dev.openmaker.go.engine.Host
import dev.openmaker.go.engine.HostChange
import dev.openmaker.go.engine.hostKey
import dev.openmaker.go.engine.Input
import dev.openmaker.go.engine.Permit
import dev.openmaker.go.engine.SignIn
import dev.openmaker.go.engine.Source
import dev.openmaker.go.engine.Viewport
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

enum class Tab(val title: String) {
    MyApps("My Apps"),
    Favorites("Favorites"),
    Recent("Recent"),
    Hosts("Hosts"),
}

/** The sign-in a file asks for: the open fails back into it with [error]. */
data class SignInRequest(val entry: Entry, val error: String? = null, val opening: Boolean = false)

/**
 * Add Host, or Edit Host for the host it [replaces]: the fields as typed,
 * and why the last Save was refused.
 */
data class HostEditor(
    val replaces: SavedHost? = null,
    val address: String = "",
    val port: String = Entry.DEFAULT_PORT.toString(),
    val name: String = "",
    val certificate: String = "",
    /** The certificate authorities Edit Host showed when it opened. */
    val loaded: String = "",
    /** The fingerprint of the certificate permitted for the host edited; empty for none. */
    val permitted: String = "",
    val error: String? = null,
)

/**
 * A host whose certificate could not be verified, as FileMaker Go asks
 * about one: Always Permit, Connect (this time) or Don't Connect. [retry]
 * does again what met it once the user permits the certificate.
 */
class TrustPrompt(val untrusted: GoException.Untrusted, val retry: () -> Unit)

/** A host's file list, as its page shows it. */
sealed interface Listing {
    /** Being asked for. Each request is its own: a reply to an older one is dropped. */
    class Pending : Listing

    data class Listed(val files: List<Entry>) : Listing

    data class Failed(val reason: String) : Listing
}

/**
 * The Launch Center and the open file. Held for the activity's whole life,
 * so a recreated activity finds the file still open.
 */
class GoViewModel(app: Application) : AndroidViewModel(app) {
    private val library = Library(app)
    private val vault = LicenseVault(app)

    var tab by mutableStateOf(Tab.MyApps)
    var local by mutableStateOf(library.localFiles())
        private set
    var favorites by mutableStateOf(library.favorites)
        private set
    var recent by mutableStateOf(library.recent)
        private set
    var hosts by mutableStateOf(library.hosts)
        private set
    /** The host whose files the Hosts tab shows; null for the list of hosts. */
    var hostPage by mutableStateOf<SavedHost?>(null)
        private set
    /** Each host's file list, by its address (as typed) and port, while the app runs. */
    var listings by mutableStateOf(mapOf<Pair<String, Int>, Listing>())
        private set
    var hostEditor by mutableStateOf<HostEditor?>(null)
    var trustPrompt by mutableStateOf<TrustPrompt?>(null)
        private set
    /** Certificates permitted with Connect, for as long as the app runs, by host ([hostKey]). */
    private val connectedOnce = mutableMapOf<String, Permit>()
    var working by mutableStateOf(false)
        private set
    var message by mutableStateOf<String?>(null)
    var signIn by mutableStateOf<SignInRequest?>(null)
    var session by mutableStateOf<DocumentSession?>(null)
        private set
    var settings by mutableStateOf(false)
    /** Whether Settings holds a FileMaker client license. */
    var licenseSaved by mutableStateOf(vault.saved)
        private set
    private var opening: Job? = null

    /** The activity is started (between ON_START and ON_STOP). */
    private var foreground = true

    fun refresh() {
        local = library.localFiles()
        favorites = library.favorites
        recent = library.recent
        hosts = library.hosts
    }

    fun import(uri: Uri) = background({ library.import(uri) }) { refresh() }

    fun delete(entry: Entry) {
        library.delete(entry)
        refresh()
    }

    fun toggleFavorite(entry: Entry) {
        library.favorites = if (entry in library.favorites) library.favorites - entry else library.favorites + entry
        refresh()
    }

    fun forget(entry: Entry) {
        library.recent = library.recent - entry
        refresh()
    }

    /** Save the FileMaker client license typed in Settings. */
    fun saveLicense(license: String) {
        val key = license.trim()
        if (key.isEmpty()) return
        background({ vault.save(key) }) { licenseSaved = vault.saved }
    }

    fun removeLicense() = background({ vault.remove() }) { licenseSaved = vault.saved }

    /**
     * A hosted entry's host, with the certificate authorities it trusts and
     * the certificate permitted for it, or a [GoException] when its
     * certificate authorities cannot all be used.
     */
    private fun host(entry: Entry) = endpoint(entry.host, entry.port)

    /**
     * A host as the engine connects to it: with the certificate authorities
     * trusted for it, and the certificate permitted for it, the one
     * permitted with Connect in this run first (a newer certificate than
     * one saved with Always Permit); and those permitted for other hosts,
     * for a file a script opens there.
     */
    private fun endpoint(address: String, port: Int): Host {
        val key = hostKey(address, port.toUShort())
        val elsewhere = (library.permits() + connectedOnce.values).filter { hostKey(it.address, it.port) != key }
        return Host(
            address,
            port.toUShort(),
            library.connectCertificate(address, port),
            connectedOnce[key]?.fingerprint ?: library.permitted(address, port),
            elsewhere,
        )
    }

    /** Ask about a host whose certificate could not be verified, and do [retry] again if it is permitted. */
    private fun askTrust(untrusted: GoException.Untrusted, retry: () -> Unit) {
        trustPrompt = TrustPrompt(untrusted, retry)
    }

    /**
     * Connect (this time), or with [always] Always Permit: the certificate
     * [prompt] asked about is accepted for its host. Only the prompt shown
     * now is answered.
     */
    fun permit(prompt: TrustPrompt, always: Boolean) {
        if (trustPrompt !== prompt) return
        trustPrompt = null
        val untrusted = prompt.untrusted
        val key = hostKey(untrusted.host, untrusted.port)
        if (always) {
            library.permit(untrusted.host, untrusted.port.toInt(), untrusted.fingerprint)
            connectedOnce.remove(key)
        } else {
            connectedOnce[key] = Permit(untrusted.host, untrusted.port, untrusted.fingerprint)
        }
        prompt.retry()
    }

    /** Don't Connect. */
    fun dontConnect(prompt: TrustPrompt) {
        if (trustPrompt === prompt) trustPrompt = null
    }

    /** Forget the certificate permitted for the host Edit Host edits. */
    fun forgetPermitted() {
        val editor = hostEditor ?: return
        val host = editor.replaces ?: return
        library.forgetPermitted(host.address, host.port)
        connectedOnce.remove(host.key)
        forgetListing(host)
        hostEditor = editor.copy(permitted = "")
    }

    /** Add Host. */
    fun addHost() {
        hostEditor = HostEditor()
    }

    /** Edit Host, with the certificate authority trusted for it. */
    fun editHost(host: SavedHost) {
        val certificate = library.certificate(host.address, host.port)
        hostEditor =
            HostEditor(
                host,
                host.address,
                host.port.toString(),
                host.name,
                certificate,
                loaded = certificate,
                permitted = library.permitted(host.address, host.port),
            )
    }

    /**
     * Save the host Add Host or Edit Host describes, checked as the
     * desktop's Add Host checks one, or say in the editor what to fix. A
     * host whose page is shown stays shown, at its new address.
     */
    fun saveHost() {
        val editor = hostEditor ?: return
        val host =
            try {
                library.saveHost(
                    HostChange(
                        editor.replaces?.listed(),
                        editor.loaded,
                        editor.address,
                        editor.port,
                        editor.name,
                        // As typed: the engine checks its bounds before anything else.
                        editor.certificate,
                    )
                )
            } catch (error: GoException) {
                hostEditor = editor.copy(error = error.goMessage())
                return
            }
        // Nor does a certificate permitted with Connect stay with an address or port the host left.
        editor.replaces?.takeUnless { it.isHost(host) }?.let { connectedOnce.remove(it.key) }
        hosts = library.hosts
        hostEditor = null
        // Its files are listed again: the address, port or certificate
        // authority they were listed with may have changed.
        editor.replaces?.let { forgetListing(it) }
        forgetListing(host)
        if (hostPage?.let { shown -> editor.replaces?.isHost(shown) == true || shown.isHost(host) } == true) {
            showHost(host)
        }
    }

    fun removeHost(host: SavedHost) {
        library.removeHost(host)
        connectedOnce.remove(host.key)
        hosts = library.hosts
        forgetListing(host)
        if (hostPage?.isHost(host) == true) hostPage = null
    }

    /** Show [host]'s files, listing them if they have not been. */
    fun showHost(host: SavedHost) {
        hostPage = host
        if (listings[key(host)] == null) listHost(host)
    }

    /** Back from a host's files to the list of hosts. */
    fun showHosts() {
        hostPage = null
    }

    /**
     * Ask [host] for the files it lists. The answer shows on its page; one
     * that comes after the host was changed or removed, or asked again, is
     * dropped.
     */
    fun listHost(host: SavedHost) {
        val key = key(host)
        // Read here, as the permits are kept on this thread; a host whose
        // certificate authorities cannot all be used fails as a listing.
        val endpoint = runCatching { endpoint(host.address, host.port) }
        val asked = Listing.Pending()
        listings = listings + (key to asked)
        viewModelScope.launch {
            val result =
                withContext(Dispatchers.IO) { endpoint.mapCatching { dev.openmaker.go.engine.hostedFiles(it) } }
            if (listings[key] !== asked) return@launch
            (result.exceptionOrNull() as? GoException.Untrusted)?.let { askTrust(it) { listHost(host) } }
            listings =
                listings +
                    (key to
                        result.fold(
                            { names -> Listing.Listed(names.map { Entry(it, host.address, host.port) }) },
                            { Listing.Failed(it.goMessage()) },
                        ))
        }
    }

    /** Sign in to a file [host] does not list, by its name ([name].fmp12). */
    fun openByName(host: SavedHost, name: String) {
        if (name.isBlank()) return
        signIn = SignInRequest(Entry(dev.openmaker.go.engine.hostedFileName(name), host.address, host.port))
    }

    private fun key(host: SavedHost) = host.address to host.port

    private fun forgetListing(host: SavedHost) {
        listings = listings - key(host)
    }

    /**
     * Open a file chosen in the Launch Center. A local file opens as
     * FileMaker Go opens one, signed in by the file's own settings, and asks
     * for an account only when the file needs one; a hosted file asks first.
     */
    fun choose(entry: Entry, viewport: Viewport) {
        if (opening?.isActive == true || session != null) return
        if (entry.local) open(entry, "", "", "", viewport, prompted = false)
        else signIn = SignInRequest(entry)
    }

    /**
     * Open a file with the sign-in the user typed ([prompted]), or with none.
     * A refused account, or a file that needs one, keeps or opens the
     * sign-in with the reason; other failures show as a message.
     */
    fun open(entry: Entry, account: String, password: String, license: String, viewport: Viewport, prompted: Boolean = true) {
        // One open at a time, and none while a file is open.
        if (opening?.isActive == true || session != null) return
        if (prompted) {
            signIn = SignInRequest(entry, opening = true)
        } else {
            if (working) return
            working = true
        }
        opening = viewModelScope.launch {
            val signal = FrameSignal()
            // Read here, as the permits are kept on this thread; a host whose
            // certificate authorities cannot all be used fails as an open.
            val source = runCatching {
                if (entry.local) Source.Local(library.path(entry)) else Source.Hosted(host(entry), entry.name)
            }
            // An open can't be interrupted, and its document belongs to this
            // coroutine until the session takes it: if the view model is
            // cleared first (the activity went), even as the open's result
            // comes back, the document is closed rather than left open.
            var opened: Document? = null
            try {
                val result =
                    withContext(Dispatchers.IO + NonCancellable) {
                        source.mapCatching { source ->
                            // A hosted sign-in with no license typed sends
                            // the one saved in Settings.
                            val key = license.trim().ifEmpty { if (entry.local) "" else vault.load() }
                            Document.open(source, SignIn(account, password, key), viewport, signal)
                        }
                            .onSuccess { opened = it }
                    }
                if (!prompted) working = false
                // An unreadable saved license was dropped as it was read.
                licenseSaved = vault.saved
                result
                    .onSuccess { document ->
                        signIn = null
                        // A file opened meanwhile keeps its place; this one
                        // is closed below.
                        if (session != null) return@onSuccess
                        session = DocumentSession(entry, document, signal)
                        opened = null
                        // The app may have left the foreground during the open.
                        if (!foreground) session?.send(Input.Active(false))
                        library.recent = listOf(entry) + library.recent
                        refresh()
                    }
                    .onFailure { error ->
                        when (error) {
                            // A file that signs in with no account of its own,
                            // or whose own account is refused, asks for one.
                            is GoException.Credentials -> signIn = SignInRequest(entry, error.reason.takeIf { prompted })
                            is GoException.SignInRequired -> signIn = SignInRequest(entry, error.reason.takeIf { prompted })
                            // Asked about, and opened again with the same sign-in if permitted.
                            is GoException.Untrusted -> {
                                signIn = SignInRequest(entry, error.reason.takeIf { prompted })
                                askTrust(error) { open(entry, account, password, license, viewport, prompted) }
                            }
                            else -> {
                                signIn = null
                                message = error.goMessage()
                            }
                        }
                    }
            } finally {
                // Letting go closes the file, on the engine's own thread.
                opened?.destroy()
            }
        }
    }

    /**
     * The open file closed (Close File, a script or the host), or its
     * document stopped on a [failure]: back to the Launch Center, which
     * shows the failure.
     */
    fun documentClosed(closed: DocumentSession, failure: String?) {
        if (session !== closed) return
        session = null
        failure?.let { message = it }
        viewModelScope.launch(Dispatchers.IO) { closed.release() }
    }

    /** The app left the foreground or came back: leaving saves the record. */
    fun setActive(active: Boolean) {
        foreground = active
        session?.send(Input.Active(active))
    }

    override fun onCleared() {
        // The activity is gone for good. Letting go of the document closes
        // the file as Close File does, saving what was typed, on the
        // engine's own thread: a close the file's trigger just refused is
        // not asked again.
        session?.release()
        session = null
    }

    private fun <T> background(work: () -> T, done: (T) -> Unit) {
        if (working) return
        working = true
        viewModelScope.launch {
            val result = withContext(Dispatchers.IO) { runCatching(work) }
            working = false
            result.onSuccess(done).onFailure { message = it.goMessage() }
        }
    }
}

fun Throwable.goMessage(): String =
    when (this) {
        is GoException.Credentials -> reason
        is GoException.SignInRequired -> reason
        is GoException.Failed -> reason
        is GoException.Untrusted -> reason
        else -> localizedMessage ?: "The operation failed."
    }
