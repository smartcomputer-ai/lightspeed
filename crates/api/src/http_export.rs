//! OpenAPI and routing metadata derived from the typed operation catalog.
use crate::*;
use schemars::generate::SchemaSettings;
use serde_json::{Map, json};
use std::collections::BTreeSet;

pub(crate) fn export_http_contract() -> (Value, Value) {
    let mut logical_generator = SchemaSettings::draft07().into_generator();
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let problem = generator.subschema_for::<HttpProblem>();
    let notification = generator.subschema_for::<AgentNotification>();
    let specs = crate::schema_export::full_method_manifest();
    let mut paths = Map::new();
    let mut operations = Vec::new();
    let mut ids = BTreeSet::new();
    let mut route_shapes = BTreeSet::new();
    let mut route_paths: BTreeSet<&str> = BTreeSet::new();
    for spec in specs {
        assert!(
            ids.insert(spec.method),
            "duplicate operation ID {}",
            spec.method
        );
        let binding = spec.http;
        for existing in &route_paths {
            assert!(
                *existing == binding.path || !paths_overlap(existing, binding.path),
                "static/parameter HTTP route overlap: {existing} and {}",
                binding.path
            );
        }
        route_paths.insert(binding.path);
        let description = match spec.method {
            METHOD_BLOBS_READ => {
                "Streams immutable blob bytes from this universe. Content-Length reports the complete representation size. Range requests are not supported."
            }
            METHOD_INITIALIZE => {
                "Returns runtime identity, HTTP contract revision and configured-token attribution before selecting a universe. Requires authentication in authenticated mode; universe target headers are refused."
            }
            _ => spec.description,
        };
        let route_shape = binding
            .path
            .split('/')
            .map(|s| if s.starts_with('{') { "{}" } else { s })
            .collect::<Vec<_>>()
            .join("/");
        assert!(
            route_shapes.insert((binding.verb.as_str(), route_shape)),
            "colliding HTTP route {}",
            binding.path
        );
        let logical_schemas = (spec.register_schemas)(&mut logical_generator);
        let mut logical_params =
            serde_json::to_value(logical_schemas.params).expect("schema serializes");
        let mut logical_result =
            serde_json::to_value(logical_schemas.result).expect("schema serializes");
        rewrite_refs(
            &mut logical_params,
            "#/definitions/",
            "api.schema.json#/definitions/",
        );
        rewrite_refs(
            &mut logical_result,
            "#/definitions/",
            "api.schema.json#/definitions/",
        );
        let schemas = (spec.register_schemas)(&mut generator);
        for schema in generator.definitions_mut().values_mut() {
            close_record_schemas(schema, false);
        }
        let root = json!({"$defs":generator.definitions()});
        let input = serde_json::to_value(schemas.params).expect("schema serializes");
        let input = http_resolve_schema(&root, &input).clone();
        if binding.success == HttpSuccess::CreatedFlag {
            let result = serde_json::to_value(&schemas.result).expect("schema serializes");
            let result = http_resolve_schema(&root, &result);
            assert_eq!(
                result["properties"]["created"]["type"], "boolean",
                "created status selection requires a boolean result.created field"
            );
            assert!(
                result["required"]
                    .as_array()
                    .is_some_and(|fields| fields.contains(&json!("created"))),
                "created status selection requires result.created to be present"
            );
        }
        let mut body = input.clone();
        let mut parameters = Vec::new();
        let mut declared = BTreeSet::new();
        for segment in binding.path.split('/') {
            let Some(name) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
                continue;
            };
            assert!(declared.insert(name), "duplicate path parameter");
            let schema = if let Some(mapping) =
                binding.path_fields.iter().find(|p| p.parameter == name)
            {
                let value = field_schema(&root, &input, mapping.field)
                    .unwrap_or_else(|| panic!("{}: missing input {}", spec.method, mapping.field));
                remove_field(&root, &mut body, mapping.field);
                value.clone()
            } else {
                assert_eq!(
                    name, "universe",
                    "unbound path parameter on {}",
                    spec.method
                );
                assert!(matches!(
                    spec.scope,
                    MethodScope::Universe | MethodScope::Service
                ));
                json!({"type":"string","description":"Universe UUID or active handle."})
            };
            parameters.push(json!({"name":name,"in":"path","required":true,"schema":schema}));
        }
        for mapping in binding.path_fields {
            assert!(declared.contains(mapping.parameter), "unused path mapping");
        }
        assert_eq!(
            binding
                .path_fields
                .iter()
                .map(|p| p.parameter)
                .collect::<BTreeSet<_>>()
                .len(),
            binding.path_fields.len(),
            "duplicate path mapping"
        );
        assert_eq!(
            binding
                .path_fields
                .iter()
                .map(|p| p.field)
                .collect::<BTreeSet<_>>()
                .len(),
            binding.path_fields.len(),
            "input field bound to multiple path parameters"
        );
        for name in binding.query_fields {
            assert!(
                !binding.path_fields.iter().any(|p| p.field == *name),
                "field bound twice"
            );
            let schema = field_schema(&root, &input, name)
                .unwrap_or_else(|| panic!("{}: missing query field {}", spec.method, name));
            let required = input["required"]
                .as_array()
                .is_some_and(|a| a.contains(&json!(name)));
            let resolved = http_resolve_schema(&root, schema);
            let encoding = query_encoding(&root, resolved);
            let mut parameter = json!({"name":name,"in":"query","required":required,"x-lightspeed-encoding":encoding});
            if encoding == "json" {
                parameter["content"] = json!({"application/json":{"schema":schema}});
            } else {
                parameter["schema"] = schema.clone();
                parameter["style"] = json!("form");
                parameter["explode"] = json!(true);
            }
            parameters.push(parameter);
            remove_field(&root, &mut body, name);
        }
        body["additionalProperties"] = json!(false);
        let has_body =
            binding.verb.has_json_body() && binding.transfer != HttpTransfer::BinaryUpload;
        if !has_body && binding.transfer != HttpTransfer::BinaryUpload {
            assert!(
                body["properties"].as_object().is_none_or(Map::is_empty),
                "unbound input fields on {}",
                spec.method
            );
        }
        let content_type = match binding.transfer {
            HttpTransfer::Json | HttpTransfer::BinaryUpload => "application/json",
            HttpTransfer::Sse => "text/event-stream",
            HttpTransfer::BinaryDownload => "application/octet-stream",
        };
        let result_schema = match binding.transfer {
            HttpTransfer::BinaryDownload => json!({"type":"string","format":"binary"}),
            HttpTransfer::Sse => json!({
                "type":"string",
                "description":"Named SSE events. Each data field contains that notification's params object, without an outer envelope."
            }),
            _ => serde_json::to_value(&schemas.result).expect("schema serializes"),
        };
        let mut responses = Map::new();
        for status in binding.success.statuses() {
            responses.insert(
                status.to_string(),
                json!({"description":"Success","content":{content_type:{"schema":result_schema}}}),
            );
        }
        for status in [
            400, 401, 403, 404, 405, 409, 410, 413, 415, 422, 500, 503, 504,
        ] {
            responses.insert(status.to_string(),json!({"description":"Request or service failure","content":{"application/problem+json":{"schema":problem}}}));
        }
        let mut operation = json!({"operationId":spec.method,"summary":spec.summary,"description":description,"parameters":parameters,"responses":responses,"security":[{"runtimeToken":[]}],"x-lightspeed-access":spec.access,"x-lightspeed-scope":spec.scope.as_str(),"x-lightspeed-transfer":binding.transfer});
        operation["x-lightspeed-success"] = match binding.success {
            HttpSuccess::CreatedFlag => {
                json!({"kind":"createdFlag", "field":"created", "true":201, "false":200})
            }
            HttpSuccess::Ok => json!({"kind":"fixed", "status":200}),
            HttpSuccess::Created => json!({"kind":"fixed", "status":201}),
        };
        if binding.transfer == HttpTransfer::Sse {
            let notification = serde_json::to_value(&notification).expect("schema serializes");
            let variants = http_resolve_schema(&root, &notification)["oneOf"]
                .as_array()
                .expect("notification variants");
            let events: Map<String, Value> = spec
                .notifications
                .iter()
                .map(|name| {
                    let variant = variants
                        .iter()
                        .find(|v| v["properties"]["method"]["const"] == *name)
                        .unwrap_or_else(|| panic!("missing event schema {name}"));
                    ((*name).to_owned(), variant["properties"]["params"].clone())
                })
                .collect();
            operation["x-lightspeed-sse"] = json!({"events":events,"terminalEvent":"watch/end","terminalSchema":{"type":"object","properties":{},"additionalProperties":false},"resumable":false});
        }
        if has_body {
            let required = body["required"].as_array().is_some_and(|a| !a.is_empty());
            operation["requestBody"] =
                json!({"required":required,"content":{"application/json":{"schema":body}}});
        } else if binding.transfer == HttpTransfer::BinaryUpload {
            operation["requestBody"] = json!({"required":true,"content":{"application/octet-stream":{"schema":{"type":"string","format":"binary"}}}});
            operation["parameters"].as_array_mut().unwrap().push(json!({"name":"Content-Length","in":"header","required":true,"schema":{"type":"integer","minimum":0}}));
        }
        paths
            .entry(binding.path)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap()
            .insert(
                binding.verb.as_str().to_ascii_lowercase(),
                operation.clone(),
            );
        let mut descriptor = json!({"params":{"type":spec.params_type,"schema":logical_params},"result":if binding.transfer == HttpTransfer::BinaryDownload { json!({"type":"bytes","schema":{"type":"string","format":"binary"}}) } else { json!({"type":spec.result_type,"schema":logical_result}) },"operationId":spec.method,"method":binding.verb,"path":binding.path,"scope":spec.scope.as_str(),"access":spec.access,"group":MethodGroup::of(spec.method),"role":spec.access.recommended_role(),"target":method_target(spec.method),"pathFields":binding.path_fields,"queryFields":binding.query_fields,"parameters":operation["parameters"],"body":if has_body {body}else{Value::Null},"transfer":binding.transfer,"success":binding.success,"responses":responses,"events":operation["x-lightspeed-sse"],"summary":spec.summary,"description":description});
        if binding.transfer == HttpTransfer::Sse {
            descriptor
                .as_object_mut()
                .expect("operation object")
                .remove("result");
        }
        operations.push(descriptor);
    }
    let definitions = generator.take_definitions(true);
    let mut openapi = json!({"openapi":"3.1.1","jsonSchemaDialect":"https://json-schema.org/draft/2020-12/schema","info":{"title":"Lightspeed resource HTTP API","version":env!("CARGO_PKG_VERSION")},"paths":paths,"components":{"schemas":definitions,"securitySchemes":{"runtimeToken":{"type":"http","scheme":"bearer"}}}});
    let mut manifest = json!({"contractRevision":HTTP_CONTRACT_REVISION,"operations":operations,"schemas":openapi["components"]["schemas"]});
    rewrite_refs(&mut openapi, "#/$defs/", "#/components/schemas/");
    rewrite_refs(&mut manifest, "#/$defs/", "#/schemas/");
    (manifest, openapi)
}
/// HTTP input decoding refuses unknown record fields. Maps and unconstrained
/// JSON values remain open. A tagged union's common and variant properties
/// are evaluated together before closing the object.
fn close_record_schemas(schema: &mut Value, variant_with_common_fields: bool) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    let common_fields = object
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|p| !p.is_empty());
    let composite = ["allOf", "anyOf", "oneOf"]
        .iter()
        .any(|key| object.contains_key(*key));
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for property in properties.values_mut() {
            close_record_schemas(property, false);
        }
    }
    for key in ["items", "additionalProperties"] {
        if let Some(child) = object.get_mut(key) {
            close_record_schemas(child, false);
        }
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(children) = object.get_mut(key).and_then(Value::as_array_mut) {
            for child in children {
                close_record_schemas(child, common_fields && key != "prefixItems");
            }
        }
    }
    let is_object = object.contains_key("properties")
        || object.get("type").is_some_and(|t| {
            t == "object"
                || t.as_array()
                    .is_some_and(|types| types.contains(&json!("object")))
        });
    if is_object && !variant_with_common_fields && !object.contains_key("additionalProperties") {
        object.insert(
            if composite {
                "unevaluatedProperties"
            } else {
                "additionalProperties"
            }
            .into(),
            json!(false),
        );
    }
}
/// Axum chooses a path before matching its HTTP verb. A static sibling must
/// therefore never shadow a valid resource identifier on another verb.
fn paths_overlap(left: &str, right: &str) -> bool {
    let left: Vec<_> = left.split('/').collect();
    let right: Vec<_> = right.split('/').collect();
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| *a == b || a.starts_with('{') || b.starts_with('{'))
}
fn field_schema<'a>(root: &'a Value, input: &'a Value, field: &str) -> Option<&'a Value> {
    let mut current = input;
    for part in field.split('.') {
        current = http_resolve_schema(root, current)
            .get("properties")?
            .get(part)?;
    }
    Some(current)
}
fn remove_field(root: &Value, input: &mut Value, field: &str) {
    *input = http_resolve_schema(root, input).clone();
    if let Some((parent, rest)) = field.split_once('.') {
        let child = input
            .get_mut("properties")
            .and_then(|p| p.get_mut(parent))
            .expect("bound parent exists");
        remove_field(root, child, rest);
        if accepts_empty_object(root, child)
            && let Some(required) = input.get_mut("required").and_then(Value::as_array_mut)
        {
            // Path assembly creates this parent even when the JSON body omits
            // it. Requiring an otherwise empty object would misdescribe that input.
            required.retain(|value| value != parent);
        }
    } else {
        input
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .expect("input object")
            .remove(field)
            .expect("bound field exists");
        if let Some(required) = input.get_mut("required").and_then(Value::as_array_mut) {
            required.retain(|v| v != field);
        }
    }
    if input.get("oneOf").is_some() || input.get("anyOf").is_some() || input.get("allOf").is_some()
    {
        input["unevaluatedProperties"] = json!(false);
    } else {
        input["additionalProperties"] = json!(false);
    }
}
fn accepts_empty_object(root: &Value, schema: &Value) -> bool {
    let schema = http_resolve_schema(root, schema);
    if schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|r| !r.is_empty())
    {
        return false;
    }
    if schema
        .get("minProperties")
        .and_then(Value::as_u64)
        .is_some_and(|n| n > 0)
    {
        return false;
    }
    for (keyword, exact_one) in [("anyOf", false), ("oneOf", true)] {
        if let Some(variants) = schema.get(keyword).and_then(Value::as_array) {
            let matches = variants
                .iter()
                .filter(|v| accepts_empty_object(root, v))
                .count();
            if matches == 0 || (exact_one && matches != 1) {
                return false;
            }
        }
    }
    schema
        .get("allOf")
        .and_then(Value::as_array)
        .is_none_or(|variants| variants.iter().all(|v| accepts_empty_object(root, v)))
}
fn query_encoding(root: &Value, schema: &Value) -> &'static str {
    fn shape(root: &Value, schema: &Value) -> Option<&'static str> {
        let schema = http_resolve_schema(root, schema);
        let ty = &schema["type"];
        let has = |t: &str| ty == t || ty.as_array().is_some_and(|a| a.contains(&json!(t)));
        if has("array") {
            return Some("repeated");
        }
        if has("object") {
            return Some("json");
        }
        if ["string", "boolean", "integer", "number"]
            .iter()
            .any(|t| has(t))
        {
            return Some("scalar");
        }
        if ty == "null" {
            return None;
        }
        if let Some(variants) = schema
            .get("anyOf")
            .or_else(|| schema.get("oneOf"))
            .and_then(Value::as_array)
        {
            let mut kinds = variants.iter().filter_map(|v| shape(root, v));
            let first = kinds.next()?;
            return Some(if kinds.all(|k| k == first) {
                first
            } else {
                "json"
            });
        }
        Some("json")
    }
    shape(root, schema).unwrap_or("json")
}
fn rewrite_refs(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if key == "$ref"
                    && let Some(s) = value.as_str().and_then(|s| s.strip_prefix(from))
                {
                    *value = json!(format!("{to}{s}"));
                } else {
                    rewrite_refs(value, from, to);
                }
            }
        }
        Value::Array(values) => {
            for v in values {
                rewrite_refs(v, from, to);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn static_resource_names_cannot_shadow_dynamic_identifiers_on_other_verbs() {
        assert!(paths_overlap(
            "/environments/external",
            "/environments/{environmentId}"
        ));
        assert!(paths_overlap("/grants/import", "/grants/{grantId}"));
        assert!(!paths_overlap(
            "/grants/actions/import",
            "/grants/{grantId}"
        ));
        assert!(!paths_overlap(
            "/external-environments",
            "/environments/{environmentId}"
        ));
    }
    #[test]
    fn resource_catalog_covers_every_operation_once() {
        let (manifest, openapi) = export_http_contract();
        let operations = manifest["operations"].as_array().unwrap();
        assert_eq!(
            operations.len(),
            crate::schema_export::full_method_manifest().len()
        );
        for spec in crate::schema_export::full_method_manifest() {
            let operation = operations
                .iter()
                .find(|o| o["operationId"] == spec.method)
                .unwrap();
            let verb = operation["method"].as_str().unwrap().to_ascii_lowercase();
            assert_eq!(
                openapi["paths"][spec.http.path][verb]["operationId"],
                spec.method
            );
        }
    }
    #[test]
    fn body_schema_excludes_path_identifiers_including_nested_documents() {
        let (_, openapi) = export_http_contract();
        let operation =
            &openapi["paths"]["/api/v1/universes/{universe}/profiles/{profileId}"]["put"];
        let body = &operation["requestBody"]["content"]["application/json"]["schema"];
        assert!(
            body["properties"]["profile"]["properties"]
                .get("profileId")
                .is_none()
        );
        assert_eq!(body["properties"]["profile"]["additionalProperties"], false);
        assert_eq!(
            operation["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p["name"] == "profileId")
                .count(),
            1
        );
    }
    #[test]
    fn openapi_references_resolve_and_use_the_declared_dialect() {
        let (_, openapi) = export_http_contract();
        assert_eq!(openapi["openapi"], "3.1.1");
        fn visit(root: &Value, node: &Value) {
            match node {
                Value::Object(map) => {
                    for (key, value) in map {
                        if key == "$ref" {
                            let reference = value.as_str().unwrap();
                            assert!(
                                reference.starts_with("#/components/schemas/"),
                                "{reference}"
                            );
                            assert!(
                                root.pointer(&reference[1..]).is_some(),
                                "missing {reference}"
                            );
                        } else {
                            visit(root, value);
                        }
                    }
                }
                Value::Array(values) => {
                    for value in values {
                        visit(root, value)
                    }
                }
                _ => {}
            }
        }
        visit(&openapi, &openapi);
    }
    #[test]
    fn partitioning_a_tagged_document_preserves_variant_fields() {
        let (_, openapi) = export_http_contract();
        let mut schema = openapi["paths"]
            ["/api/v1/universes/{universe}/bots/{botId}/triggers/{triggerId}"]
            ["put"]["requestBody"]["content"]["application/json"]["schema"].clone();
        schema["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
        schema["components"] = openapi["components"].clone();
        let validator = jsonschema::validator_for(&schema).expect("valid partitioned schema");
        assert!(validator.is_valid(&json!({"trigger":{"kind":"webhook"}})));
        assert!(!validator.is_valid(&json!({"trigger":{"kind":"webhook","triggerId":"forged"}})));
        assert!(!validator.is_valid(&json!({"trigger":{"kind":"webhook","surprise":true}})));
        assert!(!validator.is_valid(&json!({})));
    }
    #[test]
    fn nested_input_records_are_closed_but_maps_remain_open() {
        let (_, openapi) = export_http_contract();
        let mut schema = openapi["paths"]["/api/v1/universes/{universe}/sessions/{sessionId}/runs"]
            ["post"]["requestBody"]["content"]["application/json"]["schema"]
            .clone();
        schema["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
        schema["components"] = openapi["components"].clone();
        let validator = jsonschema::validator_for(&schema).expect("valid input schema");
        assert!(validator.is_valid(&json!({"source":{"type":"input","items":[]},"config":{}})));
        assert!(
            !validator.is_valid(
                &json!({"source":{"type":"input","items":[]},"config":{"unexpected":true}})
            )
        );
        let metadata =
            &openapi["components"]["schemas"]["AgentProfileInput"]["properties"]["metadata"];
        assert_eq!(metadata["additionalProperties"]["type"], "string");
    }
    #[test]
    fn path_assembly_can_supply_an_otherwise_optional_parent() {
        let (_, openapi) = export_http_contract();
        let mut schema = openapi["paths"]["/api/v1/universes/{universe}/profiles/{profileId}"]
            ["put"]["requestBody"]["content"]["application/json"]["schema"].clone();
        schema["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
        schema["components"] = openapi["components"].clone();
        let validator = jsonschema::validator_for(&schema).expect("valid input schema");
        assert!(validator.is_valid(&json!({})));
        assert!(validator.is_valid(&json!({"profile":{}})));
    }
    #[test]
    fn binary_and_watch_contracts_do_not_advertise_json_responses() {
        let (_, openapi) = export_http_contract();
        let blob = &openapi["paths"]["/api/v1/universes/{universe}/blobs/{blobRef}/content"];
        assert!(
            blob["get"]["responses"]["200"]["content"]
                .get("application/octet-stream")
                .is_some()
        );
        assert!(
            blob["put"]["requestBody"]["content"]
                .get("application/octet-stream")
                .is_some()
        );
        assert!(openapi["paths"]["/api/v1/universes/{universe}/events"]["get"]["responses"]["200"]["content"].get("text/event-stream").is_some());
        assert!(blob.get("head").is_none());
    }
    #[test]
    fn ambiguous_completion_does_not_advertise_accepted() {
        let (_, openapi) = export_http_contract();
        for item in openapi["paths"].as_object().unwrap().values() {
            for operation in item.as_object().unwrap().values() {
                assert!(operation["responses"].get("202").is_none());
            }
        }
        let create = &openapi["paths"]["/api/v1/universes"]["post"];
        assert!(create["responses"].get("200").is_some());
        assert!(create["responses"].get("201").is_some());
    }
}
