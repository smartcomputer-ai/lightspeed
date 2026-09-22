//! Every public method authorizes by name. The manifest declares each
//! method's requirement; a handler that never passes its method name to the
//! authorizer would be dispatched with no check at all, so this reads the
//! gateway sources and demands one call per method.

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn read_sources(dir: &Path, into: &mut String) {
        for entry in std::fs::read_dir(dir).expect("gateway source directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                read_sources(&path, into);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                into.push_str(&std::fs::read_to_string(&path).expect("gateway source file"));
            }
        }
    }

    /// The constant naming each method, found in the `api` crate's sources so
    /// the manifest and the handlers are matched through the same identifier.
    fn method_constants() -> Vec<(&'static str, String)> {
        let api_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../api/src");
        let mut sources = String::new();
        read_sources(&api_src, &mut sources);
        api::method_manifest()
            .into_iter()
            .chain(api::deployment_method_manifest())
            .map(|spec| {
                let needle = format!("\"{}\"", spec.method);
                let constant = sources
                    .split("pub const ")
                    .skip(1)
                    .find(|decl| decl.contains(&needle) && decl.starts_with("METHOD_"))
                    .and_then(|decl| decl.split(':').next())
                    .unwrap_or_else(|| panic!("no METHOD_ constant declares {}", spec.method))
                    .to_owned();
                (spec.method, constant)
            })
            .collect()
    }

    #[test]
    fn every_method_authorizes_by_its_manifest_name() {
        let gateway_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gateway");
        let mut sources = String::new();
        read_sources(&gateway_src, &mut sources);
        let compact: String = sources.split_whitespace().collect();
        let mut unchecked = Vec::new();
        for (method, constant) in method_constants() {
            let authorizes = [
                format!("authorize_method({constant},"),
                format!("authorize_method({constant})"),
                format!("authorize_method(api::{constant},"),
                format!("admitted({constant},"),
                format!("admitted(api::{constant},"),
            ]
            .iter()
            .any(|call| compact.contains(call.as_str()));
            if !authorizes {
                unchecked.push(method);
            }
        }
        assert!(
            unchecked.is_empty(),
            "methods whose handlers never authorize by name: {unchecked:?}"
        );
    }
}
