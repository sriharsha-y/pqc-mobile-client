import com.vanniktech.maven.publish.SonatypeHost

plugins {
    id("com.android.library") version "8.5.0"
    kotlin("android") version "1.9.20"
    id("com.vanniktech.maven.publish") version "0.30.0"
}

// release-please-config.json's extra-files rewrites this version literal
// on each release (via the x-release-please-version marker). Keep the
// format `version = "X.Y.Z"` so the generic updater regex matches.
version = "0.3.0" // x-release-please-version
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
        // Hand-written Kotlin glue under android/src/main/kotlin/ — adds
        // uniffi.pqc.android.PqcAndroidInit (the JNI bridge that hands
        // the Application Context to rustls-platform-verifier). Sits
        // alongside the generated bindings so consumers receive a single
        // AAR with everything wired up.
        java.srcDir("src/main/kotlin")
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

    // Bundle the rustls-platform-verifier Kotlin glue
    // (org.rustls.platformverifier.*) directly into our published AAR.
    // The upstream crate ships those classes as a vendored Maven AAR
    // inside ~/.cargo/registry, not on any public Maven repo. Without
    // bundling, consumers downloading our AAR from Maven Central would
    // get an `org.rustls.platformverifier.CertificateVerifier` NoClassDefFoundError
    // at handshake time. scripts/build-android.sh extracts the upstream
    // AAR's classes.jar into android/libs/; declaring it as `api`
    // (a) puts the symbols on our compile classpath and
    // (b) tells AGP to embed the jar under the AAR's libs/ entry, so
    // consumers transitively pick it up with zero extra configuration.
    api(fileTree("libs") { include("*.jar") })
}

// Fail-fast guard: an empty android/libs/ would silently produce an AAR
// missing the rustls-platform-verifier classes — reproducing the exact
// NoClassDefFoundError the bundling is supposed to fix. android/libs/ is
// .gitignore'd and only populated by scripts/build-android.sh, so a
// maintainer running `./gradlew assembleRelease` (or publishToMavenLocal)
// without first invoking the script needs a loud error, not a broken AAR.
tasks.named("preBuild").configure {
    doFirst {
        val jars = fileTree("libs").matching { include("*.jar") }.files
        require(jars.isNotEmpty()) {
            """
            android/libs/ contains no jars. The published AAR would be missing
            the rustls-platform-verifier Kotlin glue and fail at handshake time
            with NoClassDefFoundError: org.rustls.platformverifier.CertificateVerifier.

            Run scripts/build-android.sh from the repo root first — it extracts
            the vendored classes.jar out of the rustls-platform-verifier-android
            crate and writes it to android/libs/.
            """.trimIndent()
        }
    }
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
