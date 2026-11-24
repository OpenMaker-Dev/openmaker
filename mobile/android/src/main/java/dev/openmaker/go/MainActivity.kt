package dev.openmaker.go

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import dev.openmaker.go.ui.GoApp
import dev.openmaker.go.ui.GoTheme

class MainActivity : ComponentActivity() {
    private val model: GoViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        lifecycle.addObserver(
            LifecycleEventObserver { _, event ->
                when (event) {
                    // Leaving the foreground saves the record (the engine's Active).
                    Lifecycle.Event.ON_START -> model.setActive(true)
                    Lifecycle.Event.ON_STOP -> model.setActive(false)
                    // Files may have arrived in the documents folder meanwhile.
                    Lifecycle.Event.ON_RESUME -> model.refresh()
                    else -> Unit
                }
            }
        )
        setContent { GoTheme { GoApp(model) } }
    }
}
