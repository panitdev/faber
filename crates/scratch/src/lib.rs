//! Scratch: a per-session environment, run as a container in tenantd that
//! starts when a session first needs it.

pub mod compute;
pub mod fuse;
pub mod mount;
pub mod session;
pub mod volume;

pub use compute::{Endpoint, TenantdConfig, TlsEndpoint};
pub use mount::{Entry, ErrorLog, FsError, Kind, MountBackend, MountError, MountTable};
pub use session::{
    Call, IDLE_TIMEOUT, ScratchConfig, ScratchError, ScratchManager, Session, TMP_RETENTION,
};
pub use volume::TenantdVolume;
