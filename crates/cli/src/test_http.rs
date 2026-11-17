//! HTTP fixture decoding against the generated public route catalog.
use serde_json::{Value, json};

pub fn decode(headers: &str, body: &[u8]) -> Value {
    let mut first = headers.lines().next().unwrap().split_whitespace();
    let verb = first.next().unwrap();
    let target = first.next().unwrap();
    let url = reqwest::Url::parse(&format!("http://fixture{target}")).unwrap();
    let catalog: Value =
        serde_json::from_str(include_str!("../../api/contract/http.json")).unwrap();
    let segments: Vec<_> = url.path().split('/').collect();
    for operation in catalog["operations"].as_array().unwrap() {
        if operation["method"] != verb {
            continue;
        }
        let template: Vec<_> = operation["path"].as_str().unwrap().split('/').collect();
        if template.len() != segments.len()
            || !template
                .iter()
                .zip(&segments)
                .all(|(a, b)| a.starts_with('{') || a == b)
        {
            continue;
        }
        let mut params = if body.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(body).unwrap()
        };
        let mut universe = Value::Null;
        for (a, b) in template.iter().zip(&segments) {
            if !a.starts_with('{') {
                continue;
            }
            let name = &a[1..a.len() - 1];
            let value = percent_decode(b);
            if name == "universe" {
                universe = json!(value);
            }
            for field in operation["pathFields"].as_array().unwrap() {
                if field["parameter"] == name {
                    insert(&mut params, field["field"].as_str().unwrap(), json!(value));
                }
            }
        }
        for (name, value) in url.query_pairs() {
            let descriptor = operation["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["in"] == "query" && p["name"] == name.as_ref())
                .expect("known query field");
            let schema = &descriptor["schema"];
            let types = schema["type"].to_string();
            let value = if descriptor["x-lightspeed-encoding"] == "json"
                || types.contains("integer")
                || types.contains("number")
                || types.contains("boolean")
            {
                serde_json::from_str(&value).unwrap()
            } else {
                json!(value)
            };
            params[name.as_ref()] = value;
        }
        return json!({"method":operation["operationId"], "params":params, "universe":universe});
    }
    panic!("unknown HTTP route: {verb} {target}");
}
fn insert(value: &mut Value, path: &str, input: Value) {
    if let Some((head, tail)) = path.split_once('.') {
        if value.get(head).is_none() {
            value[head] = json!({});
        }
        insert(&mut value[head], tail, input);
    } else {
        value[path] = input;
    }
}
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(u8::from_str_radix(&value[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}
