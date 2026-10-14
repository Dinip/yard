package com.yard;

/** VolumeCtrl in cmd media_session uses a package name that does not belong to the shell UID. */
public final class AudioVolume {
    public static void main(String[] args) throws Exception {
        int volume = Integer.parseInt(args[0]);
        if (volume < 0) {
            throw new IllegalArgumentException("Media volume must be nonnegative");
        }
        Class<?> manager = Class.forName("android.os.ServiceManager");
        Object binder = manager.getMethod("getService", String.class).invoke(null, "audio");
        Class<?> stub = Class.forName("android.media.IAudioService$Stub");
        Object service = stub.getMethod("asInterface", Class.forName("android.os.IBinder"))
            .invoke(null, binder);
        // Resolve the device's Binder interface instead of hardcoding transaction IDs per Android release.
        Class<?> audio = Class.forName("android.media.IAudioService");
        audio.getMethod("setStreamVolume", int.class, int.class, int.class, String.class)
            .invoke(service, 3, volume, 0, "com.android.shell");
    }
}
