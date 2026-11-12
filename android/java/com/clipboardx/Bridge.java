package com.clipboardx;

import android.app.Application;
import android.content.ClipData;
import android.content.ClipDescription;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.pm.PackageManager;
import android.hardware.biometrics.BiometricManager;
import android.hardware.biometrics.BiometricPrompt;
import android.os.Build;
import android.os.CancellationSignal;
import android.os.Handler;
import android.os.Looper;
import android.os.PersistableBundle;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyPermanentlyInvalidatedException;
import android.security.keystore.KeyProperties;

import java.security.KeyStore;
import java.util.Arrays;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Executor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;

import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/**
 * What ClipboardX's Rust code needs from Android that only Java reaches: the
 * Keystore, the biometric prompt and the clipboard's sensitive flag. Loaded by
 * Rust at run time (InMemoryDexClassLoader); every method is static.
 *
 * Errors are exceptions whose message Rust reads: "canceled" (the user closed
 * the prompt), "invalidated" (the biometric key went with a change of the
 * enrolled fingerprints or faces), anything else a failure.
 */
public final class Bridge {
    private static final String KEYSTORE = "AndroidKeyStore";
    private static final String TRANSFORMATION = "AES/GCM/NoPadding";
    private static final int IV_BYTES = 12;
    private static final int TAG_BITS = 128;
    private static final long PROMPT_TIMEOUT_SECONDS = 120;
    /** ClipDescription.EXTRA_IS_SENSITIVE (API 33); older systems read it too. */
    private static final String EXTRA_IS_SENSITIVE = "android.content.extra.IS_SENSITIVE";
    private static final String CANCELED = "canceled";
    private static final String INVALIDATED = "invalidated";

    private static final Handler MAIN = new Handler(Looper.getMainLooper());

    private Bridge() {}

    /** The application, through the framework's own record of it. */
    private static Context context() throws Exception {
        Application app = (Application) Class.forName("android.app.ActivityThread")
                .getMethod("currentApplication")
                .invoke(null);
        if (app == null) {
            throw new IllegalStateException("no application yet");
        }
        return app;
    }

    /** The app's private files directory. */
    public static String filesDir() throws Exception {
        return context().getFilesDir().getAbsolutePath();
    }

    /**
     * Whether a strong biometric (fingerprint, face) is enrolled and usable,
     * and the app may use it (USE_BIOMETRIC).
     */
    public static boolean biometricAvailable() throws Exception {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
            return false;
        }
        Context context = context();
        // NSH's manifest may not ask for it: no biometrics then, quietly.
        if (context.checkSelfPermission("android.permission.USE_BIOMETRIC")
                != PackageManager.PERMISSION_GRANTED) {
            return false;
        }
        BiometricManager manager = context.getSystemService(BiometricManager.class);
        return manager != null
                && manager.canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_STRONG)
                        == BiometricManager.BIOMETRIC_SUCCESS;
    }

    /**
     * Encrypts {@code plain} with the Keystore key {@code alias} (made when
     * missing); with {@code biometric} the key needs the user's biometric
     * for each use and {@code title} heads the prompt. Returns IV then
     * ciphertext. Blocking: never on the main thread.
     */
    public static byte[] seal(String alias, byte[] plain, boolean biometric, String title)
            throws Exception {
        Cipher cipher = Cipher.getInstance(TRANSFORMATION);
        try {
            cipher.init(Cipher.ENCRYPT_MODE, key(alias, biometric));
        } catch (KeyPermanentlyInvalidatedException e) {
            // New fingerprints since: a fresh key for the new copy.
            deleteKey(alias);
            cipher.init(Cipher.ENCRYPT_MODE, key(alias, biometric));
        }
        if (biometric) {
            cipher = authenticate(cipher, title);
        }
        byte[] iv = cipher.getIV();
        byte[] sealed = cipher.doFinal(plain);
        byte[] out = Arrays.copyOf(iv, iv.length + sealed.length);
        System.arraycopy(sealed, 0, out, iv.length, sealed.length);
        return out;
    }

    /** Decrypts what {@link #seal} returned with the same key. Blocking. */
    public static byte[] open(String alias, byte[] sealed, boolean biometric, String title)
            throws Exception {
        if (sealed.length <= IV_BYTES) {
            throw new IllegalArgumentException("sealed data too short");
        }
        SecretKey key = existingKey(alias);
        if (key == null) {
            throw new IllegalStateException(INVALIDATED);
        }
        Cipher cipher = Cipher.getInstance(TRANSFORMATION);
        try {
            cipher.init(Cipher.DECRYPT_MODE, key,
                    new GCMParameterSpec(TAG_BITS, Arrays.copyOfRange(sealed, 0, IV_BYTES)));
        } catch (KeyPermanentlyInvalidatedException e) {
            deleteKey(alias);
            throw new IllegalStateException(INVALIDATED);
        }
        if (biometric) {
            cipher = authenticate(cipher, title);
        }
        return cipher.doFinal(sealed, IV_BYTES, sealed.length - IV_BYTES);
    }

    public static void deleteKey(String alias) throws Exception {
        KeyStore store = KeyStore.getInstance(KEYSTORE);
        store.load(null);
        if (store.containsAlias(alias)) {
            store.deleteEntry(alias);
        }
    }

    private static SecretKey existingKey(String alias) throws Exception {
        KeyStore store = KeyStore.getInstance(KEYSTORE);
        store.load(null);
        return (SecretKey) store.getKey(alias, null);
    }

    private static SecretKey key(String alias, boolean biometric) throws Exception {
        SecretKey existing = existingKey(alias);
        if (existing != null) {
            return existing;
        }
        KeyGenParameterSpec.Builder spec = new KeyGenParameterSpec.Builder(alias,
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256);
        if (biometric) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
                throw new IllegalStateException("biometric keys need Android 11");
            }
            spec.setUserAuthenticationRequired(true)
                    .setInvalidatedByBiometricEnrollment(true)
                    .setUserAuthenticationParameters(0,
                            KeyProperties.AUTH_BIOMETRIC_STRONG);
        }
        KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE);
        generator.init(spec.build());
        return generator.generateKey();
    }

    /** Shows the biometric prompt for {@code cipher}; the cipher it unlocks. */
    private static Cipher authenticate(Cipher cipher, String title) throws Exception {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            throw new IllegalStateException("biometric prompt waited for on the main thread");
        }
        Context context = context();
        Executor main = MAIN::post;
        CountDownLatch done = new CountDownLatch(1);
        AtomicReference<Cipher> unlocked = new AtomicReference<>();
        AtomicReference<String> error = new AtomicReference<>();
        CancellationSignal cancel = new CancellationSignal();
        BiometricPrompt.AuthenticationCallback callback = new BiometricPrompt.AuthenticationCallback() {
            @Override
            public void onAuthenticationSucceeded(BiometricPrompt.AuthenticationResult result) {
                unlocked.set(result.getCryptoObject().getCipher());
                done.countDown();
            }

            @Override
            public void onAuthenticationError(int code, CharSequence message) {
                boolean canceled = code == BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED
                        || code == BiometricPrompt.BIOMETRIC_ERROR_CANCELED
                        || code == BiometricPrompt.BIOMETRIC_ERROR_TIMEOUT;
                error.set(canceled ? CANCELED : String.valueOf(message));
                done.countDown();
            }
        };
        MAIN.post(() -> {
            try {
                BiometricPrompt prompt = new BiometricPrompt.Builder(context)
                        .setTitle(title)
                        .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG)
                        .setNegativeButton("使用主密码", main, (dialog, which) -> {
                            error.set(CANCELED);
                            done.countDown();
                        })
                        .build();
                prompt.authenticate(new BiometricPrompt.CryptoObject(cipher), cancel, main, callback);
            } catch (RuntimeException e) {
                error.set(String.valueOf(e.getMessage()));
                done.countDown();
            }
        });
        if (!done.await(PROMPT_TIMEOUT_SECONDS, TimeUnit.SECONDS)) {
            cancel.cancel();
            throw new IllegalStateException(CANCELED);
        }
        if (unlocked.get() == null) {
            throw new IllegalStateException(error.get() == null ? CANCELED : error.get());
        }
        return unlocked.get();
    }

    /** The last sensitive copy: its id (from Rust) and its clip's timestamp. Main thread. */
    private static long copyId;
    private static long copyStamp;

    /**
     * Copies {@code text} marked sensitive, so the system's previews and
     * keyboards leave it out, as copy {@code id}. Posted to the main thread:
     * returns at once.
     */
    public static void copySensitive(String text, long id) throws Exception {
        ClipboardManager clipboard = clipboard();
        MAIN.post(() -> {
            ClipData clip = ClipData.newPlainText("", text);
            PersistableBundle extras = new PersistableBundle();
            extras.putBoolean(EXTRA_IS_SENSITIVE, true);
            clip.getDescription().setExtras(extras);
            clipboard.setPrimaryClip(clip);
            ClipDescription now = clipboard.getPrimaryClipDescription();
            copyId = id;
            copyStamp = now == null ? 0 : now.getTimestamp();
        });
    }

    /**
     * Clears the clipboard if copy {@code id} is the last sensitive copy and
     * nothing was copied after it. In the background the clipboard cannot be
     * read: it is cleared then, as a password manager would rather lose a
     * copy than leave a password there.
     */
    public static void clearSensitive(long id) throws Exception {
        ClipboardManager clipboard = clipboard();
        MAIN.post(() -> {
            if (id != copyId) {
                return;
            }
            ClipDescription now = clipboard.getPrimaryClipDescription();
            if (now != null && copyStamp != 0 && now.getTimestamp() != copyStamp) {
                return;
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                clipboard.clearPrimaryClip();
            } else {
                clipboard.setPrimaryClip(ClipData.newPlainText("", ""));
            }
        });
    }

    private static ClipboardManager clipboard() throws Exception {
        ClipboardManager clipboard = context().getSystemService(ClipboardManager.class);
        if (clipboard == null) {
            throw new IllegalStateException("no clipboard service");
        }
        return clipboard;
    }
}
