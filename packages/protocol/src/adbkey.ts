import { createHash, createPublicKey } from "node:crypto";

/**
 * Parsing for `~/.android/adbkey.pub`.
 *
 * Deliberately *not* re-exported from the package index: it needs `node:crypto`
 * and the web app bundles the index. Import it by subpath.
 */

/** Bytes in the on-disk blob: two u32 headers, modulus, R², exponent. */
const BLOB_SIZE = 524;
/** 2048-bit keys only, which is all `adb` has ever generated. */
const MODULUS_WORDS = 64;
const MODULUS_SIZE = MODULUS_WORDS * 4;

export interface AdbPublicKey {
  /** The base64 blob alone, with any comment stripped. */
  publicKey: string;
  /** `SHA256:<base64>` over the DER SubjectPublicKeyInfo, SSH-style. */
  fingerprint: string;
  comment?: string;
}

export class AdbKeyParseError extends Error {}

/**
 * Parse the contents of an `adbkey.pub` file.
 *
 * The format is Android's own: base64 of a little-endian `RSAPublicKey` struct
 * — `modulus_size_words`, `n0inv`, a 256-byte little-endian modulus, a 256-byte
 * R² used by the device's Montgomery reduction, then the exponent — optionally
 * followed by a space and a comment like `dinip@laptop`. It is not PEM and not
 * an OpenSSH key, so nothing off the shelf reads it.
 *
 * `n0inv` and R² are derivable from the modulus and are only there to save the
 * bootloader the work, so they are read past rather than checked.
 */
export function parseAdbPublicKey(contents: string): AdbPublicKey {
  const [encoded, ...rest] = contents.trim().split(/\s+/);
  if (!encoded) throw new AdbKeyParseError("The key is empty");

  let blob: Buffer;
  try {
    blob = Buffer.from(encoded, "base64");
  } catch {
    throw new AdbKeyParseError("The key is not valid base64");
  }

  if (blob.length !== BLOB_SIZE) {
    throw new AdbKeyParseError(
      `Expected a ${BLOB_SIZE}-byte ADB public key, got ${blob.length} bytes. ` +
        "This should be the contents of ~/.android/adbkey.pub, not the private key.",
    );
  }
  if (blob.readUInt32LE(0) !== MODULUS_WORDS) {
    throw new AdbKeyParseError("Only 2048-bit ADB keys are supported");
  }

  // Little-endian on disk, big-endian everywhere else in cryptography.
  const modulus = Buffer.from(blob.subarray(8, 8 + MODULUS_SIZE)).reverse();
  const exponent = blob.readUInt32LE(8 + MODULUS_SIZE * 2);

  return {
    publicKey: encoded,
    fingerprint: fingerprintOf(modulus, exponent),
    comment: rest.length ? rest.join(" ") : undefined,
  };
}

/**
 * The fingerprint the UI shows and the provider matches on.
 *
 * SHA-256 over the DER SubjectPublicKeyInfo, base64, unpadded — the shape
 * `ssh-keygen -l` prints, so it is recognisable at a glance. STF used MD5 over
 * a different encoding; there is nothing to stay compatible with.
 *
 * The provider derives this in Rust from the same key and the two must agree
 * exactly, which is what the shared vectors in `test/adbkey.test.ts` and
 * `adb-bridge` guard.
 */
function fingerprintOf(modulus: Buffer, exponent: number): string {
  const der = createPublicKey({
    key: {
      kty: "RSA",
      n: base64url(trimLeadingZeros(modulus)),
      e: base64url(trimLeadingZeros(bigEndian(exponent))),
    },
    format: "jwk",
  }).export({ type: "spki", format: "der" });

  return `SHA256:${createHash("sha256").update(der).digest("base64").replace(/=+$/, "")}`;
}

function bigEndian(value: number): Buffer {
  const buf = Buffer.alloc(4);
  buf.writeUInt32BE(value);
  return buf;
}

/** JWK integers carry no leading zero byte, unlike DER. */
function trimLeadingZeros(buf: Buffer): Buffer {
  let start = 0;
  while (start < buf.length - 1 && buf[start] === 0) start++;
  return buf.subarray(start);
}

function base64url(buf: Buffer): string {
  return buf.toString("base64url");
}
