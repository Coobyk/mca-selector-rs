//! Port of selection.ChunkSet + selection.Selection.

use crate::point::Point2i;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 1024-bit set of chunk indexes inside a region.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkSet {
    words: [u64; 16],
    set_bits: u16,
}

impl ChunkSet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(&mut self, i: usize) {
        if !self.get(i) {
            self.set_bits += 1;
        }
        self.words[i >> 6] |= 1u64 << (i & 63);
    }
    pub fn clear(&mut self, i: usize) {
        if self.get(i) {
            self.set_bits -= 1;
        }
        self.words[i >> 6] &= !(1u64 << (i & 63));
    }
    pub fn clear_all(&mut self) {
        self.words = [0; 16];
        self.set_bits = 0;
    }
    pub fn get(&self, i: usize) -> bool {
        (self.words[i >> 6] >> (i & 63)) & 1 == 1
    }
    pub fn size(&self) -> usize {
        self.set_bits as usize
    }
    pub fn is_empty(&self) -> bool {
        self.set_bits == 0
    }
    pub fn fill(&mut self) {
        self.words = [u64::MAX; 16];
        self.set_bits = 1024;
    }
    pub fn flip(&self) -> ChunkSet {
        let mut c = self.clone();
        for w in &mut c.words {
            *w = !*w;
        }
        c.set_bits = 1024 - self.set_bits;
        c
    }
    pub fn and(&mut self, o: &ChunkSet) {
        let mut n = 0u16;
        for i in 0..16 {
            self.words[i] &= o.words[i];
            n += self.words[i].count_ones() as u16;
        }
        self.set_bits = n;
    }
    pub fn and_not(&mut self, o: &ChunkSet) {
        let mut n = 0u16;
        for i in 0..16 {
            self.words[i] &= !o.words[i];
            n += self.words[i].count_ones() as u16;
        }
        self.set_bits = n;
    }
    pub fn or(&mut self, o: &ChunkSet) {
        let mut n = 0u16;
        for i in 0..16 {
            self.words[i] |= o.words[i];
            n += self.words[i].count_ones() as u16;
        }
        self.set_bits = n;
    }
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..1024).filter(|&i| self.get(i))
    }
    pub fn to_bytes(&self) -> [u8; 128] {
        let mut out = [0u8; 128];
        for (i, w) in self.words.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
        }
        out
    }
    pub fn from_bytes(b: &[u8]) -> ChunkSet {
        let mut cs = ChunkSet::new();
        for i in 0..16 {
            cs.words[i] = u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
        }
        cs.set_bits = cs.words.iter().map(|w| w.count_ones() as u16).sum();
        cs
    }
}

/// region-asLong -> selected chunks (None = whole region selected).
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub selection: HashMap<i64, Option<ChunkSet>>,
    pub inverted: bool,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }
    /// an "everything selected" selection: inverted with no exclusions
    /// (the encoding for Selection = whole world / no bound).
    pub fn all() -> Self {
        Self {
            inverted: true,
            ..Self::default()
        }
    }
    pub fn is_inverted(&self) -> bool {
        self.inverted
    }
    pub fn is_empty(&self) -> bool {
        !self.inverted && self.selection.is_empty()
    }
    /// count() — number of selected chunks (meaningless when inverted).
    pub fn count(&self) -> usize {
        self.selection
            .values()
            .map(|c| c.as_ref().map(|c| c.size()).unwrap_or(1024))
            .sum()
    }

    fn chunk_selected(&self, region: i64, chunk: usize) -> bool {
        match self.selection.get(&region) {
            Some(set) => set.as_ref().map(|c| c.get(chunk)).unwrap_or(true) != self.inverted,
            None => self.inverted,
        }
    }

    pub fn is_chunk_selected(&self, chunk: Point2i) -> bool {
        self.chunk_selected(chunk.chunk_to_region().as_long(), chunk.as_chunk_index())
    }
    pub fn is_region_selected(&self, region: i64) -> bool {
        if self.inverted {
            !self.selection.contains_key(&region)
        } else {
            self.selection.get(&region).map(|v| v.is_none()).unwrap_or(false)
        }
    }
    pub fn is_any_chunk_in_region_selected(&self, region: i64) -> bool {
        if self.inverted {
            self.selection.get(&region).map(|v| v.is_some()).unwrap_or(true)
        } else {
            self.selection.contains_key(&region)
        }
    }
    /// selected chunk bits for a region (None => all selected / inverted-all).
    pub fn selected_chunks(&self, region: Point2i) -> Option<Option<ChunkSet>> {
        if self.inverted {
            match self.selection.get(&region.as_long()) {
                Some(v) => Some(Some(
                    v.clone().map(|c| c.flip()).unwrap_or_default(),
                )),
                // inverted + unmarked region = all chunks selected (whole-region
                // sentinel, matching is_chunk_selected)
                None => Some(None),
            }
        } else {
            match self.selection.get(&region.as_long()) {
                Some(v) => Some(v.clone()),
                None => Some(Some(ChunkSet::new())),
            }
        }
    }

    pub fn add_chunk(&mut self, chunk: Point2i) {
        let region = chunk.chunk_to_region().as_long();
        let idx = chunk.as_chunk_index();
        if self.inverted {
            if let Some(v) = self.selection.get_mut(&region) {
                match v {
                    None => {
                        let mut cs = ChunkSet::new();
                        cs.fill();
                        cs.clear(idx);
                        *v = Some(cs);
                    }
                    Some(cs) => {
                        cs.clear(idx);
                        if cs.is_empty() {
                            self.selection.remove(&region);
                        }
                    }
                }
            }
        } else if let Some(v) = self.selection.get_mut(&region) {
            if let Some(cs) = v {
                cs.set(idx);
                if cs.size() == 1024 {
                    *v = None;
                }
            }
        } else {
            let mut cs = ChunkSet::new();
            cs.set(idx);
            self.selection.insert(region, Some(cs));
        }
    }

    /// returns number of chunks added
    pub fn add_region(&mut self, region: i64) -> usize {
        if self.inverted {
            if let Some(v) = self.selection.remove(&region) {
                return v.map(|c| c.size()).unwrap_or(1024);
            }
            0
        } else if let Some(v) = self.selection.get_mut(&region) {
            match v {
                None => 0,
                Some(cs) => {
                    let n = 1024 - cs.size();
                    *v = None;
                    n
                }
            }
        } else {
            self.selection.insert(region, None);
            1024
        }
    }

    /// Write a region entry directly, keeping the encoding's invariants:
    /// a full 1024-bit set normalizes to the whole-region `None` sentinel
    /// and an empty set removes the entry — the same states the mutators
    /// (add_chunk / add_region / remove_chunk) can produce.
    pub fn set_region(&mut self, region: i64, chunks: Option<ChunkSet>) {
        match chunks {
            None => {
                self.selection.insert(region, None);
            }
            Some(cs) if cs.size() >= 1024 => {
                self.selection.insert(region, None);
            }
            Some(cs) if cs.is_empty() => {
                self.selection.remove(&region);
            }
            Some(cs) => {
                self.selection.insert(region, Some(cs));
            }
        }
    }

    /// Iterate (region-asLong, entry) pairs. The entry is the raw map
    /// encoding (None = whole region); for inverted selections it is the
    /// *unselected* set — use `selected_chunks`/`is_chunk_selected` for
    /// resolved reads.
    pub fn regions(&self) -> impl Iterator<Item = (i64, Option<&ChunkSet>)> + '_ {
        self.selection.iter().map(|(&k, v)| (k, v.as_ref()))
    }

    /// returns number of chunks removed
    pub fn remove_region(&mut self, region: i64) -> usize {
        if self.inverted {
            if let Some(v) = self.selection.get_mut(&region) {
                match v {
                    None => 0,
                    Some(cs) => {
                        let n = 1024 - cs.size();
                        *v = None;
                        n
                    }
                }
            } else {
                self.selection.insert(region, None);
                1024
            }
        } else if let Some(v) = self.selection.remove(&region) {
            v.map(|c| c.size()).unwrap_or(1024)
        } else {
            0
        }
    }

    pub fn remove_chunk(&mut self, chunk: Point2i) {
        let region = chunk.chunk_to_region().as_long();
        let idx = chunk.as_chunk_index();
        if self.inverted {
            if let Some(v) = self.selection.get_mut(&region) {
                if let Some(cs) = v {
                    cs.set(idx);
                    if cs.size() == 1024 {
                        *v = None;
                    }
                }
            } else {
                let mut cs = ChunkSet::new();
                cs.set(idx);
                self.selection.insert(region, Some(cs));
            }
        } else if let Some(v) = self.selection.get_mut(&region) {
            match v {
                None => {
                    let mut cs = ChunkSet::new();
                    cs.fill();
                    cs.clear(idx);
                    *v = Some(cs);
                }
                Some(cs) => {
                    cs.clear(idx);
                    if cs.is_empty() {
                        self.selection.remove(&region);
                    }
                }
            }
        }
    }

    /// port of Selection.addRadius(radius, bounds): for every selected chunk
    /// (or the edges of fully-selected regions), also mark all chunks within
    /// a circular radius ((radius + 0.3)^2) that are selected in `bounds`.
    pub fn add_radius(&mut self, radius: i32, bounds: Option<&Selection>) {
        let entries: Vec<(i64, Option<ChunkSet>)> =
            self.selection.iter().map(|(k, v)| (*k, v.clone())).collect();
        for (key, cs) in entries {
            let base = Point2i::from_long(key).region_to_chunk();
            match cs {
                None => {
                    for x in 0..32 {
                        self.add_radius_at(Point2i::new(x, 0).add_p(base), radius, bounds);
                        self.add_radius_at(Point2i::new(x, 31).add_p(base), radius, bounds);
                    }
                    for z in 1..31 {
                        self.add_radius_at(Point2i::new(0, z).add_p(base), radius, bounds);
                        self.add_radius_at(Point2i::new(31, z).add_p(base), radius, bounds);
                    }
                }
                Some(cs) => {
                    for i in cs.iter() {
                        let center = Point2i::from_chunk_index(i as i32).add_p(base);
                        self.add_radius_at(center, radius, bounds);
                    }
                }
            }
        }
    }

    fn add_radius_at(&mut self, center: Point2i, radius: i32, bounds: Option<&Selection>) {
        let min = center.sub_v(radius, radius);
        let max = center.add(radius, radius);
        let r2 = (radius as f64 + 0.3) * (radius as f64 + 0.3);
        for x in min.x..=max.x {
            for z in min.z..=max.z {
                let dx = (x - center.x) as f64;
                let dz = (z - center.z) as f64;
                let inside = bounds.map(|b| b.is_chunk_selected(Point2i::new(x, z))).unwrap_or(true);
                if inside && dx * dx + dz * dz <= r2 {
                    self.add_chunk(Point2i::new(x, z));
                }
            }
        }
    }

    pub fn invert_all(&mut self) {
        let keys: Vec<i64> = self.selection.keys().copied().collect();
        for k in keys {
            match self.selection.get(&k).unwrap() {
                None => {
                    self.selection.remove(&k);
                }
                Some(cs) => {
                    let f = cs.flip();
                    self.selection.insert(k, Some(f));
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.selection.clear();
        self.inverted = false;
    }

    /// merge(other) — union of selections (Selection.merge).
    pub fn merge(&mut self, other: &Selection) {
        if !self.inverted && !other.inverted {
            for (r, v) in &other.selection {
                match self.selection.get_mut(r) {
                    Some(mine) => {
                        *mine = cs_add(mine.clone(), v.clone());
                    }
                    None => {
                        self.selection.insert(*r, v.clone());
                    }
                }
            }
        } else if self.inverted && !other.inverted {
            for (r, v) in &other.selection {
                if let Some(mine) = self.selection.get_mut(r) {
                    let res = cs_sub(mine.clone(), v.clone());
                    if res.is_empty() {
                        self.selection.remove(r);
                    } else {
                        *mine = Some(res);
                    }
                }
            }
        } else if !self.inverted {
            for (r, v) in &other.selection {
                match self.selection.get_mut(r) {
                    Some(mine) => {
                        let res = cs_sub(v.clone(), mine.clone());
                        if res.is_empty() {
                            self.selection.remove(r);
                        } else {
                            *mine = Some(res);
                        }
                    }
                    None => {
                        self.selection.insert(*r, v.clone());
                    }
                }
            }
            self.selection.retain(|k, _| other.selection.contains_key(k));
            self.inverted = true;
        } else {
            // both inverted
            let keys: Vec<i64> = self.selection.keys().copied().collect();
            for r in keys {
                if !other.selection.contains_key(&r) {
                    self.selection.remove(&r);
                } else {
                    let union = cs_union(self.selection.get(&r).unwrap().clone(), other.selection.get(&r).unwrap().clone());
                    if union.as_ref().map(|u| u.is_empty()).unwrap_or(false) {
                        self.selection.remove(&r);
                    } else {
                        self.selection.insert(r, union);
                    }
                }
            }
        }
    }

    /// CSV export/import (saveToFile / readFromFile).
    /// min/max chunk bounds across all selected chunks (inclusive).
    pub fn bounds(&self) -> Option<(Point2i, Point2i)> {
        let mut min: Option<Point2i> = None;
        let mut max: Option<Point2i> = None;
        for (&rl, cs) in &self.selection {
            let base = Point2i::from_long(rl).region_to_chunk();
            match cs {
                Some(c) => {
                    for i in c.iter() {
                        let p = base.add(i as i32 % 32, i as i32 / 32);
                        min = Some(min.map_or(p, |m: Point2i| Point2i::new(m.x.min(p.x), m.z.min(p.z))));
                        max = Some(max.map_or(p, |m: Point2i| Point2i::new(m.x.max(p.x), m.z.max(p.z))));
                    }
                }
                None => {
                    for i in 0..1024usize {
                        let p = base.add((i % 32) as i32, (i / 32) as i32);
                        min = Some(min.map_or(p, |m: Point2i| Point2i::new(m.x.min(p.x), m.z.min(p.z))));
                        max = Some(max.map_or(p, |m: Point2i| Point2i::new(m.x.max(p.x), m.z.max(p.z))));
                    }
                }
            }
        }
        Some((min?, max?))
    }

    pub fn save_csv(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut out = String::new();
        if self.inverted {
            out.push_str("inverted\n");
        }
        for (r, v) in &self.selection {
            let region = Point2i::from_long(*r);
            match v {
                None => {
                    out.push_str(&format!("{};{}\n", region.x, region.z));
                }
                Some(cs) => {
                    for i in cs.iter() {
                        let c = Point2i::from_chunk_index(i as i32).add_p(region.region_to_chunk());
                        out.push_str(&format!("{};{};{};{}\n", region.x, region.z, c.x, c.z));
                    }
                }
            }
        }
        std::fs::write(path, out)
    }

    pub fn read_csv(path: &std::path::Path) -> std::io::Result<Selection> {
        let text = std::fs::read_to_string(path)?;
        let mut sel = Selection::new();
        for (num, line) in text.lines().enumerate() {
            let num = num + 1;
            if num == 1 && line == "inverted" {
                sel.inverted = true;
                continue;
            }
            let e: Vec<&str> = line.split(';').collect();
            if e.len() != 2 && e.len() != 4 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid coordinate format in line {num}"),
                ));
            }
            let x: i32 = e[0].parse().map_err(|_| bad_line(num))?;
            let z: i32 = e[1].parse().map_err(|_| bad_line(num))?;
            let region = Point2i::new(x, z);
            if e.len() == 4 {
                let cx: i32 = e[2].parse().map_err(|_| bad_line(num))?;
                let cz: i32 = e[3].parse().map_err(|_| bad_line(num))?;
                let chunk = Point2i::new(cx, cz);
                if chunk.chunk_to_region() != region {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("chunk {chunk} is not in region {region} in line {num}"),
                    ));
                }
                sel.add_chunk(chunk);
            } else {
                sel.add_region(region.as_long());
            }
        }
        Ok(sel)
    }
}

fn bad_line(num: usize) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("failed to read coordinates in line {num}"),
    )
}

fn cs_add(a: Option<ChunkSet>, b: Option<ChunkSet>) -> Option<ChunkSet> {
    match (a, b) {
        (None, _) | (_, None) => None,
        (Some(mut x), Some(y)) => {
            x.or(&y);
            if x.size() == 1024 {
                None
            } else {
                Some(x)
            }
        }
    }
}

/// upstream Selection.subtract — `a \ b` over *stored* sets. In the merge
/// branches that call this, `a` is an unselected-set of an inverted
/// selection and `b` a selected-set: `None` for `a` means all-unselected
/// (result = complement of b), `None` for `b` means all-selected
/// (a \ all = empty).
fn cs_sub(a: Option<ChunkSet>, b: Option<ChunkSet>) -> ChunkSet {
    match (a, b) {
        (None, b) => b.map(|y| y.flip()).unwrap_or_default(),
        (Some(_), None) => ChunkSet::new(),
        (Some(mut x), Some(y)) => {
            x.and_not(&y);
            x
        }
    }
}

/// upstream Selection.union (both-inverted merge): stored sets are
/// *unselected* sets, so merging selected sets unions to an *intersection*
/// of unselected sets. `None` (all-unselected) absorbs to the other side.
fn cs_union(a: Option<ChunkSet>, b: Option<ChunkSet>) -> Option<ChunkSet> {
    match (a, b) {
        (Some(mut x), Some(y)) => {
            x.and(&y);
            Some(x)
        }
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(idxs: &[usize]) -> ChunkSet {
        let mut c = ChunkSet::new();
        for &i in idxs {
            c.set(i);
        }
        c
    }

    fn region(x: i32, z: i32) -> i64 {
        Point2i::new(x, z).as_long()
    }

    /// absolute chunk Point2i for region (0,0), in-region index i.
    fn chunk(i: i32) -> Point2i {
        Point2i::from_chunk_index(i)
    }

    #[test]
    fn chunkset_bit_ops() {
        let mut a = cs(&[0, 1, 5]);
        let b = cs(&[1, 2]);
        a.and(&b);
        assert_eq!(a, cs(&[1]));
        let mut a = cs(&[0, 1, 5]);
        a.and_not(&b);
        assert_eq!(a, cs(&[0, 5]));
        let mut a = cs(&[0]);
        a.or(&b);
        assert_eq!(a, cs(&[0, 1, 2]));
        assert_eq!(cs(&[0, 1]).flip().size(), 1022);
        assert_eq!(cs(&[3]).to_bytes(), ChunkSet::from_bytes(&cs(&[3]).to_bytes()).to_bytes());
        assert_eq!(cs(&[3, 1000]).iter().collect::<Vec<_>>(), vec![3, 1000]);
    }

    // upstream Selection.subtract: None(a) = all-unselected, None(b) =
    // all-selected.
    #[test]
    fn cs_sub_table() {
        assert_eq!(cs_sub(None, Some(cs(&[0, 1]))).size(), 1022);
        assert!(cs_sub(None, None).is_empty());
        // a \ all-selected = empty (upstream returns new ChunkSet, not the
        // complement of a)
        assert!(cs_sub(Some(cs(&[0, 1])), None).is_empty());
        assert_eq!(cs_sub(Some(cs(&[0, 1, 5])), Some(cs(&[1, 2]))), cs(&[0, 5]));
    }

    // upstream Selection.union (both-inverted merge): intersection of the
    // stored unselected-sets, None absorbs to the other operand.
    #[test]
    fn cs_union_table() {
        let y = cs(&[1, 2]);
        assert_eq!(cs_union(None, Some(y.clone())), Some(y));
        assert_eq!(cs_union(Some(cs(&[0, 1])), None), Some(cs(&[0, 1])));
        assert_eq!(cs_union(None, None), None);
        assert_eq!(cs_union(Some(cs(&[0, 1])), Some(cs(&[1, 2]))), Some(cs(&[1])));
    }

    #[test]
    fn merge_normal_normal_unions() {
        let r = region(0, 0);
        let mut a = Selection::new();
        a.set_region(r, Some(cs(&[0])));
        let mut b = Selection::new();
        b.set_region(r, Some(cs(&[1])));
        a.merge(&b);
        assert_eq!(a.selection.get(&r), Some(&Some(cs(&[0, 1]))));
        // union with a whole-region entry stays whole-region
        let mut c = Selection::new();
        c.set_region(r, None);
        c.merge(&b);
        assert_eq!(c.selection.get(&r), Some(&None));
    }

    #[test]
    fn merge_inverted_plus_normal() {
        let r = region(0, 0);
        // A: all selected except {0,1}; B: {0,5} selected.
        // union = all except {1}  (stored: unselected {1})
        let mut a = Selection::all();
        a.set_region(r, Some(cs(&[0, 1])));
        let mut b = Selection::new();
        b.set_region(r, Some(cs(&[0, 5])));
        a.merge(&b);
        assert!(a.inverted);
        assert_eq!(a.selection.get(&r), Some(&Some(cs(&[1]))));
        assert!(!a.is_chunk_selected(chunk(1)));
        assert!(a.is_chunk_selected(chunk(0)));
        assert!(a.is_chunk_selected(chunk(5)));
        // a region not in A's map (all selected there) is unchanged by B
        assert!(!a.selection.contains_key(&region(1, 0)));
    }

    #[test]
    fn merge_normal_plus_inverted() {
        let r = region(0, 0);
        // A: {0}; B: all except {1} → union = all except {1}, inverted.
        let mut a = Selection::new();
        a.set_region(r, Some(cs(&[0])));
        a.set_region(region(2, 0), Some(cs(&[3]))); // not in B
        let mut b = Selection::all();
        b.set_region(r, Some(cs(&[1])));
        a.merge(&b);
        assert!(a.inverted);
        assert_eq!(a.selection.get(&r), Some(&Some(cs(&[1]))));
        // A-only region: other all-selected there → result all selected
        // (missing entry in inverted encoding)
        assert!(!a.selection.contains_key(&region(2, 0)));
        assert!(a.is_chunk_selected(Point2i::new(64, 0).add_p(chunk(3))));
    }

    #[test]
    fn merge_both_inverted_intersects_unselected() {
        let r = region(0, 0);
        // A: all except {0,1}; B: all except {1,2}
        // union = all except ({0,1} ∩ {1,2}) = all except {1}
        let mut a = Selection::all();
        a.set_region(r, Some(cs(&[0, 1])));
        let mut b = Selection::all();
        b.set_region(r, Some(cs(&[1, 2])));
        a.merge(&b);
        assert_eq!(a.selection.get(&r), Some(&Some(cs(&[1]))));
        assert!(a.is_chunk_selected(chunk(0)));
        assert!(!a.is_chunk_selected(chunk(1)));
        assert!(a.is_chunk_selected(chunk(2)));
        // region in A but not B: B all-selected → union all-selected
        let mut a = Selection::all();
        a.set_region(region(5, 5), Some(cs(&[7])));
        a.merge(&b);
        assert!(!a.selection.contains_key(&region(5, 5)));
    }

    #[test]
    fn inverted_select_unselect_roundtrip() {
        let r = region(0, 0);
        let mut s = Selection::all();
        assert!(s.is_chunk_selected(chunk(10)));
        // unselect one chunk -> stored as a bit in the unselected-set
        s.remove_chunk(chunk(10));
        assert!(!s.is_chunk_selected(chunk(10)));
        assert!(s.is_chunk_selected(chunk(11)));
        // unselect everything in the region -> all-unselected None entry
        for i in 0..1024 {
            s.remove_chunk(chunk(i));
        }
        assert_eq!(s.selection.get(&r), Some(&None));
        assert!(!s.is_region_selected(r));
        // reselect the whole region -> entry removed entirely
        s.add_region(r);
        assert!(!s.selection.contains_key(&r));
        assert!(s.is_region_selected(r));
        // unselect whole region again, then reselect one chunk
        s.remove_region(r);
        s.add_chunk(chunk(4));
        assert_eq!(s.selection.get(&r), Some(&Some({
            let mut u = ChunkSet::new();
            u.fill();
            u.clear(4);
            u
        })));
        assert!(s.is_chunk_selected(chunk(4)));
        assert!(!s.is_chunk_selected(chunk(5)));
    }

    #[test]
    fn normal_select_unselect_roundtrip() {
        let r = region(0, 0);
        let mut s = Selection::new();
        s.add_chunk(chunk(0));
        // select every chunk -> collapses to whole-region None
        for i in 0..1024 {
            s.add_chunk(chunk(i));
        }
        assert_eq!(s.selection.get(&r), Some(&None));
        assert!(s.is_region_selected(r));
        // unselect one -> back to a 1023-bit set
        s.remove_chunk(chunk(0));
        assert_eq!(s.selection.get(&r).unwrap().as_ref().map(|c| c.size()), Some(1023));
        // unselect all -> entry removed
        for i in 1..1024 {
            s.remove_chunk(chunk(i));
        }
        assert!(!s.selection.contains_key(&r));
    }

    #[test]
    fn set_region_normalizes() {
        let r = region(0, 0);
        let mut s = Selection::new();
        s.set_region(r, Some({
            let mut c = ChunkSet::new();
            c.fill();
            c
        }));
        assert_eq!(s.selection.get(&r), Some(&None));
        s.set_region(r, Some(ChunkSet::new()));
        assert!(!s.selection.contains_key(&r));
        s.set_region(r, Some(cs(&[9])));
        assert_eq!(s.selection.get(&r), Some(&Some(cs(&[9]))));
        s.set_region(r, None);
        assert_eq!(s.selection.get(&r), Some(&None));
    }

    #[test]
    fn selected_chunks_resolves_inverted() {
        let r = Point2i::new(0, 0);
        let mut s = Selection::all();
        s.set_region(r.as_long(), Some(cs(&[0])));
        match s.selected_chunks(r) {
            Some(Some(c)) => {
                assert_eq!(c.size(), 1023);
                assert!(!c.get(0));
            }
            other => panic!("expected partial set, got {other:?}"),
        }
        // region with no exclusions -> whole-region sentinel
        assert_eq!(s.selected_chunks(Point2i::new(1, 1)), Some(None));
    }

    #[test]
    fn add_radius_circle() {
        let r = region(0, 0);
        let mut s = Selection::new();
        s.add_chunk(chunk(5 * 32 + 5)); // center-ish chunk (5,5)
        s.add_radius(1, None);
        // (1 + 0.3)^2 = 1.69: orthogonal neighbours in, diagonals out
        for i in [5 * 32 + 4, 5 * 32 + 6, 4 * 32 + 5, 6 * 32 + 5] {
            assert!(s.selection.get(&r).unwrap().as_ref().unwrap().get(i), "idx {i}");
        }
        for i in [4 * 32 + 4, 6 * 32 + 6] {
            assert!(!s.selection.get(&r).unwrap().as_ref().unwrap().get(i), "idx {i}");
        }
        // radius bounded by `bounds`
        let mut bounds = Selection::new();
        bounds.add_region(region(1, 0));
        let mut s2 = Selection::new();
        s2.add_chunk(chunk(31)); // (31,0): x=32 is region (1,0)
        s2.add_radius(1, Some(&bounds));
        // x=32,z=0 is in bounds region (1,0) -> added; all other additions
        // are inside region (0,0) where bounds selects nothing
        assert!(s2.is_chunk_selected(Point2i::new(32, 0)));
        assert!(!s2.is_chunk_selected(Point2i::new(30, 0)));
        assert!(!s2.is_chunk_selected(Point2i::new(31, 1)));
    }
}
