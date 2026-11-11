import { expect, test } from "bun:test";
import { appInstallError } from "../src/lib/app-package";

test("wrong-platform packages explain which file to choose", () => {
  expect(appInstallError("release.ipa", "android")).toBe(
    "Android devices require an .apk file. .ipa files are for iOS.",
  );
  expect(appInstallError("release.apk", "ios")).toBe(
    "iOS devices require an .ipa file. .apk files are for Android.",
  );
});

test("matching packages accept either extension case", () => {
  for (const [filename, platform] of [
    ["release.apk", "android"],
    ["release.APK", "android"],
    ["release.ipa", "ios"],
    ["release.IPA", "ios"],
  ] as const) {
    expect(appInstallError(filename, platform)).toBeNull();
  }
});

test("non-package files are rejected on both platforms", () => {
  expect(appInstallError("release.zip", "ios")).toBe("Choose an .apk or .ipa file.");
  expect(appInstallError("release.apk.zip", "android")).toBe("Choose an .apk or .ipa file.");
});
