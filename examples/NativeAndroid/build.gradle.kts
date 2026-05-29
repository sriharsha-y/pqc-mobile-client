// Root build script. Versions match the library's own android module
// (AGP 8.5.0 / Kotlin 1.9.20) so the example tracks what CI builds.
plugins {
    id("com.android.application") version "8.5.0" apply false
    kotlin("android") version "1.9.20" apply false
}
