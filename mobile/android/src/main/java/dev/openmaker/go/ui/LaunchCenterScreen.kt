package dev.openmaker.go.ui

import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.DateRange
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.openmaker.go.Entry
import dev.openmaker.go.GoViewModel
import dev.openmaker.go.HostEditor
import dev.openmaker.go.Listing
import dev.openmaker.go.SavedHost
import dev.openmaker.go.Tab

private val Tab.icon: ImageVector
    get() =
        when (this) {
            Tab.MyApps -> Icons.Filled.Home
            Tab.Favorites -> Icons.Filled.Star
            Tab.Recent -> Icons.Filled.DateRange
            Tab.Hosts -> Icons.Filled.Share
        }

/** FileMaker Go's Launch Center: My Apps, Favorites, Recent and Hosts. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LaunchCenterScreen(model: GoViewModel) {
    val importer =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri -> uri?.let(model::import) }
    val page = model.hostPage.takeIf { model.tab == Tab.Hosts }
    var openingByName by remember { mutableStateOf(false) }
    // Back from a host's files goes to the list of hosts.
    BackHandler(enabled = page != null) { model.showHosts() }
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    if (page == null) {
                        Text(model.tab.title)
                    } else {
                        Column {
                            Text(page.label, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            if (page.name.isNotEmpty()) {
                                Text(
                                    page.location,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                    }
                },
                navigationIcon = {
                    if (page != null) {
                        IconButton(onClick = model::showHosts) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Hosts") }
                    }
                },
                actions = {
                    when {
                        model.tab == Tab.MyApps ->
                            // .fmp12 files have no MIME type of their own.
                            IconButton(onClick = { importer.launch(arrayOf("*/*")) }) {
                                Icon(Icons.Filled.Add, "Import a file")
                            }
                        page != null -> {
                            TextButton(onClick = { openingByName = true }) { Text("Open by Name…") }
                            IconButton(
                                onClick = { model.listHost(page) },
                                enabled = model.listings[page.address to page.port] !is Listing.Pending,
                            ) {
                                Icon(Icons.Filled.Refresh, "Refresh")
                            }
                        }
                        model.tab == Tab.Hosts ->
                            IconButton(onClick = model::addHost) { Icon(Icons.Filled.Add, "Add Host") }
                    }
                    IconButton(onClick = { model.settings = true }) { Icon(Icons.Filled.Settings, "Settings") }
                },
            )
        },
        bottomBar = {
            NavigationBar {
                for (tab in Tab.entries) {
                    NavigationBarItem(
                        selected = model.tab == tab,
                        onClick = { model.tab = tab },
                        icon = { Icon(tab.icon, null) },
                        label = { Text(tab.title) },
                    )
                }
            }
        },
    ) { padding ->
        var deleting by remember { mutableStateOf<Entry?>(null) }
        Column(Modifier.padding(padding).fillMaxSize()) {
            if (model.working) LinearProgressIndicator(Modifier.fillMaxWidth())
            val viewport = screenViewport()
            val open = { entry: Entry -> model.choose(entry, viewport) }
            when (model.tab) {
                Tab.MyApps ->
                    Files(
                        model,
                        model.local,
                        "Import a FileMaker app to open it here.",
                        open,
                        listOf(RowAction("Delete") { deleting = it }),
                    )
                Tab.Favorites -> Files(model, model.favorites, "Star a file to add it here.", open)
                Tab.Recent ->
                    Files(model, model.recent, "Files you open appear here.", open, listOf(RowAction("Remove", model::forget)))
                Tab.Hosts -> if (page == null) Hosts(model) else HostFiles(model, page, open)
            }
        }
        if (openingByName && page != null) {
            AskDialog(
                "Open by Name",
                "File name",
                "Sign In",
                onConfirm = { name ->
                    openingByName = false
                    model.openByName(page, name)
                },
                onDismiss = { openingByName = false },
            )
        }
        model.hostEditor?.let { editor ->
            HostDialog(
                editor,
                onEdit = { model.hostEditor = it.copy(error = null) },
                onSave = model::saveHost,
                onCancel = { model.hostEditor = null },
                onForgetPermitted = model::forgetPermitted,
            )
        }
        deleting?.let { entry ->
            ConfirmDelete(
                entry,
                onDelete = {
                    model.delete(entry)
                    deleting = null
                },
                onCancel = { deleting = null },
            )
        }
    }
}

/** A command in a file row's menu. */
private class RowAction(val label: String, val run: (Entry) -> Unit)

@Composable
private fun Files(
    model: GoViewModel,
    entries: List<Entry>,
    empty: String,
    open: (Entry) -> Unit,
    actions: List<RowAction> = emptyList(),
) {
    if (entries.isEmpty()) {
        Text(empty, Modifier.padding(24.dp), color = MaterialTheme.colorScheme.onSurfaceVariant)
        return
    }
    LazyColumn(contentPadding = PaddingValues(vertical = 8.dp)) {
        items(entries, key = { listOf(it.name, it.host, it.port) }) { entry ->
            FileRow(entry, favorite = entry in model.favorites, open, { model.toggleFavorite(entry) }, actions)
        }
    }
}

@Composable
private fun FileRow(
    entry: Entry,
    favorite: Boolean,
    open: (Entry) -> Unit,
    toggleFavorite: () -> Unit,
    actions: List<RowAction>,
    // Not on a host's page, whose title names the host.
    subtitle: Boolean = true,
) {
    ListItem(
        headlineContent = { Text(entry.name) },
        supportingContent = if (subtitle) ({ Text(entry.subtitle) }) else null,
        trailingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                // Material's core icons have no hollow star: a file that is
                // not a favorite gets a faint one.
                IconButton(onClick = toggleFavorite) {
                    if (favorite) Icon(Icons.Filled.Star, "Remove favorite", tint = MaterialTheme.colorScheme.primary)
                    else Icon(Icons.Filled.Star, "Add favorite", tint = MaterialTheme.colorScheme.outlineVariant)
                }
                if (actions.isNotEmpty()) {
                    var expanded by remember { mutableStateOf(false) }
                    Box {
                        IconButton(onClick = { expanded = true }) { Icon(Icons.Filled.MoreVert, "More") }
                        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                            for (action in actions) {
                                DropdownMenuItem(
                                    text = { Text(action.label) },
                                    onClick = {
                                        expanded = false
                                        action.run(entry)
                                    },
                                )
                            }
                        }
                    }
                }
            }
        },
        modifier = Modifier.clickable { open(entry) },
    )
}

@Composable
private fun ConfirmDelete(entry: Entry, onDelete: () -> Unit, onCancel: () -> Unit) {
    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text("Delete “${entry.name}”?") },
        text = { Text("The file is removed from this device. Copies elsewhere are kept.") },
        confirmButton = { TextButton(onClick = onDelete) { Text("Delete", color = MaterialTheme.colorScheme.error) } },
        dismissButton = { TextButton(onClick = onCancel) { Text("Cancel") } },
    )
}

/** The Hosts list: each host's name or address; a tap shows its files. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun Hosts(model: GoViewModel) {
    if (model.hosts.isEmpty()) {
        Column(Modifier.padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Add a host to see the files it shares.", color = MaterialTheme.colorScheme.onSurfaceVariant)
            Button(onClick = model::addHost) { Text("Add Host") }
        }
        return
    }
    LazyColumn(contentPadding = PaddingValues(vertical = 8.dp)) {
        items(model.hosts, key = { it.key }) { host ->
            var menu by remember { mutableStateOf(false) }
            ListItem(
                headlineContent = { Text(host.label) },
                supportingContent = if (host.name.isNotEmpty()) ({ Text(host.location) }) else null,
                leadingContent = { Icon(Tab.Hosts.icon, null) },
                trailingContent = {
                    Box {
                        IconButton(onClick = { menu = true }) { Icon(Icons.Filled.MoreVert, "More") }
                        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                            DropdownMenuItem(
                                text = { Text("Edit Host…") },
                                onClick = {
                                    menu = false
                                    model.editHost(host)
                                },
                            )
                            DropdownMenuItem(
                                text = { Text("Remove Host") },
                                onClick = {
                                    menu = false
                                    model.removeHost(host)
                                },
                            )
                        }
                    }
                },
                modifier = Modifier.combinedClickable(onLongClick = { menu = true }) { model.showHost(host) },
            )
        }
    }
}

/**
 * A host's files, with a search box; a pull down lists them again
 * (FileMaker Go's Hosts tab). A failed listing says why on the page.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun HostFiles(model: GoViewModel, host: SavedHost, open: (Entry) -> Unit) {
    val listing = model.listings[host.address to host.port]
    var search by rememberSaveable(host.address, host.port) { mutableStateOf("") }
    Column(Modifier.fillMaxSize()) {
        OutlinedTextField(
            search,
            { search = it },
            placeholder = { Text("Search Files") },
            leadingIcon = { Icon(Icons.Filled.Search, null) },
            singleLine = true,
            keyboardOptions = KeyboardOptions(autoCorrectEnabled = false, imeAction = ImeAction.Search),
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        )
        PullToRefreshBox(
            isRefreshing = listing is Listing.Pending,
            onRefresh = { model.listHost(host) },
            modifier = Modifier.fillMaxSize(),
        ) {
            // The pull needs something to scroll, also when there are no files.
            LazyColumn(Modifier.fillMaxSize()) {
                when (listing) {
                    is Listing.Listed -> {
                        val query = search.trim()
                        val files = listing.files.filter { it.name.contains(query, ignoreCase = true) }
                        when {
                            listing.files.isEmpty() -> item { PageNote("This host lists no files.") }
                            files.isEmpty() -> item { PageNote("No files match “$query”.") }
                            else ->
                                items(files, key = { it.name }) { entry ->
                                    FileRow(
                                        entry,
                                        entry in model.favorites,
                                        open,
                                        { model.toggleFavorite(entry) },
                                        emptyList(),
                                        subtitle = false,
                                    )
                                }
                        }
                    }
                    is Listing.Failed ->
                        item {
                            Column(Modifier.padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                                Text("The files could not be listed", style = MaterialTheme.typography.titleMedium)
                                Text(listing.reason, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    Button(onClick = { model.listHost(host) }) { Text("Try Again") }
                                    TextButton(onClick = { model.editHost(host) }) { Text("Edit Host…") }
                                }
                            }
                        }
                    is Listing.Pending,
                    null -> {}
                }
            }
        }
    }
}

@Composable
private fun PageNote(text: String) {
    Text(text, Modifier.padding(24.dp), color = MaterialTheme.colorScheme.onSurfaceVariant)
}

/** Add Host and Edit Host: the address, port, name and a private certificate authority. */
@Composable
private fun HostDialog(
    editor: HostEditor,
    onEdit: (HostEditor) -> Unit,
    onSave: () -> Unit,
    onCancel: () -> Unit,
    onForgetPermitted: () -> Unit,
) {
    var certificate by remember { mutableStateOf(editor.certificate.isNotEmpty()) }
    AlertDialog(
        onDismissRequest = onCancel,
        modifier = Modifier.keepClearOfKeyboard(),
        properties = TypingDialog,
        title = { Text(if (editor.replaces == null) "Add Host" else "Edit Host") },
        text = {
            // Scrolls, so the dialog's buttons stay above the keyboard.
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                // First, where the keyboard cannot hide it.
                editor.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                OutlinedTextField(
                    editor.address,
                    { onEdit(editor.copy(address = it)) },
                    label = { Text("Host name or IP address") },
                    singleLine = true,
                    keyboardOptions =
                        KeyboardOptions(
                            autoCorrectEnabled = false,
                            keyboardType = KeyboardType.Uri,
                            imeAction = ImeAction.Next,
                        ),
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    editor.port,
                    { onEdit(editor.copy(port = it)) },
                    label = { Text("Port") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Next),
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    editor.name,
                    { onEdit(editor.copy(name = it)) },
                    label = { Text("Name (optional)") },
                    supportingText = { Text("The Hosts list shows this name instead of the address.") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                    keyboardActions = KeyboardActions(onDone = { onSave() }),
                    modifier = Modifier.fillMaxWidth(),
                )
                if (certificate) {
                    OutlinedTextField(
                        editor.certificate,
                        { onEdit(editor.copy(certificate = it)) },
                        label = { Text("Certificate authority (PEM)") },
                        supportingText = { Text("Trusted for this host and port only.") },
                        minLines = 2,
                        maxLines = 5,
                        keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
                        modifier = Modifier.fillMaxWidth(),
                    )
                } else {
                    TextButton(onClick = { certificate = true }) { Text("Private certificate authority…") }
                }
                if (editor.permitted.isNotEmpty()) {
                    Text(
                        "Connects although its certificate cannot be verified: you permitted the one with SHA-256 fingerprint ${editor.permitted}.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                    TextButton(onClick = onForgetPermitted) { Text("Forget Permitted Certificate") }
                }
            }
        },
        confirmButton = { TextButton(onClick = onSave) { Text("Save") } },
        dismissButton = { TextButton(onClick = onCancel) { Text("Cancel") } },
    )
}
