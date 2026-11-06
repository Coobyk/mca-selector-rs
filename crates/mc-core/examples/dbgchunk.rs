//! Debug one chunk's palette representation.

use fastnbt::Value;
use mc_core::region::Region;

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: dbgchunk <region.mca>");
    let region = Region::open(&path)?;
    for cz in 0..32u8 {
        for cx in 0..32u8 {
            let Ok(bytes) = region.chunk_data(cx, cz) else {
                continue;
            };
            let Ok(Value::Compound(root)) = fastnbt::from_bytes::<Value>(&bytes) else {
                continue;
            };
            let Some(Value::List(sections)) = root.get("sections") else {
                continue;
            };
            let mut first = None;
            for s in sections {
                if let Value::Compound(c) = s {
                    if c.contains_key("block_states") {
                        first = Some(c.clone());
                        break;
                    }
                }
            }
            let Some(first) = first else {
                continue;
            };
            let Some(Value::Compound(bs)) = first.get("block_states") else {
                continue;
            };
            let palette = bs.get("palette");
            println!("chunk ({cx},{cz}) palette repr:");
            match palette {
                Some(Value::List(items)) => {
                    println!("  list of {} items, first 3:", items.len());
                    for item in items.iter().take(3) {
                        println!("    {item:?}");
                    }
                }
                Some(Value::String(s)) => println!("  plain string: {s:?}"),
                other => println!("  other: {other:?}"),
            }
            return Ok(());
        }
    }
    println!("no chunk with block_states found");
    Ok(())
}
