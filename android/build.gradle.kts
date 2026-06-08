import com.vanniktech.maven.publish.SonatypeHost

plugins {
    id("com.android.library") version "8.5.0"
    kotlin("android") version "1.9.20"
    id("com.vanniktech.maven.publish") version "0.30.0"
}

// release-please rewrites this on each release; keep the
// `version = "X.Y.Z"` format so its updater regex matches.
version = "0.8.1" // x-release-please-version
group = "io.github.sriharsha-y"

android {
    // Namespace must be a valid Java package — sriharsha-y → sriharsha_y.
    // Independent of the Maven groupId (which can keep the dash).
    namespace = "io.github.sriharsha_y.pqc"
    compileSdk = 35

    defaultConfig {
        // The AAR's own minSdk is 22, but we hold at 24 — the floor where
        // rustls-platform-verifier revocation checking is fully supported.
        // See the top-level README for rationale.
        minSdk = 24

        // Ship R8/ProGuard keep rules inside the AAR so consumers with
        // minifyEnabled=true don't have to discover them (JNA, the UniFFI
        // bindings, and the rustls-platform-verifier glue are all R8 strip /
        // rename targets, and JNA's java.awt refs otherwise fail the build).
        consumerProguardFiles("consumer-rules.pro")
    }

    sourceSets["main"].apply {
        // Inputs produced by scripts/build-android.sh, resolved relative to
        // the gradle rootProject (android/).
        jniLibs.srcDir(rootProject.file("../target/jniLibs"))
        java.srcDir(rootProject.file("../generated/kotlin"))
        // Hand-written glue (PqcAndroidInit, PqcConfigDefaults,
        // PqcInterceptor) sits alongside the generated bindings so
        // consumers get a single, fully-wired AAR. PqcInterceptor pulls
        // OkHttp in via the `compileOnly` dep below — most Android
        // consumers use OkHttp directly or transitively via Retrofit /
        // Ktor / RN, so co-locating is fine.
        java.srcDir("src/main/kotlin")
        manifest.srcFile("AndroidManifest.xml")
    }

    buildFeatures {
        // Binary-binding lib — no BuildConfig / Resources.
        buildConfig = false
    }

    // Do NOT declare `publishing { singleVariant("release") { ... } }` here.
    // Vanniktech's maven-publish plugin (configured below) already calls
    // singleVariant("release") internally; declaring it again triggers:
    //   "Using singleVariant publishing DSL multiple times ... is not allowed."
}

kotlin {
    // Matches the JDK installed in CI (temurin 17).
    jvmToolchain(17)
}

dependencies {
    // JNA powers the UniFFI bindings' JNI bridge. `@aar` is required because
    // the jar form JNA also publishes is incompatible with Android.
    api("net.java.dev.jna:jna:5.14.0@aar")

    // Generated Kotlin bindings expose `suspend` fns.
    api("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.8.1")

    // PqcInterceptor compiles against OkHttp's API; consumers bring their
    // own OkHttp at runtime (almost all Android apps do via Retrofit / Ktor
    // / RN). Pinned to the supported floor so the AAR doesn't accidentally
    // use APIs that would break older 4.x consumers.
    compileOnly("com.squareup.okhttp3:okhttp:4.0.0")

    // Bundle the rustls-platform-verifier Kotlin glue
    // (org.rustls.platformverifier.*) into our published AAR. Upstream ships
    // those classes only as a Cargo-vendored AAR, not on any Maven repo, so
    // without bundling, Maven Central consumers hit an
    // `org.rustls.platformverifier.CertificateVerifier` NoClassDefFoundError
    // at handshake time. scripts/build-android.sh extracts the classes.jar
    // into android/libs/; `api` both compiles against it and embeds it under
    // the AAR's libs/ so consumers pick it up transitively.
    api(fileTree("libs") { include("*.jar") })
}

// Fail-fast guard: an empty android/libs/ silently produces an AAR missing
// the rustls-platform-verifier classes, reproducing the very
// NoClassDefFoundError bundling is meant to prevent. android/libs/ is
// .gitignore'd and only populated by scripts/build-android.sh, so building
// without first running the script needs a loud error, not a broken AAR.
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
