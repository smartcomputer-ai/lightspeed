//! Windows inventory identity. VFS and portable tree paths keep their grammar;
//! admission into this filesystem adds native-name and case-alias checks.
use environment_protocol::{
    data::{fs::TreeContent, transfer::InventoryEntry},
    error::{Capability, ProtocolError},
    paths::EnvironmentPath,
};
use std::{cmp::Ordering, ffi::OsStr, os::windows::ffi::OsStrExt};
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CSTR_LESS_THAN, CompareStringOrdinal};

#[derive(Clone, Debug)]
pub(super) struct NameKey(Vec<u16>);
impl NameKey {
    pub fn new(value: &OsStr) -> Self {
        Self(value.encode_wide().collect())
    }
}
impl Ord for NameKey {
    fn cmp(&self, other: &Self) -> Ordering {
        // SAFETY: both UTF-16 buffers remain live. Native path limits fit i32.
        match unsafe {
            CompareStringOrdinal(
                self.0.as_ptr(),
                self.0.len() as i32,
                other.0.as_ptr(),
                other.0.len() as i32,
                1,
            )
        } {
            CSTR_EQUAL => Ordering::Equal,
            CSTR_LESS_THAN => Ordering::Less,
            _ => Ordering::Greater,
        }
    }
}
impl PartialOrd for NameKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for NameKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for NameKey {}

pub(super) fn validate(entry: &InventoryEntry, field: &str) -> Result<NameKey, ProtocolError> {
    if !entry.path.is_root() {
        EnvironmentPath::new(format!("C:/{}", entry.path)).map_err(|error| {
            ProtocolError::invalid_field(format!("{field}.path"), error.to_string())
        })?;
    }
    if matches!(
        entry.content,
        TreeContent::File {
            executable: true,
            ..
        }
    ) {
        return Err(ProtocolError::capability_unavailable(
            Capability::FilesystemWrite,
            "Windows transfers cannot represent Unix executable bits",
        ));
    }
    Ok(NameKey::new(OsStr::new(entry.path.as_str())))
}
