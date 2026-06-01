plugins {
    id("com.android.application")
    kotlin("android")
}

android {
    namespace = "io.github.sriharsha_y.pqcsample"
    compileSdk = 35

    defaultConfig {
        applicationId = "io.github.sriharsha_y.pqcsample"
        // Matches the library floor: rustls-platform-verifier needs API >= 24
        // for full revocation checking.
        minSdk = 24
        targetSdk = 35
        versionCode = 1
        versionName = "1.0"
    }

    // Consume the LOCALLY-BUILT artifacts straight from the repo, instead of
    // the published Maven AAR. `rootDir` is examples/NativeAndroid, so the
    // repo root is two levels up. Run `make android` at the repo root first
    // to populate these (see README).
    //
    // This reconstructs exactly what the published AAR bundles:
    //   - target/jniLibs/*           the native .so files (libpqc_client.so +
    //                                the rustls-platform-verifier .so) per ABI
    //   - generated/kotlin/*         the UniFFI-generated bindings (pqc.kt)
    //   - android/src/main/kotlin/*  the hand-written PqcAndroidInit JNI shim
    sourceSets["main"].apply {
        jniLibs.srcDir(rootDir.resolve("../../target/jniLibs"))
        java.srcDir(rootDir.resolve("../../generated/kotlin"))
        java.srcDir(rootDir.resolve("../../android/src/main/kotlin"))
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }

    buildTypes {
        // Keep the sample simple: no R8 so we don't need the keep rules the
        // published AAR ships (see docs/android.md §7 for production ProGuard).
        release {
            isMinifyEnabled = false
        }
    }
}

dependencies {
    // JNA powers the UniFFI bindings' JNI bridge. `@aar` is mandatory — the
    // plain jar form of JNA is incompatible with Android.
    implementation("net.java.dev.jna:jna:5.14.0@aar")

    // The generated bindings expose `suspend` functions; -android brings
    // Dispatchers.Main for the UI thread.
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.8.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")

    // OkHttp powers Android's de-facto HTTP stack (it's also what backs
    // java.net.HttpURLConnection on the platform since KitKat). PqcInterceptor
    // adapts the Rust PqcHttpClient to OkHttp; this sample demonstrates the
    // realistic integration. The published AAR declares OkHttp as
    // compileOnly, so consumers using PqcInterceptor must add it themselves.
    implementation("com.squareup.okhttp3:okhttp:4.12.0")

    // rustls-platform-verifier's Kotlin glue (org.rustls.platformverifier.*),
    // extracted into android/libs/ by scripts/build-android.sh. Without it the
    // first TLS handshake throws NoClassDefFoundError: CertificateVerifier.
    implementation(files(rootDir.resolve("../../android/libs/rustls-platform-verifier-0.1.1.jar")))
}
