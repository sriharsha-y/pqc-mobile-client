# Consumer R8/ProGuard rules, packaged into the published AAR so apps with
# minifyEnabled=true build and run without writing their own keep rules.
# Validated against the RnSample release build (R8, AGP 8.x).

# JNA — the UniFFI bindings call into the native library via JNA. JNA
# references desktop java.awt.* (absent on Android), so R8 hard-fails on the
# missing classes without -dontwarn. Rules per java-native-access/jna FAQ +
# mozilla/application-services proguard-rules-consumer-jna.pro.
-dontwarn java.awt.**
-keep class com.sun.jna.* { *; }
-keep class * extends com.sun.jna.* { *; }
-keepclassmembers class * extends com.sun.jna.* { public *; }

# Annotations/signatures the JNA Structures (RustBuffer etc.) and Kotlin
# metadata rely on under R8 fullMode; without them fullMode strips field
# metadata and the FFI marshalling crashes.
-keepattributes RuntimeVisibleAnnotations,RuntimeInvisibleAnnotations,RuntimeVisibleTypeAnnotations,RuntimeInvisibleTypeAnnotations,AnnotationDefault,InnerClasses,EnclosingMethod,Signature

# UniFFI-generated bindings (JNA maps their native declarations by name) plus
# io.github.sriharsha_y.pqc.android.PqcAndroidInit (the JNI entry point).
-keep class io.github.sriharsha_y.pqc.** { *; }

# rustls-platform-verifier Android glue is invoked from Rust via JNI by name;
# upstream ships no consumer rules, so we provide them.
-keep class org.rustls.platformverifier.** { *; }
