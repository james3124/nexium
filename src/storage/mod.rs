//! Filesystem-backed browser storage: profiles, cookies, local storage,
//! history and sessions.

pub mod filesystem;
pub mod profiles;

pub use filesystem::{FsStore, read_locked, write_atomic};
pub use profiles::{HistoryEntry, Profile, ProfilePreferences, origin_file};
