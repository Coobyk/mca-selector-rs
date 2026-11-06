//! mc-core: Anvil region parsing, chunk model and map rendering for
//! mca-selector-rs.
//!
//! The crate is deliberately split into layers:
//!
//! - [`region`]: raw `.mca` region file access (header, sector table,
//!   decompression).
//! - [`chunk`]: the chunk NBT model for modern Anvil (1.18+) plus the packed
//!   bit helpers used by block palettes and heightmaps.
//! - [`world`]: locating dimensions of a save folder and listing regions.
//! - [`map`]: block colors and chunk tile rendering for the world map.

pub mod chunk;
pub mod error;
pub mod export;
pub mod map;
pub mod packed;
pub mod region;
pub mod region_writer;
pub mod world;

pub use error::{Error, Result};
