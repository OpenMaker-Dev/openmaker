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
import dev.openmaker.go.engine.Input
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

/** The host form of the Hosts tab. */
data class HostForm(val address: String, val port: String, val file: String = "", val certificate: String = "")

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
    var hostForm by mutableStateOf(
        HostForm(
            library.lastHost,
            library.lastPort.toString(),
            certificate = library.certificate(library.lastHost.trim(), library.lastPort),
        )
    )
    var hosted by mutableStateOf(emptyList<Entry>())
        private set
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

    /** The certificate authority a hosted entry trusts. */
    private fun host(entry: Entry) = Host(entry.host, entry.port.toUShort(), library.certificate(entry.host, entry.port))

    /**
     * The host form's address or port changed. The certificate authority
     * in the form belongs to its host and port: another endpoint's is the
     * one saved for it (none if none was), never one typed for the last.
     */
    fun editHost(address: String = hostForm.address, port: String = hostForm.port) {
        val form = hostForm
        val moved =
            address.trim() != form.address.trim() || port.trim().toIntOrNull() != form.port.trim().toIntOrNull()
        val certificate =
            if (!moved) form.certificate
            else port.trim().toIntOrNull()?.let { library.certificate(address.trim(), it) }.orEmpty()
        hostForm = form.copy(address = address, port = port, certificate = certificate)
    }

    /** Browse the form's host, or with a file name go straight to its sign-in. */
    fun browseHost() {
        val form = hostForm
        val address = form.address.trim()
        val port = form.port.trim().toIntOrNull()
        if (address.isEmpty() || port == null || port !in 1..65535) {
            message = "Enter a host and a port from 1 to 65535."
            return
        }
        // A certificate authority belongs to the host and port it was
        // entered for. The form shows the one saved for them, so a field
        // the user cleared removes it.
        val certificate = form.certificate.trim()
        val remember = {
            library.lastHost = address
            library.lastPort = port
            library.trust(address, port, certificate)
        }
        val file = form.file.trim()
        if (file.isNotEmpty()) {
            remember()
            signIn = SignInRequest(Entry(file, address, port))
            return
        }
        background({ dev.openmaker.go.engine.hostedFiles(Host(address, port.toUShort(), certificate)) }) { names ->
            remember()
            hosted = names.map { Entry(it, address, port) }
            if (names.isEmpty()) message = "This host lists no files."
        }
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
            val source = if (entry.local) Source.Local(library.path(entry)) else Source.Hosted(host(entry), entry.name)
            // An open can't be interrupted, and its document belongs to this
            // coroutine until the session takes it: if the view model is
            // cleared first (the activity went), even as the open's result
            // comes back, the document is closed rather than left open.
            var opened: Document? = null
            try {
                val result =
                    withContext(Dispatchers.IO + NonCancellable) {
                        runCatching {
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
        else -> localizedMessage ?: "The operation failed."
    }
