//! Machine-readable export of the JSON-RPC wire contract.
//!
//! Renders three artifacts from the method manifest and the schemars-derived
//! types: a draft-07 JSON Schema bundle of every wire type, a method manifest
//! document, and an OpenRPC document for docs tooling. The committed copies
//! under `interop/contract/` are kept current by `schema_artifacts` integration tests;
//! regenerate them with `cargo run -p api --bin export-schema`.

use std::collections::BTreeMap;

use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

use crate::{
    AgentNotification, JsonRpcError, MethodSpec, NOTIFICATION_METHODS, PROTOCOL_VERSION,
    method_manifest, operator_method_manifest,
};

/// Every dispatchable method across both scope classes: the universe-scoped
/// manifest followed by the operator-scoped one. The wire contract is one
/// document; `scope` on each entry tells clients which authorization class a
/// method belongs to (operator methods never carry the universe header).
pub fn full_method_manifest() -> Vec<MethodSpec> {
    let mut methods = method_manifest();
    methods.extend(operator_method_manifest());
    methods
}

pub struct ExportedSchemas {
    /// Draft-07 JSON Schema bundle: every wire type under `definitions`.
    pub schema_bundle: Value,
    /// Method/notification manifest with refs into the schema bundle.
    pub methods: Value,
    /// OpenRPC document assembled from the two; for docs tooling only, no
    /// downstream codegen may depend on it.
    pub openrpc: Value,
}

pub fn export_schemas() -> ExportedSchemas {
    let mut generator = SchemaSettings::draft07().into_generator();

    let mut methods = Vec::new();
    let mut openrpc_methods = Vec::new();
    for spec in full_method_manifest() {
        let schemas = (spec.register_schemas)(&mut generator);
        let params_schema = serde_json::to_value(&schemas.params).expect("schema serializes");
        let result_schema = serde_json::to_value(&schemas.result).expect("schema serializes");
        methods.push(json!({
            "method": spec.method,
            "scope": spec.scope.as_str(),
            "params": { "type": spec.params_type, "schema": params_schema },
            "result": { "type": spec.result_type, "schema": result_schema },
        }));
        openrpc_methods.push(json!({
            "name": spec.method,
            "paramStructure": "by-name",
            "description":
                "Single-object params: the request `params` member is one JSON object \
                 described by this descriptor's schema.",
            "params": [{
                "name": "params",
                "required": true,
                "schema": params_schema,
            }],
            "result": { "name": "result", "schema": result_schema },
        }));
    }
    let notification_schema = serde_json::to_value(generator.subschema_for::<AgentNotification>())
        .expect("schema serializes");
    let error_schema =
        serde_json::to_value(generator.subschema_for::<JsonRpcError>()).expect("schema serializes");

    let definitions: BTreeMap<String, Value> =
        generator.take_definitions(true).into_iter().collect();

    let schema_bundle = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Lightspeed Agent API",
        "description": "All JSON-RPC wire types of the Lightspeed agent API.",
        "definitions": definitions,
    });

    let methods_doc = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "transport": {
            "kind": "http-json-rpc",
            "endpoint": "/rpc",
            "resultEnvelope": "AgentApiOutcome",
            "error": { "type": "JsonRpcError", "schema": error_schema },
        },
        "methods": methods,
        "notifications": NOTIFICATION_METHODS,
        "notificationSchema": notification_schema,
    });

    let mut openrpc = json!({
        "openrpc": "1.3.2",
        "info": {
            "title": "Lightspeed Agent API",
            "version": env!("CARGO_PKG_VERSION"),
            "description":
                "JSON-RPC 2.0 over HTTP POST /rpc. Results are wrapped in the \
                 AgentApiOutcome envelope; session events are consumed via \
                 session/events/read cursor pagination.",
        },
        "methods": openrpc_methods,
        "components": { "schemas": schema_bundle["definitions"].clone() },
    });
    rewrite_refs(&mut openrpc, "#/definitions/", "#/components/schemas/");

    ExportedSchemas {
        schema_bundle,
        methods: methods_doc,
        openrpc,
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
    fn method_manifest_methods_are_unique_and_complete() {
        let manifest = full_method_manifest();
        let mut methods: Vec<&str> = manifest.iter().map(|spec| spec.method).collect();
        let total = methods.len();
        methods.sort_unstable();
        methods.dedup();
        assert_eq!(methods.len(), total, "duplicate method in manifest");
        assert_eq!(total, 89);
        assert_eq!(
            manifest
                .iter()
                .filter(|spec| spec.scope == crate::MethodScope::Operator)
                .count(),
            5
        );
    }

    #[test]
    fn method_names_carry_their_scope_prefix() {
        for spec in full_method_manifest() {
            assert_eq!(
                crate::is_operator_method(spec.method),
                spec.scope == crate::MethodScope::Operator,
                "scope of {} must match its method-name prefix",
                spec.method
            );
        }
    }

    /// The `session/` prefix is the wire marker for session-scoped methods:
    /// everything under it addresses one session through a `sessionId` param,
    /// and nothing outside it may take one. `session/list` queries the
    /// collection, so it is the single exemption.
    #[test]
    fn session_prefix_matches_session_id_in_params() {
        let exported = export_schemas();
        let definitions = exported.schema_bundle["definitions"]
            .as_object()
            .expect("bundle has definitions");
        for entry in exported.methods["methods"].as_array().expect("methods") {
            let method = entry["method"].as_str().expect("method name");
            let params = match entry["params"]["schema"]["$ref"].as_str() {
                Some(reference) => {
                    let name = reference
                        .strip_prefix("#/definitions/")
                        .unwrap_or_else(|| panic!("unexpected ref shape: {reference}"));
                    &definitions[name]
                }
                None => &entry["params"]["schema"],
            };
            let has_session_id = !params["properties"]["sessionId"].is_null();
            let session_scoped =
                method.starts_with("session/") && method != crate::METHOD_SESSION_LIST;
            assert_eq!(
                has_session_id, session_scoped,
                "{method}: sessionId param presence must match its session/ prefix"
            );
        }
    }

    #[test]
    fn notification_methods_match_notification_schema_variants() {
        let exported = export_schemas();
        let variants = exported.methods["notificationSchema"]["oneOf"]
            .as_array()
            .or_else(|| {
                // The subschema may be a $ref into the bundle.
                let reference = exported.methods["notificationSchema"]["$ref"].as_str()?;
                let name = reference.strip_prefix("#/definitions/")?;
                exported.schema_bundle["definitions"][name]["oneOf"].as_array()
            })
            .expect("AgentNotification schema exposes oneOf variants");
        let mut schema_methods: Vec<&str> = variants
            .iter()
            .map(|variant| {
                variant["properties"]["method"]["const"]
                    .as_str()
                    .expect("variant has const method tag")
            })
            .collect();
        schema_methods.sort_unstable();
        let mut declared: Vec<&str> = NOTIFICATION_METHODS.to_vec();
        declared.sort_unstable();
        assert_eq!(schema_methods, declared);
    }

    #[test]
    fn exported_refs_resolve_within_their_documents() {
        let exported = export_schemas();
        let definitions = exported.schema_bundle["definitions"]
            .as_object()
            .expect("bundle has definitions");
        let mut references = Vec::new();
        collect_refs(&exported.schema_bundle, &mut references);
        collect_refs(&exported.methods, &mut references);
        assert!(!references.is_empty());
        for reference in references {
            let name = reference
                .strip_prefix("#/definitions/")
                .unwrap_or_else(|| panic!("unexpected ref shape: {reference}"));
            assert!(definitions.contains_key(name), "dangling ref: {reference}");
        }

        let components = exported.openrpc["components"]["schemas"]
            .as_object()
            .expect("openrpc has component schemas");
        let mut openrpc_refs = Vec::new();
        collect_refs(&exported.openrpc, &mut openrpc_refs);
        for reference in openrpc_refs {
            let name = reference
                .strip_prefix("#/components/schemas/")
                .unwrap_or_else(|| panic!("unexpected openrpc ref shape: {reference}"));
            assert!(components.contains_key(name), "dangling ref: {reference}");
        }
    }

    fn collect_refs(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, entry) in map {
                    if key == "$ref" {
                        if let Value::String(reference) = entry {
                            out.push(reference.clone());
                        }
                    } else {
                        collect_refs(entry, out);
                    }
                }
            }
            Value::Array(entries) => {
                for entry in entries {
                    collect_refs(entry, out);
                }
            }
            _ => {}
        }
    }
}
