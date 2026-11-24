package dev.openmaker.go.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Check
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.text.font.FontWeight
import dev.openmaker.go.engine.Menu
import dev.openmaker.go.engine.MenuRow
import dev.openmaker.go.engine.MenuRowKind

/** A row of one of FileMaker's menus, with the rows of its submenu. */
private class MenuNode(val row: MenuRow, val children: List<MenuNode>)

/** The engine lists a menu's rows flat: a submenu's rows follow it, deeper. */
private fun tree(rows: List<MenuRow>): List<MenuNode> {
    var at = 0
    fun level(depth: UInt): List<MenuNode> {
        val nodes = mutableListOf<MenuNode>()
        while (at < rows.size && rows[at].depth == depth) {
            val row = rows[at++]
            val children = if (row.kind == MenuRowKind.SUBMENU) level(depth + 1u) else emptyList()
            nodes += MenuNode(row, children)
        }
        // Rows deeper than a submenu's own (malformed) are skipped, not lost in a loop.
        while (at < rows.size && rows[at].depth > depth) at++
        return nodes
    }
    return level(0u)
}

/**
 * A button that opens one of FileMaker's menus. A submenu opens in place,
 * with a row back to the menu it is in, as phone menus do.
 */
@Composable
fun FileMakerMenu(
    menu: Menu?,
    onChoose: (id: String) -> Unit,
    button: @Composable (enabled: Boolean, open: () -> Unit) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    var path by remember { mutableStateOf(emptyList<MenuNode>()) }
    val nodes = remember(menu) { tree(menu?.rows.orEmpty()) }
    Box {
        button(nodes.isNotEmpty()) {
            path = emptyList()
            expanded = true
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            val parent = path.lastOrNull()
            if (parent != null) {
                DropdownMenuItem(
                    text = { Text(parent.row.label, fontWeight = FontWeight.SemiBold) },
                    leadingIcon = { Icon(Icons.AutoMirrored.Filled.KeyboardArrowLeft, "Back") },
                    onClick = { path = path.dropLast(1) },
                )
                HorizontalDivider()
            }
            for (node in parent?.children ?: nodes) {
                val row = node.row
                when (row.kind) {
                    MenuRowKind.SEPARATOR -> HorizontalDivider()
                    MenuRowKind.SUBMENU ->
                        DropdownMenuItem(
                            text = { Text(row.label) },
                            enabled = row.enabled && node.children.isNotEmpty(),
                            trailingIcon = { Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null) },
                            onClick = { path = path + node },
                        )
                    MenuRowKind.COMMAND ->
                        DropdownMenuItem(
                            text = { Text(row.label) },
                            enabled = row.enabled,
                            leadingIcon = if (row.checked) ({ Icon(Icons.Filled.Check, "Checked") }) else null,
                            onClick = {
                                expanded = false
                                onChoose(row.id)
                            },
                        )
                }
            }
        }
    }
}
