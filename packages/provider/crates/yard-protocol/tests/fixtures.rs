//! Cross-language wire check.
//!
//! `tests/fixtures.json` is written by `bun test packages/protocol` from the
//! same fixture set zod validates. Parsing it here with serde and re-encoding
//! it proves the two implementations agree byte-for-byte — a schema change that
//! breaks one language but not the other fails this test.
//!
//! If this file is missing, run `bun test packages/protocol` first.

use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use yard_protocol::{
    ClientMessage, CoordinatorMessage, FileListing, ProviderMessage, ServerMessage,
};

fn fixtures() -> Value {
    let raw = include_str!("fixtures.json");
    serde_json::from_str(raw).expect("fixtures.json is not valid JSON")
}

/// Rewrites every JSON number as an f64 so `3` and `3.0` compare equal.
///
/// JSON does not distinguish integers from floats, but `serde_json::Value`
/// does, and a zod `z.number()` field typed as `f64` in Rust re-encodes `3` as
/// `3.0`. That is a representation difference, not a protocol difference — and
/// leaving it unnormalised would train us to ignore this test's failures.
fn normalize(value: &Value) -> Value {
    match value {
        Value::Number(n) => {
            let f = n.as_f64().expect("JSON number is not representable as f64");
            Value::Number(serde_json::Number::from_f64(f).expect("non-finite JSON number"))
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), normalize(v))).collect())
        }
        other => other.clone(),
    }
}

/// Decodes each fixture into `T` and re-encodes it, asserting the JSON is
/// unchanged. Round-tripping is the part that matters: decoding alone would
/// pass even if serde silently dropped a field.
fn round_trip<T: Serialize + DeserializeOwned>(group: &str) {
    let all = fixtures();
    let group_value = all
        .get(group)
        .unwrap_or_else(|| panic!("fixtures.json has no \"{group}\" group"))
        .as_object()
        .expect("group is not an object");

    assert!(!group_value.is_empty(), "\"{group}\" group is empty");

    for (label, original) in group_value {
        let decoded: T = serde_json::from_value(original.clone())
            .unwrap_or_else(|e| panic!("{group}.{label}: decode failed: {e}"));

        let reencoded = serde_json::to_value(&decoded)
            .unwrap_or_else(|e| panic!("{group}.{label}: encode failed: {e}"));

        assert_eq!(
            normalize(&reencoded),
            normalize(original),
            "{group}.{label}: re-encoded JSON differs from the fixture"
        );
    }
}

#[test]
fn provider_messages_round_trip() {
    round_trip::<ProviderMessage>("provider");
}

#[test]
fn coordinator_messages_round_trip() {
    round_trip::<CoordinatorMessage>("coordinator");
}

#[test]
fn client_messages_round_trip() {
    round_trip::<ClientMessage>("client");
}

#[test]
fn server_messages_round_trip() {
    round_trip::<ServerMessage>("server");
}

/// Not a message — the artifact plane's directory listing, which the provider
/// serialises and the browser parses with the same schema.
#[test]
fn file_listings_round_trip() {
    round_trip::<FileListing>("files");
}
