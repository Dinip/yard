plugins {
    id("com.android.application")
    id("kotlin-android")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

android {
    namespace = "com.dinispimpao.yard.drop"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }

    kotlinOptions {
        jvmTarget = JavaVersion.VERSION_11.toString()
    }

    defaultConfig {
        // Permanent: changing it makes a different app on every farm device.
        applicationId = "com.dinispimpao.yard.drop"
        // Scoped storage without legacy fallbacks.
        minSdk = 29
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    // The farm's own signing key, supplied by the release workflow through the
    // environment. A locally built release APK falls back to debug keys, which
    // is the difference between something you can install on your own handset
    // and something the farm will accept as an upgrade.
    val keystore = System.getenv("YARD_KEYSTORE_PATH")

    signingConfigs {
        if (keystore != null) {
            create("farm") {
                storeFile = file(keystore)
                storePassword = System.getenv("YARD_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("YARD_KEY_ALIAS")
                keyPassword = System.getenv("YARD_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.getByName(if (keystore != null) "farm" else "debug")
        }
    }
}

flutter {
    source = "../.."
}

dependencies {
    // The share receiver is only true on a device: content URIs, MediaStore and
    // a stream that dies partway have no meaningful JVM stand-in.
    androidTestImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test:core:1.7.0")
    androidTestImplementation("androidx.test:runner:1.7.0")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
}
