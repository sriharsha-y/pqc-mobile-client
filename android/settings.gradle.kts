pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

// Single-module project: the library IS the root project. The .so files
// and Kotlin bindings live at ../target/jniLibs and ../generated/kotlin
// (produced by scripts/build-android.sh on a sibling cargo invocation),
// and the build.gradle.kts in this directory picks them up via srcDirs.
rootProject.name = "pqc-mobile-client"
