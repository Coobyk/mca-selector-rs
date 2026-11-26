//! mc-core: faithful port of net.querz.mcaselector's non-UI code.

#[cfg(not(target_arch = "wasm32"))]
pub mod batch;
pub mod bitset;
#[cfg(not(target_arch = "wasm32"))]
pub mod cache;
pub mod changer;
pub mod chunk;
pub mod colors;
pub mod files;
#[cfg(not(target_arch = "wasm32"))]
pub mod filter;
#[cfg(not(target_arch = "wasm32"))]
pub mod filter_ops;
#[cfg(not(target_arch = "wasm32"))]
pub mod jobs;
pub mod mca;
pub mod nbt;
pub mod overlay;
pub mod point;
pub mod registry;
pub mod render;
pub mod selection;
pub mod structures;
pub mod valuetext;
pub mod version;
