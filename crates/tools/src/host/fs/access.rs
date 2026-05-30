//! Filesystem access policy.

use serde::{Deserialize, Serialize};

use crate::host::fs::FsPath;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileAccessPolicy {
    FullReadWrite,
    FullReadOnly,
    ScopedReadWrite { root: FsPath },
    ScopedReadOnly { root: FsPath },
}

impl FileAccessPolicy {
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::FullReadOnly | Self::ScopedReadOnly { .. })
    }

    pub fn can_write_path(&self, path: &FsPath) -> bool {
        match self {
            Self::FullReadWrite => true,
            Self::FullReadOnly => false,
            Self::ScopedReadWrite { root } => path.starts_with(root),
            Self::ScopedReadOnly { .. } => false,
        }
    }

    pub fn can_read_path(&self, path: &FsPath) -> bool {
        match self {
            Self::FullReadWrite | Self::FullReadOnly => true,
            Self::ScopedReadWrite { root } | Self::ScopedReadOnly { root } => {
                path.starts_with(root)
            }
        }
    }
}
