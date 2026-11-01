//! Domain types and the versioned JSON contract shared by every gigsift surface.
//!
//! The CLI, Raycast extension, and MCP server all speak these types. Anything
//! that changes their serialized shape in a breaking way must bump
//! [`SCHEMA_VERSION`] and regenerate `/schema`.

pub mod candidate;
pub mod probe;
pub mod report;
pub mod safety;
pub mod size;

pub use candidate::*;
pub use report::*;
pub use safety::{ProtectedReason, protected_reason};

/// Version of the JSON contract. Emitted as `schema_version` on every response.
pub const SCHEMA_VERSION: u32 = 1;
