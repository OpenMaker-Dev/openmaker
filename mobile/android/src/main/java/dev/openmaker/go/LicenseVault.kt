package dev.openmaker.go

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The FileMaker client license saved in Settings, for hosts that require
 * one. It is stored encrypted under an AES key that never leaves the
 * Android Keystore: stored bytes carried to another device (a device to
 * device transfer ignores allowBackup) cannot be read there, and are
 * dropped as if no license were saved.
 */
class LicenseVault(context: Context) {
    private val prefs = context.getSharedPreferences("license", Context.MODE_PRIVATE)

    val saved: Boolean
        @Synchronized get() = prefs.contains(SEALED)

    /** The saved license, or empty when none is saved or it can no longer be read. */
    @Synchronized
    fun load(): String {
        val stored = prefs.getString(SEALED, null) ?: return ""
        return runCatching {
            val sealed = Base64.decode(stored, Base64.NO_WRAP)
            require(sealed.size > IV_BYTES)
            val key = keyStore().getKey(ALIAS, null) as? SecretKey ?: error("The license key is gone.")
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BITS, sealed, 0, IV_BYTES))
            String(cipher.doFinal(sealed, IV_BYTES, sealed.size - IV_BYTES), Charsets.UTF_8)
        }.getOrElse {
            // Unreadable here: it has to be entered again.
            remove()
            ""
        }
    }

    /** Save [license] in place of any saved before. */
    @Synchronized
    fun save(license: String) {
        val sealed =
            runCatching {
                val cipher = Cipher.getInstance(TRANSFORMATION)
                cipher.init(Cipher.ENCRYPT_MODE, key())
                check(cipher.iv.size == IV_BYTES)
                cipher.iv + cipher.doFinal(license.toByteArray(Charsets.UTF_8))
            }.getOrElse { throw IllegalStateException("The license could not be saved on this device.") }
        check(prefs.edit().putString(SEALED, Base64.encodeToString(sealed, Base64.NO_WRAP)).commit()) {
            "The license could not be saved on this device."
        }
    }

    @Synchronized
    fun remove() {
        prefs.edit().remove(SEALED).commit()
        runCatching { keyStore().deleteEntry(ALIAS) }
    }

    private fun keyStore() = KeyStore.getInstance(KEYSTORE).apply { load(null) }

    private fun key(): SecretKey {
        (keyStore().getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build()
        )
        return generator.generateKey()
    }

    private companion object {
        const val KEYSTORE = "AndroidKeyStore"
        const val ALIAS = "filemaker-client-license"
        const val SEALED = "sealed"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val IV_BYTES = 12
        const val TAG_BITS = 128
    }
}
