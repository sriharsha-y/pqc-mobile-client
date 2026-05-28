# Add project specific ProGuard rules here.
# By default, the flags in this file are appended to flags specified
# in /usr/local/Cellar/android-sdk/24.3.3/tools/proguard/proguard-android.txt
# You can edit the include path and order by changing the proguardFiles
# directive in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# Add any project specific keep options here:

# pqc-mobile-client: keep generated UniFFI Kotlin bindings and JNA's native
# method declarations so R8 doesn't strip them when minifyEnabled is true.
-keep class io.github.sriharsha_y.pqc.** { *; }
-keep class com.sun.jna.** { *; }
-keepclasseswithmembers class * { native <methods>; }
