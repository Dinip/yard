//! The fingerprint the coordinator computes in TypeScript and the one this
//! crate computes in Rust must be the same string.
//!
//! They are derived by separate implementations from the same key file. If they
//! ever drift, every `adb connect` in the farm starts asking the holder to
//! approve a key they already registered — a failure that would look like a UI
//! bug and be nothing of the kind. Both sides read the files in
//! `packages/protocol/test/vectors/`, and the expected value in `adbkey.json`
//! came from `openssl`, so neither can define itself into being correct.

use adb_bridge::PublicKey;

const VECTORS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../protocol/test/vectors/"
);

fn vector(name: &str) -> String {
    std::fs::read_to_string(format!("{VECTORS}{name}"))
        .unwrap_or_else(|err| panic!("reading {name}: {err}"))
}

/// The one field of `adbkey.json` this crate cares about, without pulling a
/// JSON parser into a crate that otherwise needs none.
fn expected(field: &str) -> String {
    let json = vector("adbkey.json");
    let needle = format!("\"{field}\": \"");
    let start = json.find(&needle).expect("field is present") + needle.len();
    json[start..].split('"').next().unwrap().to_owned()
}

#[test]
fn the_fingerprint_matches_the_one_openssl_and_typescript_produce() {
    let key = PublicKey::parse(&vector("adbkey.pub")).expect("the vector parses");
    assert_eq!(key.fingerprint(), expected("fingerprint"));
    assert_eq!(key.comment(), Some(expected("comment").as_str()));
}

#[test]
fn the_blob_is_kept_without_the_comment() {
    let file = vector("adbkey.pub");
    let key = PublicKey::parse(&file).unwrap();
    assert_eq!(key.blob(), file.split_whitespace().next().unwrap());
}

#[test]
fn a_key_offered_without_a_comment_is_the_same_key() {
    // What arrives in an `AUTH RSAPUBLICKEY` message may or may not carry the
    // comment; the identity must not depend on it.
    let file = vector("adbkey.pub");
    let bare = file.split_whitespace().next().unwrap();
    assert_eq!(
        PublicKey::parse(bare).unwrap().fingerprint(),
        PublicKey::parse(&file).unwrap().fingerprint()
    );
}
