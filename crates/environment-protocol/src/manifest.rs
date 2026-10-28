//! The method manifest: one table of every method on every plane, with its
//! params and response types and the facts servers and clients act on.
//!
//! envd dispatch and idle classification, the client's typed calls, the
//! contract export and the fixture tests all read this table, so a method
//! exists exactly when it has an entry here. Each entry also has a marker
//! type implementing [`RequestMethod`] or [`NotificationMethod`], which
//! binds the method name to its Rust types:
//!
//! ```
//! use environment_protocol::manifest::{ProcessRead, RequestMethod};
//! assert_eq!(ProcessRead::NAME, "process/read");
//! ```

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{
    control::{
        handshake::{ControllerInitializeParams, ControllerInitializeResponse},
        ingress::{EnsureIngressParams, RemoveIngressParams},
        target::{
            CloseTargetParams, CreateTargetParams, GetTargetParams, SetTargetPowerParams,
            TargetResponse,
        },
        template::{ListTemplatesParams, ListTemplatesResponse},
    },
    data::{
        fs::{
            CreateDirectoryParams, CreateDirectoryResponse, GetMetadataParams, GetMetadataResponse,
            GlobFilesParams, GlobFilesResponse, ReadDirectoryParams, ReadDirectoryResponse,
            ReadFileParams, ReadFileResponse, RemoveParams, RemoveResponse, RenameParams,
            RenameResponse, SearchTextParams, SearchTextResponse, WriteFileParams,
            WriteFileResponse,
        },
        handshake::{InitializeParams, InitializeResponse},
        idle::{GetIdleParams, GetIdleResponse},
        job::{
            CancelJobsParams, CancelJobsResponse, ReadJobsParams, ReadJobsResponse,
            StartJobsParams, StartJobsResponse,
        },
        process::{
            ReadProcessParams, ReadProcessResponse, StartProcessParams, StartProcessResponse,
            TerminateProcessParams, TerminateProcessResponse, WriteProcessParams,
            WriteProcessResponse,
        },
        scan::{ScanParams, ScanResponse},
        transfer::{
            AppendTransferInventoryParams, BeginTransferParams, ListTransferMissingParams,
            ListTransferMissingResponse, ReadTransferContentParams, ReadTransferContentResponse,
            ReadTransferInventoryParams, ReadTransferInventoryResponse, TransferOperationParams,
            TransferStatus, WriteTransferContentParams, WriteTransferContentResponse,
        },
    },
    error::Capability,
    handshake::Plane,
    jsonrpc::CancelRequestParams,
    registration::{
        ChallengeParams, ClosedParams, GoodbyeParams, OpenStreamParams, OpenStreamResponse,
        RegisterParams, RegisterResponse,
    },
};

/// A request method: its name, plane and the types of its params and
/// result.
pub trait RequestMethod {
    const NAME: &'static str;
    const PLANE: Plane;
    type Params: Serialize + DeserializeOwned + Send + Sync + 'static;
    type Response: Serialize + DeserializeOwned + Send + Sync + 'static;
}

/// A notification method: never answered.
pub trait NotificationMethod {
    const NAME: &'static str;
    const PLANE: Plane;
    type Params: Serialize + DeserializeOwned + Send + Sync + 'static;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MessageKind {
    Request,
    Notification,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Notification => "notification",
        }
    }
}

/// Who sends a method. The client is the peer that dialed: the runtime on
/// the data and controller planes, the daemon on the registration channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    ClientToServer,
    ServerToClient,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClientToServer => "clientToServer",
            Self::ServerToClient => "serverToClient",
        }
    }
}

/// Method family: the method name's domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Handshake,
    Envelope,
    Env,
    Filesystem,
    Transfer,
    Process,
    Job,
    Template,
    Target,
    Ingress,
    Registration,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Handshake => "handshake",
            Self::Envelope => "envelope",
            Self::Env => "env",
            Self::Filesystem => "filesystem",
            Self::Transfer => "transfer",
            Self::Process => "process",
            Self::Job => "job",
            Self::Template => "template",
            Self::Target => "target",
            Self::Ingress => "ingress",
            Self::Registration => "registration",
        }
    }
}

/// Whether a data-plane method counts toward the idle clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Activity {
    /// Real work: resets `idleForMs`.
    Use,
    /// Looking, not using: handshakes, idle reports, discovery scans.
    Observation,
}

impl Activity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Use => "use",
            Self::Observation => "observation",
        }
    }
}

/// Schemas of one method, registered in a shared generator.
#[cfg(feature = "schema")]
pub struct MethodSchemas {
    pub params: schemars::Schema,
    pub response: Option<schemars::Schema>,
}

/// Re-encode a value through a wire type: decoding enforces the contract
/// strictly, encoding yields the canonical form.
pub type Normalize = fn(&Value) -> Result<Value, serde_json::Error>;

/// One manifest entry.
pub struct MethodSpec {
    pub name: &'static str,
    pub plane: Plane,
    pub kind: MessageKind,
    pub direction: Direction,
    pub family: Family,
    /// Data-plane methods only.
    pub activity: Option<Activity>,
    /// The capability a request needs; a server without it answers
    /// `capabilityUnavailable` before doing anything.
    pub capability: Option<Capability>,
    /// `$/cancelRequest` aborts it: a read-only wait or walk. Every other
    /// method runs to a consistent state.
    pub abortable: bool,
    pub params_type: &'static str,
    /// Absent for notifications.
    pub response_type: Option<&'static str>,
    pub summary: &'static str,
    pub normalize_params: Normalize,
    pub normalize_response: Option<Normalize>,
    #[cfg(feature = "schema")]
    pub schemas: fn(&mut schemars::SchemaGenerator) -> MethodSchemas,
}

impl std::fmt::Debug for MethodSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MethodSpec")
            .field("name", &self.name)
            .field("plane", &self.plane)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

fn normalize<T: Serialize + DeserializeOwned>(value: &Value) -> Result<Value, serde_json::Error> {
    serde_json::to_value(T::deserialize(value)?)
}

macro_rules! requests {
    ($(
        $marker:ident = $name:literal {
            plane: $plane:ident,
            direction: $direction:ident,
            family: $family:ident,
            activity: $activity:expr,
            capability: $capability:expr,
            abortable: $abortable:expr,
            params: $params:ty,
            response: $response:ty,
            summary: $summary:literal $(,)?
        }
    )+) => {
        $(
            #[doc = concat!("`", $name, "`: ", $summary)]
            #[derive(Clone, Copy, Debug)]
            pub struct $marker;

            impl $marker {
                pub const NAME: &'static str = $name;
            }

            impl RequestMethod for $marker {
                const NAME: &'static str = $name;
                const PLANE: Plane = Plane::$plane;
                type Params = $params;
                type Response = $response;
            }
        )+

        /// Every request method.
        pub static REQUESTS: &[MethodSpec] = &[
            $(
                MethodSpec {
                    name: $name,
                    plane: Plane::$plane,
                    kind: MessageKind::Request,
                    direction: Direction::$direction,
                    family: Family::$family,
                    activity: $activity,
                    capability: $capability,
                    abortable: $abortable,
                    params_type: stringify!($params),
                    response_type: Some(stringify!($response)),
                    summary: $summary,
                    normalize_params: normalize::<$params>,
                    normalize_response: Some(normalize::<$response>),
                    #[cfg(feature = "schema")]
                    schemas: |generator| MethodSchemas {
                        params: generator.subschema_for::<$params>(),
                        response: Some(generator.subschema_for::<$response>()),
                    },
                },
            )+
        ];
    };
}

macro_rules! notifications {
    ($(
        $marker:ident = $name:literal {
            plane: $plane:ident,
            direction: $direction:ident,
            family: $family:ident,
            activity: $activity:expr,
            params: $params:ty,
            summary: $summary:literal $(,)?
        }
    )+) => {
        $(
            #[doc = concat!("`", $name, "`: ", $summary)]
            #[derive(Clone, Copy, Debug)]
            pub struct $marker;

            impl $marker {
                pub const NAME: &'static str = $name;
            }

            impl NotificationMethod for $marker {
                const NAME: &'static str = $name;
                const PLANE: Plane = Plane::$plane;
                type Params = $params;
            }
        )+

        /// Every notification method.
        pub static NOTIFICATIONS: &[MethodSpec] = &[
            $(
                MethodSpec {
                    name: $name,
                    plane: Plane::$plane,
                    kind: MessageKind::Notification,
                    direction: Direction::$direction,
                    family: Family::$family,
                    activity: $activity,
                    capability: None,
                    abortable: false,
                    params_type: stringify!($params),
                    response_type: None,
                    summary: $summary,
                    normalize_params: normalize::<$params>,
                    normalize_response: None,
                    #[cfg(feature = "schema")]
                    schemas: |generator| MethodSchemas {
                        params: generator.subschema_for::<$params>(),
                        response: None,
                    },
                },
            )+
        ];
    };
}

const USE: Option<Activity> = Some(Activity::Use);
const OBSERVATION: Option<Activity> = Some(Activity::Observation);
const FILESYSTEM: Option<Capability> = Some(Capability::Filesystem);
const WRITE: Option<Capability> = Some(Capability::FilesystemWrite);
const PROCESS: Option<Capability> = Some(Capability::Process);
const JOB: Option<Capability> = Some(Capability::Job);

requests! {
    DataInitialize = "initialize" {
        plane: Data, direction: ClientToServer, family: Handshake, activity: OBSERVATION,
        capability: None, abortable: false,
        params: InitializeParams, response: InitializeResponse,
        summary: "Check the protocol version and read the daemon's instance id, capabilities and ceilings.",
    }
    EnvGetIdle = "env/getIdle" {
        plane: Data, direction: ClientToServer, family: Env, activity: OBSERVATION,
        capability: None, abortable: false,
        params: GetIdleParams, response: GetIdleResponse,
        summary: "Report how long the environment has been idle.",
    }
    FsReadFile = "fs/readFile" {
        plane: Data, direction: ClientToServer, family: Filesystem, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: ReadFileParams, response: ReadFileResponse,
        summary: "Read a byte range of a regular file.",
    }
    FsWriteFile = "fs/writeFile" {
        plane: Data, direction: ClientToServer, family: Filesystem, activity: USE,
        capability: WRITE, abortable: false,
        params: WriteFileParams, response: WriteFileResponse,
        summary: "Write a file atomically, optionally conditioned on its version.",
    }
    FsCreateDirectory = "fs/createDirectory" {
        plane: Data, direction: ClientToServer, family: Filesystem, activity: USE,
        capability: WRITE, abortable: false,
        params: CreateDirectoryParams, response: CreateDirectoryResponse,
        summary: "Create a directory.",
    }
    FsGetMetadata = "fs/getMetadata" {
        plane: Data, direction: ClientToServer, family: Filesystem, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: GetMetadataParams, response: GetMetadataResponse,
        summary: "Describe one entry.",
    }
    FsReadDirectory = "fs/readDirectory" {
        plane: Data, direction: ClientToServer, family: Filesystem, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: ReadDirectoryParams, response: ReadDirectoryResponse,
        summary: "List one page of a directory's entries.",
    }
    FsRemove = "fs/remove" {
        plane: Data, direction: ClientToServer, family: Filesystem, activity: USE,
        capability: WRITE, abortable: false,
        params: RemoveParams, response: RemoveResponse,
        summary: "Remove a file, symlink or directory.",
    }
    FsRename = "fs/rename" {
        plane: Data, direction: ClientToServer, family: Filesystem, activity: USE,
        capability: WRITE, abortable: false,
        params: RenameParams, response: RenameResponse,
        summary: "Rename an entry atomically.",
    }
    FsSearchText = "fs/searchText" {
        plane: Data, direction: ClientToServer, family: Filesystem, activity: USE,
        capability: FILESYSTEM, abortable: true,
        params: SearchTextParams, response: SearchTextResponse,
        summary: "Search file contents below a root with a regular expression.",
    }
    FsGlobFiles = "fs/globFiles" {
        plane: Data, direction: ClientToServer, family: Filesystem, activity: USE,
        capability: FILESYSTEM, abortable: true,
        params: GlobFilesParams, response: GlobFilesResponse,
        summary: "List files below a root that match patterns.",
    }
    FsScan = "fs/scan" {
        plane: Data, direction: ClientToServer, family: Filesystem, activity: OBSERVATION,
        capability: FILESYSTEM, abortable: true,
        params: ScanParams, response: ScanResponse,
        summary: "Observe small trees below several roots, each reported on its own.",
    }
    TransferBegin = "transfer/begin" {
        plane: Data, direction: ClientToServer, family: Transfer, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: BeginTransferParams, response: TransferStatus,
        summary: "Start or resume a transfer operation.",
    }
    TransferAdvance = "transfer/advance" {
        plane: Data, direction: ClientToServer, family: Transfer, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: TransferOperationParams, response: TransferStatus,
        summary: "Do a bounded step of scanning or staging work.",
    }
    TransferGetStatus = "transfer/getStatus" {
        plane: Data, direction: ClientToServer, family: Transfer, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: TransferOperationParams, response: TransferStatus,
        summary: "Read an operation's status or receipt.",
    }
    TransferReadInventory = "transfer/readInventory" {
        plane: Data, direction: ClientToServer, family: Transfer, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: ReadTransferInventoryParams, response: ReadTransferInventoryResponse,
        summary: "Read one page of a sealed inventory.",
    }
    TransferAppend = "transfer/append" {
        plane: Data, direction: ClientToServer, family: Transfer, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: AppendTransferInventoryParams, response: TransferStatus,
        summary: "Append incoming inventory entries.",
    }
    TransferListMissing = "transfer/listMissing" {
        plane: Data, direction: ClientToServer, family: Transfer, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: ListTransferMissingParams, response: ListTransferMissingResponse,
        summary: "List digests whose content the daemon still needs.",
    }
    TransferRead = "transfer/read" {
        plane: Data, direction: ClientToServer, family: Transfer, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: ReadTransferContentParams, response: ReadTransferContentResponse,
        summary: "Read captured content by digest.",
    }
    TransferWrite = "transfer/write" {
        plane: Data, direction: ClientToServer, family: Transfer, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: WriteTransferContentParams, response: WriteTransferContentResponse,
        summary: "Upload content for a missing digest.",
    }
    TransferCommit = "transfer/commit" {
        plane: Data, direction: ClientToServer, family: Transfer, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: TransferOperationParams, response: TransferStatus,
        summary: "Finish a ready operation: publish a staged tree, or release a capture.",
    }
    TransferAbort = "transfer/abort" {
        plane: Data, direction: ClientToServer, family: Transfer, activity: USE,
        capability: FILESYSTEM, abortable: false,
        params: TransferOperationParams, response: TransferStatus,
        summary: "Abandon an operation and discard its staging.",
    }
    ProcessStart = "process/start" {
        plane: Data, direction: ClientToServer, family: Process, activity: USE,
        capability: PROCESS, abortable: false,
        params: StartProcessParams, response: StartProcessResponse,
        summary: "Start a process under a caller-chosen handle.",
    }
    ProcessRead = "process/read" {
        plane: Data, direction: ClientToServer, family: Process, activity: USE,
        capability: PROCESS, abortable: true,
        params: ReadProcessParams, response: ReadProcessResponse,
        summary: "Wait for and read a page of a process's output and its state.",
    }
    ProcessWrite = "process/write" {
        plane: Data, direction: ClientToServer, family: Process, activity: USE,
        capability: PROCESS, abortable: false,
        params: WriteProcessParams, response: WriteProcessResponse,
        summary: "Queue bytes for a process's input pipe or PTY.",
    }
    ProcessTerminate = "process/terminate" {
        plane: Data, direction: ClientToServer, family: Process, activity: USE,
        capability: PROCESS, abortable: false,
        params: TerminateProcessParams, response: TerminateProcessResponse,
        summary: "Interrupt, terminate or kill a process's group.",
    }
    JobStart = "job/start" {
        plane: Data, direction: ClientToServer, family: Job, activity: USE,
        capability: JOB, abortable: false,
        params: StartJobsParams, response: StartJobsResponse,
        summary: "Start jobs, or replay an identical earlier start.",
    }
    JobRead = "job/read" {
        plane: Data, direction: ClientToServer, family: Job, activity: OBSERVATION,
        capability: JOB, abortable: true,
        params: ReadJobsParams, response: ReadJobsResponse,
        summary: "Wait for and read jobs' summaries and output pages.",
    }
    JobCancel = "job/cancel" {
        plane: Data, direction: ClientToServer, family: Job, activity: USE,
        capability: JOB, abortable: false,
        params: CancelJobsParams, response: CancelJobsResponse,
        summary: "Cancel jobs and optionally their dependents.",
    }
    ControllerInitialize = "initialize" {
        plane: Controller, direction: ClientToServer, family: Handshake, activity: None,
        capability: None, abortable: false,
        params: ControllerInitializeParams, response: ControllerInitializeResponse,
        summary: "Check the protocol version and read the controller's capabilities.",
    }
    TemplateList = "template/list" {
        plane: Controller, direction: ClientToServer, family: Template, activity: None,
        capability: None, abortable: false,
        params: ListTemplatesParams, response: ListTemplatesResponse,
        summary: "List the templates a universe may create targets from.",
    }
    TargetCreate = "target/create" {
        plane: Controller, direction: ClientToServer, family: Target, activity: None,
        capability: None, abortable: false,
        params: CreateTargetParams, response: TargetResponse,
        summary: "Create the owner's target, or observe the one it has.",
    }
    TargetGet = "target/get" {
        plane: Controller, direction: ClientToServer, family: Target, activity: None,
        capability: None, abortable: false,
        params: GetTargetParams, response: TargetResponse,
        summary: "Observe a target without changing it.",
    }
    TargetSetPower = "target/setPower" {
        plane: Controller, direction: ClientToServer, family: Target, activity: None,
        capability: Some(Capability::Power), abortable: false,
        params: SetTargetPowerParams, response: TargetResponse,
        summary: "Move a target to a power state.",
    }
    TargetClose = "target/close" {
        plane: Controller, direction: ClientToServer, family: Target, activity: None,
        capability: None, abortable: false,
        params: CloseTargetParams, response: TargetResponse,
        summary: "Destroy the owner's target and everything it holds.",
    }
    IngressEnsure = "ingress/ensure" {
        plane: Controller, direction: ClientToServer, family: Ingress, activity: None,
        capability: Some(Capability::Ingress), abortable: false,
        params: EnsureIngressParams, response: TargetResponse,
        summary: "Route public ingress to a target.",
    }
    IngressRemove = "ingress/remove" {
        plane: Controller, direction: ClientToServer, family: Ingress, activity: None,
        capability: Some(Capability::Ingress), abortable: false,
        params: RemoveIngressParams, response: TargetResponse,
        summary: "Stop routing public ingress to a target that stays.",
    }
    RegistrationRegister = "registration/register" {
        plane: Registration, direction: ClientToServer, family: Registration, activity: None,
        capability: None, abortable: false,
        params: RegisterParams, response: RegisterResponse,
        summary: "Prove the daemon identity and be admitted to its environment.",
    }
    RegistrationOpenStream = "registration/openStream" {
        plane: Registration, direction: ServerToClient, family: Registration, activity: None,
        capability: None, abortable: false,
        params: OpenStreamParams, response: OpenStreamResponse,
        summary: "Ask the daemon to dial one reverse stream.",
    }
}

notifications! {
    CancelRequest = "$/cancelRequest" {
        plane: Data, direction: ClientToServer, family: Envelope, activity: OBSERVATION,
        params: CancelRequestParams,
        summary: "Abort a pending abortable request.",
    }
    RegistrationChallenge = "registration/challenge" {
        plane: Registration, direction: ServerToClient, family: Registration, activity: None,
        params: ChallengeParams,
        summary: "The frozen first frame: the gateway's version and nonce.",
    }
    RegistrationGoodbye = "registration/goodbye" {
        plane: Registration, direction: ClientToServer, family: Registration, activity: None,
        params: GoodbyeParams,
        summary: "The daemon is stopping deliberately.",
    }
    RegistrationClosed = "registration/closed" {
        plane: Registration, direction: ServerToClient, family: Registration, activity: None,
        params: ClosedParams,
        summary: "The gateway ends the session for good.",
    }
}

/// Every method of every plane: requests, then notifications.
pub fn methods() -> impl Iterator<Item = &'static MethodSpec> {
    REQUESTS.iter().chain(NOTIFICATIONS.iter())
}

/// Every method of one plane.
pub fn plane_methods(plane: Plane) -> impl Iterator<Item = &'static MethodSpec> {
    methods().filter(move |spec| spec.plane == plane)
}

/// The method of this name on this plane.
pub fn find(plane: Plane, name: &str) -> Option<&'static MethodSpec> {
    plane_methods(plane).find(|spec| spec.name == name)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn method_names_are_unique_per_plane() {
        let mut seen = BTreeSet::new();
        for spec in methods() {
            assert!(
                seen.insert((spec.plane.as_str(), spec.name)),
                "duplicate {} on {}",
                spec.name,
                spec.plane
            );
        }
    }

    #[test]
    fn activity_is_declared_for_data_methods_only() {
        for spec in methods() {
            assert_eq!(
                spec.activity.is_some(),
                spec.plane == Plane::Data,
                "{}",
                spec.name
            );
        }
        assert_eq!(
            find(Plane::Data, FsScan::NAME).unwrap().activity,
            Some(Activity::Observation)
        );
    }
}
