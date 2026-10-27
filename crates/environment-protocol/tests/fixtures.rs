//! One fixture per method: params, response and an error, round-tripped
//! through the manifest's wire types under the strict decoding policy.

use std::{collections::BTreeSet, fs, path::PathBuf};

use environment_protocol::{
    bootstrap::{BuildFacts, DiscoveryDocument},
    error::ProtocolError,
    handshake::Plane,
    manifest::{MessageKind, MethodSpec, RegistrationChallenge, methods},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn fixture_file(spec: &MethodSpec) -> PathBuf {
    let name = match spec.name.strip_prefix("$/") {
        Some(rest) => format!("rpc/{rest}"),
        None => spec.name.to_owned(),
    };
    fixtures_dir()
        .join(spec.plane.as_str())
        .join(format!("{}.json", name.replace('/', ".")))
}

fn load(path: &PathBuf) -> Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("missing fixture {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn round_trip<T: Serialize + DeserializeOwned>(value: &Value) -> Value {
    serde_json::to_value(serde_json::from_value::<T>(value.clone()).expect("decode"))
        .expect("encode")
}

#[test]
fn every_method_has_fixtures_that_round_trip() {
    for spec in methods() {
        let path = fixture_file(spec);
        let fixture = load(&path);
        let params = &fixture["params"];
        assert_eq!(
            (spec.normalize_params)(params)
                .unwrap_or_else(|error| panic!("{} params: {error}", spec.name)),
            *params,
            "{} params are not canonical",
            spec.name
        );
        match spec.kind {
            MessageKind::Request => {
                let response = &fixture["response"];
                let normalize = spec.normalize_response.expect("requests have responses");
                assert_eq!(
                    normalize(response)
                        .unwrap_or_else(|error| panic!("{} response: {error}", spec.name)),
                    *response,
                    "{} response is not canonical",
                    spec.name
                );
                let error = &fixture["error"];
                let decoded: ProtocolError = serde_json::from_value(error.clone())
                    .unwrap_or_else(|decode| panic!("{} error: {decode}", spec.name));
                assert_eq!(
                    error["code"],
                    json!(decoded.code()),
                    "{} error code does not match its kind",
                    spec.name
                );
                assert_eq!(serde_json::to_value(&decoded).unwrap(), *error);
            }
            MessageKind::Notification => {
                assert!(
                    fixture.get("response").is_none() && fixture.get("error").is_none(),
                    "notification {} is never answered",
                    spec.name
                );
            }
        }
    }
}

#[test]
fn every_fixture_belongs_to_a_method() {
    let expected: BTreeSet<PathBuf> = methods().map(fixture_file).collect();
    for plane in [Plane::Data, Plane::Controller, Plane::Registration] {
        for entry in fs::read_dir(fixtures_dir().join(plane.as_str())).expect("fixture dir") {
            let path = entry.expect("entry").path();
            assert!(
                expected.contains(&path),
                "orphan fixture {}",
                path.display()
            );
        }
    }
}

/// Members whose values are free-form string maps rather than wire
/// structs.
const FREE_FORM_MAPS: &[&str] = &["metadata", "env", "secretEnv"];

/// The path of every JSON object inside `value` that is a wire struct,
/// `value` itself included.
fn object_paths(value: &Value, path: &mut Vec<String>, paths: &mut Vec<Vec<String>>) {
    match value {
        Value::Object(map) => {
            paths.push(path.clone());
            for (key, child) in map {
                if FREE_FORM_MAPS.contains(&key.as_str()) {
                    continue;
                }
                path.push(key.clone());
                object_paths(child, path, paths);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                path.push(index.to_string());
                object_paths(child, path, paths);
                path.pop();
            }
        }
        _ => {}
    }
}

/// `value` with an unknown member added to the object at `path`.
fn with_unknown_member(value: &Value, path: &[String]) -> Value {
    let mut value = value.clone();
    let mut target = &mut value;
    for segment in path {
        target = match target {
            Value::Array(items) => &mut items[segment.parse::<usize>().expect("index")],
            other => &mut other[segment.as_str()],
        };
    }
    target["unexpectedMember"] = json!(true);
    value
}

/// Every nested object of `value` refuses an unknown member under `decode`.
fn assert_strict(name: &str, part: &str, value: &Value, decode: impl Fn(&Value) -> bool) {
    let mut paths = Vec::new();
    object_paths(value, &mut Vec::new(), &mut paths);
    assert!(!paths.is_empty(), "{name} {part} has no object");
    for path in paths {
        assert!(
            !decode(&with_unknown_member(value, &path)),
            "{name} {part} accepts an unknown member at /{}",
            path.join("/")
        );
    }
}

#[test]
fn versioned_messages_reject_unknown_members_at_every_depth() {
    for spec in methods() {
        if spec.name == RegistrationChallenge::NAME {
            continue;
        }
        let fixture = load(&fixture_file(spec));
        assert_strict(spec.name, "params", &fixture["params"], |value| {
            (spec.normalize_params)(value).is_ok()
        });
        if let Some(normalize) = spec.normalize_response {
            assert_strict(spec.name, "response", &fixture["response"], |value| {
                normalize(value).is_ok()
            });
            assert_strict(spec.name, "error", &fixture["error"], |value| {
                serde_json::from_value::<ProtocolError>(value.clone()).is_ok()
            });
        }
    }
}

#[test]
fn the_frozen_challenge_tolerates_additions() {
    let spec = methods()
        .find(|spec| spec.name == RegistrationChallenge::NAME)
        .expect("challenge");
    let mut params = load(&fixture_file(spec))["params"].clone();
    params["addedInALaterVersion"] = json!({"any": "shape"});
    assert!((spec.normalize_params)(&params).is_ok());
}

#[test]
fn bootstrap_documents_round_trip_and_tolerate_additions() {
    let dir = fixtures_dir().join("bootstrap");
    let discovery = load(&dir.join("discovery-document.json"));
    assert_eq!(round_trip::<DiscoveryDocument>(&discovery), discovery);
    let build = load(&dir.join("build-facts.json"));
    assert_eq!(round_trip::<BuildFacts>(&build), build);

    let mut extended = discovery.clone();
    extended["addedLater"] = json!(1);
    extended["artifacts"]["x86_64-unknown-linux-musl"]["url"] = Value::Null;
    let decoded: DiscoveryDocument = serde_json::from_value(extended).expect("tolerant");
    assert!(decoded.artifacts["x86_64-unknown-linux-musl"].url.is_none());
}
