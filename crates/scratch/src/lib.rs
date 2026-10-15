//! Scratch: a per-session environment that starts as a virtual shell and
//! promotes to a gVisor sandbox when a command needs real processes.

pub(crate) mod beneath;
pub mod compute;
pub mod decide;
pub mod files;
pub mod fuse;
pub mod layer;
pub mod mount;
pub mod quota;
pub mod session;
pub mod shell;
pub mod vfs;

pub use layer::Layer;
pub use mount::{Entry, ErrorLog, FsError, Kind, MountBackend, MountError, MountTable};
pub use quota::{Quota, QuotaExceeded, Usage};
pub use session::{Call, Mode, ScratchConfig, ScratchError, ScratchManager, Session};
pub use vfs::ScratchFs;
