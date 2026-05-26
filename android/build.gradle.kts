import com.vanniktech.maven.publish.SonatypeHost

plugins {
    id("com.android.library") version "8.5.0"
    kotlin("android") version "1.9.20"
    id("com.vanniktech.maven.publish") version "0.30.0"
}

// release-please-config.json's extra-files rewrites this version literal
// on each release (via the x-release-please-version marker). Keep the
// format `version = "X.Y.Z"` so the generic updater regex matches.
version = "0.2.0" // x-release-please-version
group = "io.github.sriharsha-y"

android {
    // Namespace must be a valid Java package — sriharsha-y → sriharsha_y.
    // Independent of the Maven groupId (which can keep the dash).
    namespace = "io.github.sriharsha_y.pqc"
    compileSdk = 35

    defaultConfig {
        // Lowest Android API where rustls-platform-verifier's revocation
        // checking is fully supported. The AAR's own minSdk is 22 but we
        // hold the floor at 24 — see the top-level README for rationale.
        minSdk = 24
    }

    sourceSets["main"].apply {
        // Inputs produced by scripts/build-android.sh:
        //   target/jniLibs/{arm64-v8a,armeabi-v7a,x86_64}/libpqc_client.so
        //   generated/kotlin/uniffi/pqc/pqc.kt
        // Resolved relative to the gradle rootProject (android/).
        jniLibs.srcDir(rootProject.file("../target/jniLibs"))
        java.srcDir(rootProject.file("../generated/kotlin"))
        manifest.srcFile("AndroidManifest.xml")
    }

    buildFeatures {
        // No BuildConfig / Resources / etc — this is a binary-binding lib.
        buildConfig = false
    }

    // NOTE: do NOT declare `publishing { singleVariant("release") { ... } }`
    // here. Vanniktech's maven-publish plugin (configured below with
    // publishToMavenCentral(...) + AndroidSingleVariantLibrary, which is
    // the default for android-library projects) internally calls
    // singleVariant("release") { withSourcesJar(); withJavadocJar() }
    // itself. Declaring it manually as well triggers a Gradle error:
    //   "Using singleVariant publishing DSL multiple times to publish
    //    variant 'release' to component 'release' is not allowed."
}

kotlin {
    // Matches the JDK installed in CI (setup-java@v4 → temurin 17).
    jvmToolchain(17)
}

dependencies {
    // JNA powers the UniFFI Kotlin bindings' JNI bridge. `@aar` is required
    // because JNA ships an AAR variant on Maven Central; without it Gradle
    // would resolve the jar form which is incompatible with Android.
    api("net.java.dev.jna:jna:5.14.0@aar")

    // Async surface — the generated Kotlin bindings expose `suspend` fns.
    api("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.8.1")
}

mavenPublishing {
    // Sonatype Central Portal is the post-2024 endpoint that replaces OSSRH.
    publishToMavenCentral(SonatypeHost.CENTRAL_PORTAL)
    signAllPublications()

    coordinates(group.toString(), "pqc-mobile-client", version.toString())

    pom {
        name.set("pqc-mobile-client")
        description.set(
            "Post-Quantum TLS HTTPS client for iOS and Android (native + " +
                "React Native) via UniFFI. Negotiates X25519MLKEM768 hybrid PQ " +
                "with rustls + rustls-post-quantum + aws-lc-rs."
        )
        inceptionYear.set("2026")
        url.set("https://github.com/sriharsha-y/pqc-mobile-client")

        licenses {
            license {
                name.set("Apache License 2.0")
                url.set("https://www.apache.org/licenses/LICENSE-2.0.txt")
                distribution.set("repo")
            }
        }

        developers {
            developer {
                id.set("sriharsha-y")
                name.set("Harsha Yarabarla")
                email.set("harsha.yarabarla@gmail.com")
                url.set("https://github.com/sriharsha-y")
            }
        }

        scm {
            url.set("https://github.com/sriharsha-y/pqc-mobile-client")
            connection.set("scm:git:https://github.com/sriharsha-y/pqc-mobile-client.git")
            developerConnection.set("scm:git:ssh://git@github.com/sriharsha-y/pqc-mobile-client.git")
        }

        issueManagement {
            system.set("GitHub Issues")
            url.set("https://github.com/sriharsha-y/pqc-mobile-client/issues")
        }
    }
}
