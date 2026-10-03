//! OS boundary for confined traversal and atomic publication. Wire paths and inventories
//! contain no descriptors, inode numbers, or Unix permission bits. A Windows backend can
//! implement this interface with directory handles, reparse-point checks and native rename.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::*;

use environment_protocol::{
    data::inventory::MAX_INVENTORY_PATH_BYTES,
    error::{EnvironmentProtocolError as Error, EnvironmentProtocolErrorCode as Code},
};
use sha2::{Digest, Sha256};
use std::path::Path;
pub(crate) type Result<T> = std::result::Result<T, Error>;
pub(crate) fn error(code: Code, message: impl Into<String>) -> Error {
    Error::new(code, message)
}
pub(crate) fn invalid(message: &str) -> Error {
    error(Code::InvalidRequest, message)
}
pub(crate) fn io(e: std::io::Error) -> Error {
    use std::io::ErrorKind::*;
    if is_path_violation(&e) {
        return error(Code::Forbidden, e.to_string());
    }
    error(
        match e.kind() {
            NotFound => Code::NotFound,
            AlreadyExists => Code::Conflict,
            PermissionDenied => Code::Forbidden,
            Unsupported => Code::Unsupported,
            InvalidInput => Code::InvalidRequest,
            _ => Code::Internal,
        },
        e.to_string(),
    )
}
pub(crate) fn conflict() -> Error {
    error(
        Code::Conflict,
        "filesystem observation changed; retry the filesystem operation",
    )
}
pub(crate) fn valid_path(path: &str) -> bool {
    path.len() <= MAX_INVENTORY_PATH_BYTES
        && (path.is_empty()
            || (!path.contains(['\\', '\0'])
                && path
                    .split('/')
                    .all(|p| !p.is_empty() && p != "." && p != "..")))
}
pub(crate) fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
pub(crate) fn digest(hash: Sha256) -> String {
    format!("sha256:{}", hex::encode(hash.finalize()))
}
pub(crate) fn relative(
    root: &Path,
    path: &environment_protocol::shared::EnvironmentPath,
) -> Result<String> {
    let path = Path::new(path.as_str())
        .strip_prefix(root)
        .map_err(|_| error(Code::Forbidden, "selection outside filesystem root"))?;
    let value = path
        .components()
        .map(|component| match component {
            std::path::Component::Normal(name) => name
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("non-UTF-8 selection")),
            _ => Err(invalid("invalid selection component")),
        })
        .collect::<Result<Vec<_>>>()?
        .join("/");
    if !valid_path(&value) {
        return Err(invalid("invalid selection"));
    }
    Ok(value)
}
