//! Plugin marketplace: index fetch + sha256 verification + reuse of install_ocplugin to complete installation.
//! See the install flow in docs/plugin-manifest.md (Phase 6).
//!
//! Data sources:
//! - a remote index.json pointed to by OPENCAPX_MARKETPLACE_URL (optional)
//! - otherwise ~/.opencapx/marketplace/seed.json as the seed (placed locally by the developer)
//!
//! index.json format:
//! ```json
//! {
//!   "entries": [
//!     {
//!       "id": "com.opencapx.echo-vision",
//!       "name": "Echo Vision",
//!       "version": "0.1.0",
//!       "description": "...",
//!       "downloadUrl": "file:///.../echo-0.1.0.ocplugin",
//!       "sha256": "<hex>",
//!       "capabilities": ["image.analyze"],
//!       "permissions": ["image.read"]
//!     }
//!   ]
//! }
//! ```

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

mod download;
mod entry;
mod index;
mod version;

pub use download::*;
pub use entry::*;
pub use index::*;
pub use version::*;

#[cfg(test)]
mod tests;
