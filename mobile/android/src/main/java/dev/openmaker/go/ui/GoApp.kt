package dev.openmaker.go.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import dev.openmaker.go.GoViewModel
import dev.openmaker.go.SignInRequest
import dev.openmaker.go.engine.Viewport

/** The Launch Center, or the open file in its place. */
@Composable
fun GoApp(model: GoViewModel) {
    val session = model.session
    if (session != null) {
        DocumentScreen(session, onClosed = { failure -> model.documentClosed(session, failure) })
    } else {
        LaunchCenterScreen(model)
    }
    model.signIn?.let { request ->
        val viewport = screenViewport()
        SignInDialog(
            request,
            model.licenseSaved,
            onOpen = { account, password, license -> model.open(request.entry, account, password, license, viewport) },
            onCancel = { model.signIn = null },
        )
    }
    if (model.settings) {
        SettingsDialog(
            model.licenseSaved,
            model.working,
            onSave = model::saveLicense,
            onRemove = model::removeLicense,
            onDone = { model.settings = false },
        )
    }
    model.message?.let { message -> MessageDialog(message) { model.message = null } }
}

/**
 * The app's settings: the FileMaker client license, as the desktop's
 * FileMaker License dialog keeps it.
 */
@Composable
private fun SettingsDialog(
    saved: Boolean,
    busy: Boolean,
    onSave: (String) -> Unit,
    onRemove: () -> Unit,
    onDone: () -> Unit,
) {
    // A secret stays in memory: saved state can outlive the process.
    var license by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDone,
        title = { Text("Settings") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("FileMaker License", style = MaterialTheme.typography.titleSmall)
                Text(
                    "Optional. FileMaker Server may require a client license; OpenMaker Server does not.",
                    style = MaterialTheme.typography.bodyMedium,
                )
                Text(
                    if (saved) "A license is saved on this device." else "No license is saved.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedTextField(
                    license,
                    { license = it },
                    label = { Text("License key") },
                    supportingText = { Text("Sent to a host when its sign-in's license is left empty.") },
                    singleLine = true,
                    enabled = !busy,
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = KeyboardOptions(autoCorrectEnabled = false, keyboardType = KeyboardType.Password),
                    modifier = Modifier.fillMaxWidth(),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(
                        onClick = {
                            onSave(license)
                            license = ""
                        },
                        enabled = license.isNotBlank() && !busy,
                    ) {
                        Text("Save License")
                    }
                    TextButton(onClick = onRemove, enabled = saved && !busy) { Text("Remove License") }
                }
            }
        },
        confirmButton = { TextButton(onClick = onDone) { Text("Done") } },
    )
}

/** The screen's size, for opening a file: the canvas reports its own once it is laid out. */
@Composable
fun screenViewport(): Viewport {
    val configuration = LocalConfiguration.current
    return Viewport(
        configuration.screenWidthDp.toFloat(),
        configuration.screenHeightDp.toFloat(),
        LocalDensity.current.density,
        INITIAL_TEXTURE_SIDE,
    )
}

/** OpenGL ES 3.0 guarantees 2048; nearly every device takes this. */
private const val INITIAL_TEXTURE_SIDE = 4096u

@Composable
private fun SignInDialog(
    request: SignInRequest,
    licenseSaved: Boolean,
    onOpen: (account: String, password: String, license: String) -> Unit,
    onCancel: () -> Unit,
) {
    var account by rememberSaveable(request.entry) { mutableStateOf("") }
    // Secrets stay in memory: saved state can outlive the process.
    var password by remember(request.entry) { mutableStateOf("") }
    var license by remember(request.entry) { mutableStateOf("") }
    val secret = KeyboardOptions(autoCorrectEnabled = false, keyboardType = KeyboardType.Password)
    AlertDialog(
        onDismissRequest = { if (!request.opening) onCancel() },
        title = { Text(request.entry.name) },
        text = {
            Column {
                Text(request.entry.subtitle, style = MaterialTheme.typography.bodyMedium)
                OutlinedTextField(
                    account,
                    { account = it },
                    label = { Text("Account Name") },
                    singleLine = true,
                    enabled = !request.opening,
                    keyboardOptions =
                        KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    password,
                    { password = it },
                    label = { Text("Password") },
                    singleLine = true,
                    enabled = !request.opening,
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = secret,
                    modifier = Modifier.fillMaxWidth(),
                )
                if (!request.entry.local) {
                    OutlinedTextField(
                        license,
                        { license = it },
                        label = { Text("Client license (optional)") },
                        supportingText =
                            if (licenseSaved) {
                                { Text("Leave empty to use the license saved in Settings.") }
                            } else {
                                null
                            },
                        singleLine = true,
                        enabled = !request.opening,
                        visualTransformation = PasswordVisualTransformation(),
                        keyboardOptions = secret,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                request.error?.let {
                    Spacer(Modifier.height(8.dp))
                    Text(it, color = MaterialTheme.colorScheme.error)
                }
                if (request.opening) {
                    Spacer(Modifier.height(12.dp))
                    LinearProgressIndicator(Modifier.fillMaxWidth())
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { onOpen(account, password, license) }, enabled = !request.opening) { Text("Open") }
        },
        dismissButton = { TextButton(onClick = onCancel, enabled = !request.opening) { Text("Cancel") } },
    )
}

@Composable
fun MessageDialog(message: String, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        text = { Text(message) },
        confirmButton = { TextButton(onClick = onDismiss) { Text("OK") } },
    )
}

/** A dialog asking for one line of text, as Go to Record and Quick Find do. */
@Composable
fun AskDialog(
    title: String,
    label: String,
    confirm: String,
    keyboardType: KeyboardType = KeyboardType.Text,
    onConfirm: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    var text by rememberSaveable { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            OutlinedTextField(
                text,
                { text = it },
                label = { Text(label) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = keyboardType),
                modifier = Modifier.fillMaxWidth(),
            )
        },
        confirmButton = {
            TextButton(onClick = { onConfirm(text) }, enabled = text.isNotBlank()) { Text(confirm) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}
