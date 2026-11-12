use mc_core::mca::McaFile;
use mc_core::nbt::Tag;
use std::path::PathBuf;

fn main() {
    let cfg = mc_core::colors::cfg_xaero();
    for path in std::env::args().skip(1) {
        let mut m = McaFile::new(PathBuf::from(&path)).unwrap();
        m.load(true).unwrap();
        for chunk in m.chunks.iter().flatten() {
            let Some(data) = chunk.data.as_ref() else { continue };
            // sections may be at root or under Level
            let secs = data
                .get("sections")
                .or_else(|| data.get("Level").and_then(|t| t.as_compound()?.get("sections")))
                .or_else(|| data.get("Level").and_then(|t| t.as_compound()?.get("Sections")))
                .and_then(Tag::as_list);
            let Some((_, secs)) = secs else { continue };
            for sec in secs.iter().filter_map(Tag::as_compound) {
                let pal = sec
                    .get("block_states")
                    .and_then(|t| t.as_compound()?.get("palette"))
                    .or_else(|| sec.get("Palette"))
                    .and_then(Tag::as_list);
                let Some((_, pal)) = pal else { continue };
                for e in pal.iter() {
                    let name = mc_core::chunk::palette_entry_name(e).to_string();
                    if !name.contains("mushroom") { continue; }
                    let props = e.as_compound().and_then(|c| {
                        c.get("Properties").or_else(|| c.get("properties")).and_then(Tag::as_compound)
                    });
                    let st = cfg.states.get_state(props);
                    let bc = cfg.get_color_state(&name, "minecraft:plains", st.as_ref());
                    println!("{name} props={:?} color={:06x}", props, bc.color);
                }
            }
        }
    }
}
