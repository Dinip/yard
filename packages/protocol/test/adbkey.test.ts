import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { AdbKeyParseError, parseAdbPublicKey } from "../src/adbkey.ts";

const vectorDir = resolve(import.meta.dir, "vectors");
const keyFile = readFileSync(resolve(vectorDir, "adbkey.pub"), "utf8");
const blob = keyFile.trim().split(/\s+/)[0] as string;
const expected = JSON.parse(readFileSync(resolve(vectorDir, "adbkey.json"), "utf8"));

describe("parseAdbPublicKey", () => {
  test("derives the fingerprint openssl derives", () => {
    // The provider re-derives this in Rust against the same file. If the two
    // ever disagree, every `adb connect` in the farm starts asking the holder
    // to approve a key they already registered.
    const key = parseAdbPublicKey(keyFile);
    expect(key.fingerprint).toBe(expected.fingerprint);
    expect(key.comment).toBe(expected.comment);
  });

  test("keeps the blob and drops the comment", () => {
    const key = parseAdbPublicKey(keyFile);
    expect(key.publicKey).toBe(blob);
    expect(key.publicKey).not.toContain(" ");
  });

  test("a key with no comment is not an error", () => {
    const key = parseAdbPublicKey(blob);
    expect(key.comment).toBeUndefined();
    expect(key.fingerprint).toBe(expected.fingerprint);
  });

  test("surrounding whitespace and a trailing newline are ignored", () => {
    expect(parseAdbPublicKey(`\n  ${keyFile}  \n`).fingerprint).toBe(expected.fingerprint);
  });

  test("rejects a private key with a message that says so", () => {
    // The mistake everyone makes: `cat ~/.android/adbkey` instead of the .pub.
    expect(() => parseAdbPublicKey("-----BEGIN PRIVATE KEY-----")).toThrow(AdbKeyParseError);
    expect(() => parseAdbPublicKey("-----BEGIN PRIVATE KEY-----")).toThrow(/adbkey\.pub/);
  });

  test("rejects an empty key", () => {
    expect(() => parseAdbPublicKey("   \n ")).toThrow(AdbKeyParseError);
  });

  test("rejects a blob of the wrong length", () => {
    const truncated = Buffer.from(Buffer.from(blob, "base64").subarray(0, 500)).toString("base64");
    expect(() => parseAdbPublicKey(truncated)).toThrow(/523|500 bytes/);
  });
});
