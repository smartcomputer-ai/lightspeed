//! Machine-readable contract export, one set of documents per plane:
//!
//! - `<plane>.schema.json`: a draft-07 JSON Schema bundle of every wire type
//!   the plane uses, under `definitions`;
//! - `<plane>.openrpc.json`: an OpenRPC document of the plane's methods with
//!   by-name params, the manifest facts as `x-lightspeed-*` members, and the
//!   error kinds.
//!
//! The controller plane also names two types no method carries as a whole:
//! `x-lightspeed-data-route` (the data route refusal body and its bounds)
//! and `x-lightspeed-idle-policy` (the policy providers run).
//!
//! The committed copies under `crates/environment-protocol/contract/` are
//! checked by the `contract` integration test; regenerate them with
//! `cargo run -p environment-protocol --features schema --bin export-contract`.

use std::collections::BTreeMap;

use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

use crate::{
    CONTROLLER_PROTOCOL_VERSION, ENVD_PROTOCOL_VERSION,
    bootstrap::{BuildFacts, DiscoveryDocument},
    error::{ProtocolError, ProtocolErrorKind},
    handshake::Plane,
    idle::IdlePolicy,
    limits::{MAX_DATA_ROUTE_HOLD_MS, MAX_MESSAGE_BYTES, RETRY_AFTER_MAX_SECS},
    manifest::{MessageKind, plane_methods},
    routes::DataRouteRefusal,
};

/// The exported documents of one plane.
pub struct PlaneContract {
    pub plane: Plane,
    pub schema_bundle: Value,
    pub openrpc: Value,
}

impl PlaneContract {
    pub fn schema_file_name(&self) -> String {
        format!("{}.schema.json", self.plane)
    }

    pub fn openrpc_file_name(&self) -> String {
        format!("{}.openrpc.json", self.plane)
    }
}

pub fn export() -> Vec<PlaneContract> {
    [Plane::Data, Plane::Controller, Plane::Registration]
        .into_iter()
        .map(export_plane)
        .collect()
}

fn export_plane(plane: Plane) -> PlaneContract {
    let mut generator = SchemaSettings::draft07().into_generator();
    let error_schema = to_value(generator.subschema_for::<ProtocolError>());
    let bootstrap = (plane == Plane::Registration).then(|| {
        json!({
            "discoveryDocument": to_value(generator.subschema_for::<DiscoveryDocument>()),
            "buildFacts": to_value(generator.subschema_for::<BuildFacts>()),
        })
    });
    let controller_types = (plane == Plane::Controller).then(|| {
        (
            json!({
                "refusal": to_value(generator.subschema_for::<DataRouteRefusal>()),
                "maxHoldMs": MAX_DATA_ROUTE_HOLD_MS,
                "retryAfterMaxSecs": RETRY_AFTER_MAX_SECS,
            }),
            to_value(generator.subschema_for::<IdlePolicy>()),
        )
    });
    let mut entries = Vec::new();
    for spec in plane_methods(plane) {
        let schemas = (spec.schemas)(&mut generator);
        entries.push((
            spec,
            to_value(schemas.params),
            schemas.response.map(to_value),
        ));
    }
    let definitions: BTreeMap<String, Value> = generator
        .take_definitions(true)
        .into_iter()
        .map(|(name, mut schema)| {
            strip_null(&mut schema);
            clean_descriptions(&mut schema);
            (name, schema)
        })
        .collect();

    let methods: Vec<Value> = entries
        .into_iter()
        .map(|(spec, params, response)| {
            let mut method = json!({
                "name": spec.name,
                "summary": spec.summary,
                "paramStructure": "by-name",
                "params": by_name_params(&params, &definitions),
                "x-lightspeed-kind": spec.kind.as_str(),
                "x-lightspeed-direction": spec.direction.as_str(),
                "x-lightspeed-family": spec.family.as_str(),
                "x-lightspeed-params-type": spec.params_type,
            });
            if let Some(activity) = spec.activity {
                method["x-lightspeed-activity"] = json!(activity.as_str());
            }
            if let Some(capability) = spec.capability {
                method["x-lightspeed-capability"] = json!(capability);
            }
            if spec.abortable {
                method["x-lightspeed-abortable"] = json!(true);
            }
            if let (MessageKind::Request, Some(response)) = (spec.kind, response) {
                method["result"] = json!({"name": "result", "schema": response});
                method["x-lightspeed-result-type"] = json!(spec.response_type);
            }
            method
        })
        .collect();

    let (title, version) = match plane {
        Plane::Data => ("Lightspeed environment data plane", ENVD_PROTOCOL_VERSION),
        Plane::Controller => (
            "Lightspeed environment controller plane",
            CONTROLLER_PROTOCOL_VERSION,
        ),
        Plane::Registration => (
            "Lightspeed environment registration channel",
            ENVD_PROTOCOL_VERSION,
        ),
    };
    let error_kinds: Vec<Value> = ProtocolErrorKind::ALL
        .into_iter()
        .map(|kind| {
            json!({
                "kind": kind.as_str(),
                "code": kind.code(),
                "retryable": kind.is_retryable(),
            })
        })
        .collect();

    let mut schema_bundle = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": title,
        "description": format!("Every wire type of the {plane} plane, protocol version {version}."),
        "definitions": definitions,
    });
    if let Some(bootstrap) = &bootstrap {
        schema_bundle["x-lightspeed-bootstrap"] = bootstrap.clone();
    }
    if let Some((data_route, idle_policy)) = &controller_types {
        schema_bundle["x-lightspeed-data-route"] = data_route.clone();
        schema_bundle["x-lightspeed-idle-policy"] = idle_policy.clone();
    }

    let mut openrpc = json!({
        "openrpc": "1.3.2",
        "info": {
            "title": title,
            "version": version.to_string(),
            "description": format!(
                "JSON-RPC 2.0 over WebSocket text frames of at most {MAX_MESSAGE_BYTES} bytes. \
                 Exact-match protocolVersion {version}. Errors carry code, message and \
                 data.kind; see x-lightspeed-error-kinds."
            ),
        },
        "methods": methods,
        "components": {
            "schemas": schema_bundle["definitions"].clone(),
        },
        "x-lightspeed-plane": plane.as_str(),
        "x-lightspeed-error": error_schema,
        "x-lightspeed-error-kinds": error_kinds,
    });
    if let Some(bootstrap) = bootstrap {
        openrpc["x-lightspeed-bootstrap"] = bootstrap;
    }
    if let Some((data_route, idle_policy)) = controller_types {
        openrpc["x-lightspeed-data-route"] = data_route;
        openrpc["x-lightspeed-idle-policy"] = idle_policy;
    }
    rewrite_refs(&mut openrpc, "#/definitions/", "#/components/schemas/");

    PlaneContract {
        plane,
        schema_bundle,
        openrpc,
    }
}

/// One OpenRPC content descriptor per property of the params object.
fn by_name_params(schema: &Value, definitions: &BTreeMap<String, Value>) -> Value {
    let resolved = schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/definitions/"))
        .and_then(|name| definitions.get(name))
        .unwrap_or(schema);
    let required: Vec<&str> = resolved
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(properties) = resolved.get("properties").and_then(Value::as_object) else {
        return json!([]);
    };
    Value::Array(
        properties
            .iter()
            .map(|(name, schema)| {
                json!({
                    "name": name,
                    "required": required.contains(&name.as_str()),
                    "schema": schema,
                })
            })
            .collect(),
    )
}

fn to_value(schema: schemars::Schema) -> Value {
    let mut value = serde_json::to_value(schema).expect("schema serializes");
    strip_null(&mut value);
    clean_descriptions(&mut value);
    value
}

/// Descriptions come from rustdoc comments; implementers read them without
/// rustdoc, so intra-doc links become their plain text.
fn clean_descriptions(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                match entry {
                    Value::String(text) if key == "description" => *text = strip_doc_links(text),
                    _ => clean_descriptions(entry),
                }
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                clean_descriptions(entry);
            }
        }
        _ => {}
    }
}

/// `[`X`](path)` and `[`X`]` become `` `X` ``, `[text](path)` becomes
/// `text`; brackets inside code spans are left alone.
fn strip_doc_links(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find(['`', '[']) {
        output.push_str(&rest[..index]);
        rest = &rest[index..];
        if let Some(after_tick) = rest.strip_prefix('`') {
            let end = after_tick.find('`').map_or(rest.len(), |end| end + 2);
            output.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        let Some(close) = rest.find(']') else {
            output.push_str(rest);
            return output;
        };
        let inner = &rest[1..close];
        let after = &rest[close + 1..];
        if let Some(target) = after.strip_prefix('(')
            && let Some(target_end) = target.find(')')
        {
            output.push_str(inner);
            rest = &target[target_end + 1..];
        } else if inner.len() >= 2 && inner.starts_with('`') && inner.ends_with('`') {
            output.push_str(inner);
            rest = after;
        } else {
            output.push('[');
            rest = &rest[1..];
        }
    }
    output.push_str(rest);
    output
}

/// Absent options are skipped on the wire, never sent as `null`, so the
/// `null` alternatives the schema generator adds for `Option` fields are
/// removed.
fn strip_null(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(types)) = map.get_mut("type") {
                types.retain(|kind| kind != "null");
                if types.len() == 1 {
                    let only = types.remove(0);
                    map.insert("type".to_owned(), only);
                }
            }
            for key in ["anyOf", "oneOf"] {
                if let Some(Value::Array(variants)) = map.get_mut(key) {
                    variants.retain(|variant| variant.get("type") != Some(&json!("null")));
                }
            }
            for entry in map.values_mut() {
                strip_null(entry);
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                strip_null(entry);
            }
        }
        _ => {}
    }
}

fn rewrite_refs(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if key == "$ref"
                    && let Value::String(reference) = entry
                    && let Some(rest) = reference.strip_prefix(from)
                {
                    *reference = format!("{to}{rest}");
                } else {
                    rewrite_refs(entry, from, to);
                }
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                rewrite_refs(entry, from, to);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_links_become_plain_text() {
        assert_eq!(
            strip_doc_links("see [`MAX_PATH_BYTES`] and [`X`](crate::x::X) or [the docs](a::b)"),
            "see `MAX_PATH_BYTES` and `X` or the docs"
        );
        assert_eq!(
            strip_doc_links("`[...]` is a class; [1, 2] stays"),
            "`[...]` is a class; [1, 2] stays"
        );
    }

    #[test]
    fn exported_descriptions_carry_no_doc_links() {
        for plane in export() {
            let text = plane.schema_bundle.to_string();
            assert!(!text.contains("](crate::"), "{}", plane.plane);
            assert!(!text.contains("[`"), "{}", plane.plane);
        }
    }
}
