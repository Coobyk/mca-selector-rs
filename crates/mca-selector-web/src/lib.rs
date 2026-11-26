#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Uint8Array};
use mc_core::mca::{filter_region_bytes, region_chunk_set, McaFile};
use mc_core::point::Point2i;
use mc_core::render::{generate_image, DimKind, Margins, RenderSettings, TILE_SIZE};
use mc_core::selection::ChunkSet;
use wasm_bindgen::prelude::*;
use wasm_bindgen::Clamped;
use web_sys::ImageData;

fn js_error(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

#[wasm_bindgen]
pub fn occupied_chunks(header: &[u8]) -> Result<Vec<u8>, JsValue> {
    Ok(region_chunk_set(header).map_err(js_error)?.to_bytes().to_vec())
}

#[wasm_bindgen]
pub fn render_region(
    name: &str,
    data: &[u8],
    dimension: &str,
    scale: usize,
    sidecars: Array,
) -> Result<ImageData, JsValue> {
    if !matches!(scale, 1 | 2 | 4 | 8 | 16 | 32) {
        return Err(js_error("unsupported tile scale"));
    }
    let sidecars = sidecars.iter().map(|entry| {
        let entry = Array::from(&entry);
        let x = entry.get(0).as_f64().unwrap_or_default() as i32;
        let z = entry.get(1).as_f64().unwrap_or_default() as i32;
        (Point2i::new(x, z), Uint8Array::new(&entry.get(2)).to_vec())
    }).collect();
    let region = McaFile::from_bytes(name.into(), data, true, &sidecars).map_err(js_error)?;
    let settings = RenderSettings {
        dim: match dimension {
            "nether" => DimKind::Nether,
            "end" => DimKind::End,
            _ => DimKind::Overworld,
        },
        ..RenderSettings::default()
    };
    let pixels = generate_image(&region, scale as i32, &settings, &[0; 256], &Margins::default())
        .ok_or_else(|| js_error("region did not render"))?;
    let rgba: Vec<u8> = pixels.into_iter().flat_map(|pixel| {
        let [a, r, g, b] = pixel.to_be_bytes();
        [r, g, b, a]
    }).collect();
    ImageData::new_with_u8_clamped_array_and_sh(Clamped(&rgba), (TILE_SIZE / scale) as u32,
        (TILE_SIZE / scale) as u32)
}

#[wasm_bindgen]
pub fn keep_chunks(data: &[u8], keep: &[u8]) -> Result<Array, JsValue> {
    if keep.len() != 128 {
        return Err(js_error("expected a 1024-bit chunk mask"));
    }
    let (bytes, external) = filter_region_bytes(data, &ChunkSet::from_bytes(keep)).map_err(js_error)?;
    let out = Array::new();
    out.push(&Uint8Array::from(bytes.as_slice()));
    out.push(&Uint8Array::from(external.to_bytes().as_slice()));
    Ok(out)
}

#[wasm_bindgen]
pub fn zip_crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}
