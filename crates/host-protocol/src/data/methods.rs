//! JSON-RPC method names for the host data plane.

pub const INITIALIZE_METHOD: &str = "initialize";
pub const INITIALIZED_METHOD: &str = "initialized";

pub const FS_READ_FILE_METHOD: &str = "fs/readFile";
pub const FS_WRITE_FILE_METHOD: &str = "fs/writeFile";
pub const FS_CREATE_DIRECTORY_METHOD: &str = "fs/createDirectory";
pub const FS_GET_METADATA_METHOD: &str = "fs/getMetadata";
pub const FS_READ_DIRECTORY_METHOD: &str = "fs/readDirectory";
pub const FS_REMOVE_METHOD: &str = "fs/remove";
pub const FS_COPY_METHOD: &str = "fs/copy";

pub const PROCESS_START_METHOD: &str = "process/start";
pub const PROCESS_READ_METHOD: &str = "process/read";
pub const PROCESS_WRITE_METHOD: &str = "process/write";
pub const PROCESS_TERMINATE_METHOD: &str = "process/terminate";
pub const PROCESS_RESIZE_METHOD: &str = "process/resize";

pub const PROCESS_OUTPUT_METHOD: &str = "process/output";
pub const PROCESS_EXITED_METHOD: &str = "process/exited";
pub const PROCESS_CLOSED_METHOD: &str = "process/closed";
