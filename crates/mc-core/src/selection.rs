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
                    if res.as_ref().map(|c| c.is_empty()).unwrap_or(false) {
                        self.selection.remove(r);
                    } else {
                        *mine = res;
                    }
                }
            }
        } else if !self.inverted {
            for (r, v) in &other.selection {
                match self.selection.get_mut(r) {
                    Some(mine) => {
                        let res = cs_sub(v.clone(), mine.clone());
                        if res.as_ref().map(|c| c.is_empty()).unwrap_or(false) {
                            self.selection.remove(r);
                        } else {
                            *mine = res;
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

fn cs_sub(a: Option<ChunkSet>, b: Option<ChunkSet>) -> Option<ChunkSet> {
    match (a, b) {
        // all selected minus anything => complement of b
        (None, Some(y)) => Some(y.flip()),
        (None, None) => Some(ChunkSet::new()),
        (Some(x), None) => Some(x.flip()),
        (Some(mut x), Some(y)) => {
            x.and_not(&y);
            Some(x)
        }
    }
}

fn cs_union(a: Option<ChunkSet>, b: Option<ChunkSet>) -> Option<ChunkSet> {
    match (a, b) {
        (Some(mut x), Some(y)) => {
            x.or(&y);
            Some(x)
        }
        (a, b) => a.or(b),
    }
}
