export function appInstallError(filename: string, platform?: string): string | null {
  const lower = filename.toLowerCase();
  if (!lower.endsWith(".apk") && !lower.endsWith(".ipa")) {
    return "Choose an .apk or .ipa file.";
  }
  if (platform === "android" && !lower.endsWith(".apk")) {
    return "Android devices require an .apk file. .ipa files are for iOS.";
  }
  if (platform === "ios" && !lower.endsWith(".ipa")) {
    return "iOS devices require an .ipa file. .apk files are for Android.";
  }
  return null;
}
