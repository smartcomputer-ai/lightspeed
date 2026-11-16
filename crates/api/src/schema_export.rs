//! Machine-readable exports of the public API contracts.
//!
//! Renders a draft-07 JSON Schema bundle of logical inputs and response DTOs,
//! an HTTP operation catalog, OpenAPI 3.1, and a compact Markdown reference. The committed copies
//! under `crates/api/contract/` are kept current by `schema_artifacts` integration tests;
//! regenerate them with `cargo run -p api --bin export-schema`.
//! The same operation catalog also exports resource HTTP metadata and OpenAPI
//! 3.1, with a separate JSON Schema 2020-12 view of the authored Rust types.

use std::{collections::BTreeMap, fmt::Write as _};

use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

use crate::{
    AgentNotification, BlobPutResult, BlobUploadParams, METHOD_BLOBS_UPLOAD, MethodAccess,
    MethodSchemas, MethodScope, MethodSpec, UniverseAction, blob_upload_binding,
    deployment_method_manifest, method_manifest,
};

/// Every dispatchable method across both scope classes: the universe-scoped
/// manifest followed by the deployment-scoped one. The wire contract is one
/// document; `scope` on each entry tells clients which target class a
/// method belongs to (deployment methods never carry the universe header).
pub fn full_method_manifest() -> Vec<MethodSpec> {
    let mut methods = method_manifest();
    methods.extend(deployment_method_manifest());
    methods.push(MethodSpec {
        method: METHOD_BLOBS_UPLOAD, notifications:&[], scope:MethodScope::Universe,
        access:MethodAccess::Universe(UniverseAction::UseResource),
        summary:"Upload a content-addressed blob",
        description:"Streams bytes into immutable universe storage. The path is the expected content digest; Content-Length is required and both length and digest are verified. An identical upload is idempotent. Admission refreshes the blob's collection grace period.",
        params_type:"BlobUploadParams", result_type:"BlobPutResult".into(),
        http:blob_upload_binding(),
        register_schemas: |g|MethodSchemas {params:g.subschema_for::<BlobUploadParams>(),result:g.subschema_for::<BlobPutResult>()},
    });
    methods
}

pub struct ExportedSchemas {
    /// Draft-07 JSON Schema bundle: every wire type under `definitions`.
    pub schema_bundle: Value,
    /// Resource HTTP routing and input partition metadata.
    pub http: Value,
    /// OpenAPI 3.1 resource contract.
    pub openapi: Value,
    /// Compact generated human reference using the same canonical method
    /// documentation as the machine-readable artifacts.
    pub api_reference: String,
}

pub fn export_schemas() -> ExportedSchemas {
    let mut generator = SchemaSettings::draft07().into_generator();

    // The access vocabulary of the manifest, so gates generated from it
    // share the canonical spelling.
    generator.subschema_for::<crate::MethodAccess>();
    generator.subschema_for::<crate::MethodGroup>();
    generator.subschema_for::<crate::RecommendedRole>();
    generator.subschema_for::<crate::MethodTarget>();
    // HTTP-only wire types share the SDK's existing draft-07 type pipeline.
    // Logical DTOs support client generation independently of HTTP codecs.
    generator.subschema_for::<crate::ConnectionResponse>();
    generator.subschema_for::<crate::HttpProblem>();
    generator.subschema_for::<crate::BlobUploadParams>();

    for spec in full_method_manifest() {
        (spec.register_schemas)(&mut generator);
    }
    generator.subschema_for::<AgentNotification>();
    let definitions: BTreeMap<String, Value> =
        generator.take_definitions(true).into_iter().collect();
    let schema_bundle = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Lightspeed HTTP API",
        "description": "Logical operation inputs and direct response types for Lightspeed clients.",
        "definitions": definitions,
    });

    let (http, openapi) = crate::http_export::export_http_contract();
    let api_reference = render_http_reference(&http);
    ExportedSchemas {
        http,
        openapi,
        schema_bundle,
        api_reference,
    }
}

fn render_http_reference(manifest: &Value) -> String {
    let mut reference = String::from(
        "# Lightspeed HTTP API Reference\n\n\
         Generated from the Rust operation catalog. `openapi.json` defines the HTTP request and response shapes; \
         `http.json` supplies the same route, policy and input-partition metadata to generated adapters.\n\n\
         Calls use resource URLs under `/api/v1` and return their response DTO directly. \
         Errors use `application/problem+json` with a stable `kind` and typed extensions. \
         Universe selection comes from the URL; a caller-supplied `x-lightspeed-universe` header is refused.\n\n\
         Every configured runtime token can call every operation. API family, role and target metadata \
         support external gates such as Platform; the runtime does not enforce caller permissions.\n\n\
         SDK logical inputs and response DTOs use the draft-07 `api.schema.json` bundle.\n\n\
         Path identifiers are omitted from JSON bodies, including nested identifiers. Query scalars use their \
         ordinary spelling, arrays repeat the key, and structured values use JSON encoding as declared in OpenAPI. \
         Unknown query/body fields and duplicate scalar query keys are refused.\n",
    );
    let operations = manifest["operations"].as_array().expect("HTTP operations");
    for (scope, title) in [
        ("connection", "Connection"),
        ("universe", "Universe"),
        ("service", "Service"),
        ("deployment", "Deployment"),
    ] {
        writeln!(reference, "\n## {title} operations\n").expect("write reference");
        for operation in operations.iter().filter(|o| o["scope"] == scope) {
            let string = |key: &str| operation[key].as_str().unwrap_or("none");
            writeln!(
                reference,
                "### `{}`\n\n**{}**\n\n{}\n",
                string("operationId"),
                string("summary"),
                string("description")
            )
            .expect("write reference");
            writeln!(
                reference,
                "- HTTP: `{} {}`",
                string("method"),
                string("path")
            )
            .expect("write reference");
            let statuses: Vec<_> = operation["responses"]
                .as_object()
                .expect("responses")
                .keys()
                .filter(|s| s.starts_with('2'))
                .map(|s| format!("`{s}`"))
                .collect();
            let selection = if operation["success"] == "createdFlag" {
                " (`created: true` selects 201; false selects 200)"
            } else {
                ""
            };
            writeln!(reference, "- Success: {}{}", statuses.join(", "), selection)
                .expect("write reference");
            writeln!(reference, "- Transfer: `{}`", string("transfer")).expect("write reference");
            let query: Vec<_> = operation["queryFields"]
                .as_array()
                .expect("query fields")
                .iter()
                .map(|q| format!("`{}`", q.as_str().expect("query name")))
                .collect();
            writeln!(
                reference,
                "- Query: {}",
                if query.is_empty() {
                    "none".into()
                } else {
                    query.join(", ")
                }
            )
            .expect("write reference");
            let path_fields: Vec<_> = operation["pathFields"]
                .as_array()
                .expect("path fields")
                .iter()
                .map(|p| {
                    format!(
                        "`{}` → `{}`",
                        p["parameter"].as_str().unwrap(),
                        p["field"].as_str().unwrap()
                    )
                })
                .collect();
            if !path_fields.is_empty() {
                writeln!(reference, "- Path input fields: {}", path_fields.join(", "))
                    .expect("write reference");
            }
            let body = if operation["transfer"] == "binaryUpload" {
                "`application/octet-stream`; `Content-Length` required".to_owned()
            } else if let Some(properties) = operation["body"]["properties"].as_object() {
                let fields: Vec<_> = properties
                    .keys()
                    .map(|field| format!("`{field}`"))
                    .collect();
                if fields.is_empty() {
                    "empty JSON object or no body".into()
                } else {
                    format!("JSON object: {}", fields.join(", "))
                }
            } else {
                "none".into()
            };
            writeln!(
                reference,
                "- Request body: {body}\n- Access: `{}`\n- Group: `{}`\n- Role: `{}`",
                operation["access"],
                string("group"),
                string("role")
            )
            .expect("write reference");
            if operation["transfer"] == "sse" {
                writeln!(reference, "- Events: named SSE events with params-only JSON data; `watch/end` ends the stream. Reconnect and refetch; no replay cursor is promised.").expect("write reference");
            }
            reference.push('\n');
        }
    }
    reference
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_reference_covers_the_http_catalog() {
        let exported = export_schemas();
        assert!(
            exported
                .api_reference
                .starts_with("# Lightspeed HTTP API Reference")
        );
        assert!(exported.api_reference.contains("application/problem+json"));
        for operation in exported.http["operations"].as_array().unwrap() {
            let id = operation["operationId"].as_str().unwrap();
            let method = operation["method"].as_str().unwrap();
            let path = operation["path"].as_str().unwrap();
            assert!(
                exported.api_reference.contains(&format!("### `{id}`")),
                "missing {id}"
            );
            assert!(
                exported
                    .api_reference
                    .contains(&format!("`{method} {path}`")),
                "missing {method} {path}"
            );
        }
        assert!(exported.api_reference.contains("### `blobs/upload`"));
    }

    #[test]
    fn method_manifest_methods_are_unique_and_complete() {
        let manifest = full_method_manifest();
        let mut methods: Vec<&str> = manifest.iter().map(|spec| spec.method).collect();
        let total = methods.len();
        methods.sort_unstable();
        methods.dedup();
        assert_eq!(methods.len(), total, "duplicate method in manifest");
        assert_eq!(total, 147);
        let streaming: Vec<_> = manifest
            .iter()
            .filter(|spec| !spec.notifications.is_empty())
            .collect();
        assert_eq!(streaming.len(), 1);
        assert_eq!(streaming[0].method, crate::METHOD_UNIVERSE_WATCH);
        assert_eq!(streaming[0].notifications, crate::WATCH_NOTIFICATIONS);
        assert_eq!(
            manifest
                .iter()
                .filter(|spec| spec.scope == crate::MethodScope::Deployment)
                .count(),
            17
        );
    }

    #[test]
    fn method_manifest_has_compact_operational_documentation() {
        for spec in full_method_manifest() {
            assert_eq!(spec.summary, spec.summary.trim(), "{} summary", spec.method);
            assert_eq!(
                spec.description,
                spec.description.trim(),
                "{} description",
                spec.method
            );
            assert!(!spec.summary.is_empty(), "{} has no summary", spec.method);
            assert!(
                !spec.description.is_empty(),
                "{} has no description",
                spec.method
            );
            assert!(
                spec.summary.len() <= 80,
                "{} summary is too long: {} bytes",
                spec.method,
                spec.summary.len()
            );
            assert!(
                spec.description.len() <= 360,
                "{} description is too long: {} bytes",
                spec.method,
                spec.description.len()
            );
        }
    }

    #[test]
    fn exported_operation_docs_match_openapi() {
        let exported = export_schemas();
        for operation in exported.http["operations"].as_array().unwrap() {
            let path = operation["path"].as_str().unwrap();
            let verb = operation["method"].as_str().unwrap().to_ascii_lowercase();
            let openapi = &exported.openapi["paths"][path][verb];
            for field in ["operationId", "summary", "description"] {
                assert_eq!(operation[field], openapi[field]);
            }
            assert_eq!(operation["access"], openapi["x-lightspeed-access"]);
        }
    }

    #[test]
    fn method_names_carry_their_scope_prefix() {
        for spec in full_method_manifest()
            .into_iter()
            .filter(|spec| spec.scope == crate::MethodScope::Deployment)
        {
            assert!(
                crate::is_deployment_method(spec.method),
                "scope of {} must match its method-name prefix",
                spec.method
            );
        }
    }

    /// The `session/` prefix is the wire marker for session-scoped methods:
    /// everything under it addresses one session through a `sessionId` param,
    /// and nothing outside it may take one. Two exemptions: `session/list`
    /// queries the collection, and `bots/sessions/rotate` addresses a bot
    /// (the session's lifecycle controller), naming which of its sessions
    /// to rotate.
    #[test]
    fn session_prefix_matches_session_id_in_params() {
        let exported = export_schemas();
        let definitions = exported.schema_bundle["definitions"]
            .as_object()
            .expect("bundle has definitions");
        for entry in exported.http["operations"].as_array().expect("methods") {
            let method = entry["operationId"].as_str().expect("method name");
            let params = match entry["params"]["schema"]["$ref"].as_str() {
                Some(reference) => {
                    let name = reference
                        .strip_prefix("api.schema.json#/definitions/")
                        .unwrap_or_else(|| panic!("unexpected ref shape: {reference}"));
                    &definitions[name]
                }
                None => &entry["params"]["schema"],
            };
            let has_session_id = !params["properties"]["sessionId"].is_null();
            let session_scoped = (method.starts_with("session/")
                && method != crate::METHOD_SESSION_LIST)
                || method == crate::METHOD_BOTS_SESSIONS_ROTATE
                || method == crate::METHOD_DEPLOYMENT_SESSIONS_PURGE;
            assert_eq!(
                has_session_id, session_scoped,
                "{method}: sessionId param presence must match its session/ prefix"
            );
        }
    }

    #[test]
    fn notification_methods_match_notification_schema_variants() {
        let exported = export_schemas();
        let variants = exported.schema_bundle["definitions"]["AgentNotification"]["oneOf"]
            .as_array()
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
        let mut declared: Vec<&str> = crate::WATCH_NOTIFICATIONS.to_vec();
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
        for operation in exported.http["operations"].as_array().unwrap() {
            collect_refs(&operation["params"], &mut references);
            collect_refs(&operation["result"], &mut references);
        }
        assert!(!references.is_empty());
        for reference in references {
            let name = reference
                .strip_prefix("api.schema.json")
                .unwrap_or(&reference)
                .strip_prefix("#/definitions/")
                .unwrap_or_else(|| panic!("unexpected ref shape: {reference}"));
            assert!(definitions.contains_key(name), "dangling ref: {reference}");
        }

        let components = exported.openapi["components"]["schemas"]
            .as_object()
            .expect("openapi has component schemas");
        let mut openapi_refs = Vec::new();
        collect_refs(&exported.openapi, &mut openapi_refs);
        for reference in openapi_refs {
            let name = reference
                .strip_prefix("#/components/schemas/")
                .unwrap_or_else(|| panic!("unexpected openapi ref shape: {reference}"));
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
