//! Domain types and the versioned JSON contract shared by every gigsift surface.
//!
//! The CLI, Raycast extension, and MCP server all speak these types. Anything
//! that changes their serialized shape in a breaking way must bump
//! [`SCHEMA_VERSION`] and regenerate `/schema`.

pub mod age;
pub mod apply;
pub mod candidate;
pub mod plan;
pub mod probe;
pub mod redact;
pub mod report;
pub mod safety;
pub mod size;
pub mod trends;

pub use apply::*;
pub use candidate::*;
pub use plan::*;
pub use redact::{PathSharing, redact};
pub use report::*;
pub use safety::{ProtectedReason, protected_reason};
pub use trends::*;

/// Version of the JSON contract. Emitted as `schema_version` on every response.
pub const SCHEMA_VERSION: u32 = 1;
