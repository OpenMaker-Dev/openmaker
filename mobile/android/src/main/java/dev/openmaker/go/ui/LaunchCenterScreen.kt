package dev.openmaker.go.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
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
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.DateRange
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.MoreVert
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
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import dev.openmaker.go.Entry
import dev.openmaker.go.GoViewModel
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
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(model.tab.title) },
                actions = {
                    if (model.tab == Tab.MyApps) {
                        // .fmp12 files have no MIME type of their own.
                        IconButton(onClick = { importer.launch(arrayOf("*/*")) }) { Icon(Icons.Filled.Add, "Import a file") }
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
                Tab.Hosts -> Hosts(model, open)
            }
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
) {
    ListItem(
        headlineContent = { Text(entry.name) },
        supportingContent = { Text(entry.subtitle) },
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

@Composable
private fun Hosts(model: GoViewModel, open: (Entry) -> Unit) {
    val form = model.hostForm
    var certificate by remember { mutableStateOf(form.certificate.isNotEmpty()) }
    Column(Modifier.padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedTextField(
            form.address,
            { model.editHost(address = it) },
            label = { Text("Host name or IP address") },
            singleLine = true,
            keyboardOptions = KeyboardOptions(autoCorrectEnabled = false, keyboardType = KeyboardType.Uri),
            modifier = Modifier.fillMaxWidth(),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(
                form.port,
                { model.editHost(port = it) },
                label = { Text("Port") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.weight(1f),
            )
            OutlinedTextField(
                form.file,
                { model.hostForm = form.copy(file = it) },
                label = { Text("File name (optional)") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
                modifier = Modifier.weight(2f),
            )
        }
        if (certificate) {
            OutlinedTextField(
                form.certificate,
                { model.hostForm = form.copy(certificate = it) },
                label = { Text("Certificate authority (PEM)") },
                minLines = 2,
                maxLines = 5,
                keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
                modifier = Modifier.fillMaxWidth(),
            )
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Button(onClick = model::browseHost, enabled = !model.working) {
                Text(if (form.file.isBlank()) "Browse Host" else "Open File")
            }
            if (!certificate) {
                TextButton(onClick = { certificate = true }) { Text("Private certificate authority…") }
            }
        }
    }
    Files(model, model.hosted, "Browse a host to see the files it shares.", open)
}
