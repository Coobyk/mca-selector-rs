use std::path::Path;

/// Errors produced while reading Anvil data.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error on {path}: {source}")]
    Io {
        path: Box<Path>,
        source: std::io::Error,
    },

    #[error("region file too short: expected at least {expected} bytes, got {got} ({path})")]
    TooShort {
        path: Box<Path>,
        expected: usize,
        got: usize,
    },

    #[error("chunk {x},{z} not present in region")]
    MissingChunk { x: i32, z: i32 },

    #[error("chunk sector offset {offset} out of bounds in {path}")]
    BadOffset { offset: u32, path: Box<Path> },

    #[error("chunk {x},{z} uses external storage, which is not supported")]
    ExternalStorage { x: i32, z: i32 },

    #[error("unsupported chunk compression type {0}")]
    UnknownCompression(u8),

    #[error("chunk payload length {length} out of bounds in {path}")]
    BadLength { length: u32, path: Box<Path> },

    #[error("nbt error: {0}")]
    Nbt(#[from] fastnbt::error::Error),

    #[error("region file name {0:?} does not match r.<x>.<z>.mca")]
    BadRegionName(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
