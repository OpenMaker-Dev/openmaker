package dev.openmaker.go.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
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
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.openmaker.go.engine.Command
import dev.openmaker.go.engine.DocumentMode
import dev.openmaker.go.engine.DocumentState
import dev.openmaker.go.engine.Input
import dev.openmaker.go.engine.Key
import dev.openmaker.go.engine.Menu

private fun List<Menu>.titled(title: String) = firstOrNull { it.title == title }

private fun command(send: (Input) -> Unit, command: Command) = send(Input.Run(command))

/** Files, the window's title and its menus; in Find mode, FileMaker Go's Find bar. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DocumentTopBar(state: DocumentState?, menus: List<Menu>, send: (Input) -> Unit) {
    val finding = state?.mode == DocumentMode.FIND
    TopAppBar(
        navigationIcon = {
            if (finding) {
                TextButton(onClick = { command(send, Command.CancelFind) }, enabled = state?.canCancelFind == true) {
                    Text("Cancel")
                }
            } else {
                // Close names the file shown: one a script brought forward
                // since is not closed.
                TextButton(
                    onClick = { state?.let { command(send, Command.Close(it.file)) } },
                    enabled = state?.canClose == true,
                ) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, null)
                    Text("Files", Modifier.padding(start = 4.dp))
                }
            }
        },
        title = {
            Text(if (finding) "Find" else state?.title.orEmpty(), maxLines = 1, overflow = TextOverflow.Ellipsis)
        },
        actions = {
            if (state?.busy == true) {
                CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                Spacer(Modifier.width(8.dp))
            }
            val choose = { id: String -> send(Input.Menu(id)) }
            if (finding) {
                FilterChip(
                    selected = state?.omitting == true,
                    onClick = { command(send, Command.ToggleOmit) },
                    enabled = state?.canToggleOmit == true,
                    label = { Text("Omit") },
                )
                FileMakerMenu(menus.titled("Requests"), choose) { enabled, open ->
                    IconButton(onClick = open, enabled = enabled) { Icon(Icons.Filled.MoreVert, "Requests") }
                }
                Button(
                    onClick = { command(send, Command.PerformFind) },
                    enabled = state?.canPerformFind == true,
                    modifier = Modifier.padding(end = 8.dp),
                ) {
                    Text("Search")
                }
            } else {
                FindMenu(state, send)
                FileMakerMenu(menus.titled("Scripts"), choose) { enabled, open ->
                    IconButton(onClick = open, enabled = enabled) { Icon(Icons.Filled.PlayArrow, "Scripts") }
                }
                FileMakerMenu(menus.titled("View"), choose) { enabled, open ->
                    IconButton(onClick = open, enabled = enabled) { Icon(Icons.AutoMirrored.Filled.List, "View") }
                }
                FileMakerMenu(menus.titled("Records"), choose) { enabled, open ->
                    IconButton(onClick = open, enabled = enabled) { Icon(Icons.Filled.MoreVert, "Records") }
                }
            }
        },
    )
}

/** FileMaker Go's Find icon: Quick Find and a new find. */
@Composable
private fun FindMenu(state: DocumentState?, send: (Input) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    var quickFind by remember { mutableStateOf(false) }
    Box {
        IconButton(
            onClick = { expanded = true },
            enabled = state?.canFind == true || state?.canQuickFind == true,
        ) {
            Icon(Icons.Filled.Search, "Find")
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(
                text = { Text("Quick Find") },
                enabled = state?.canQuickFind == true,
                onClick = {
                    expanded = false
                    quickFind = true
                },
            )
            DropdownMenuItem(
                text = { Text("Create New Find") },
                enabled = state?.canFind == true,
                onClick = {
                    expanded = false
                    command(send, Command.Find)
                },
            )
        }
    }
    if (quickFind) {
        AskDialog(
            title = "Quick Find",
            label = "Search",
            confirm = "Search",
            onConfirm = { words ->
                quickFind = false
                command(send, Command.QuickFind(words))
            },
            onDismiss = { quickFind = false },
        )
    }
}

/** The layout menu, and the arrows and count of the records or requests. */
@Composable
fun RecordBar(state: DocumentState?, menus: List<Menu>, send: (Input) -> Unit) {
    var goTo by remember { mutableStateOf(false) }
    Surface(tonalElevation = 3.dp) {
        Row(
            Modifier.fillMaxWidth().height(56.dp).padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            FileMakerMenu(menus.titled("Layouts"), { send(Input.Menu(it)) }) { enabled, open ->
                TextButton(onClick = open, enabled = enabled) {
                    Text(state?.layout?.ifEmpty { null } ?: "Layout", maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Icon(Icons.Filled.ArrowDropDown, null)
                }
            }
            Spacer(Modifier.weight(1f))
            if (state != null && state.mode != DocumentMode.OTHER) {
                val finding = state.mode == DocumentMode.FIND
                IconButton(onClick = { command(send, Command.Previous) }, enabled = state.canPrevious) {
                    Icon(Icons.AutoMirrored.Filled.KeyboardArrowLeft, if (finding) "Previous request" else "Previous record")
                }
                TextButton(onClick = { goTo = true }, enabled = !finding && state.canGoToRecord) {
                    Text(position(state))
                }
                IconButton(onClick = { command(send, Command.Next) }, enabled = state.canNext) {
                    Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, if (finding) "Next request" else "Next record")
                }
            }
        }
    }
    if (goTo) {
        AskDialog(
            title = "Go to Record",
            label = "Record number",
            confirm = "Go",
            keyboardType = KeyboardType.Number,
            onConfirm = { text ->
                goTo = false
                text.trim().toULongOrNull()?.let { command(send, Command.GoToRecord(it)) }
            },
            onDismiss = { goTo = false },
        )
    }
}

private fun position(state: DocumentState): String =
    when {
        state.mode == DocumentMode.FIND -> "${if (state.omitting) "Omit" else "Find"} ${state.position + 1u}/${state.count}"
        state.count == 0uL -> "No records"
        else -> "${state.position + 1u} of ${state.count}"
    }

/** FileMaker Go's editing toolbar, above the keyboard. */
@Composable
fun EditingBar(state: DocumentState?, send: (Input) -> Unit) {
    Surface(tonalElevation = 3.dp) {
        Row(
            Modifier.fillMaxWidth().height(48.dp).padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(onClick = { send(Input.Press(Key.TAB, true, false)) }) { Text("Previous") }
            TextButton(onClick = { send(Input.Press(Key.TAB, false, false)) }) { Text("Next") }
            Spacer(Modifier.weight(1f))
            TextButton(onClick = { command(send, Command.Done) }, enabled = state?.canDone == true) { Text("Done") }
        }
    }
}
