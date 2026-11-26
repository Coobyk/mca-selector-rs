import init, {keep_chunks, occupied_chunks, render_region, zip_crc32} from "./wasm/mca_selector_web.js";
import {Zip} from "./zip.js";

const ready = init();
const regionPattern = /^r\.(-?\d+)\.(-?\d+)\.mca$/;
const sidecarPattern = /^c\.(-?\d+)\.(-?\d+)\.mcc$/;
const dimensions = new Map();
const files = new Map();
const sidecars = new Map();
const key = (x, z) => x + "," + z;
const read = async file => new Uint8Array(await file.arrayBuffer());
const has = (mask, i) => (mask[i >> 3] & (1 << (i & 7))) !== 0;
let worldName = "";

function dimInfo(path) {
  if (!path) return ["Overworld", "overworld"];
  if (path === "DIM-1") return ["The Nether", "nether"];
  if (path === "DIM1") return ["The End", "end"];
  const name = path.split("/").at(-1).toLowerCase();
  const kind = name.includes("nether") || name === "dim-1" ? "nether" :
    name.includes("end") || name === "dim1" ? "end" : "overworld";
  const custom = /^dimensions\/([^/]+)\/(.+)$/.exec(path);
  return [custom ? custom[1] + ":" + custom[2] : path, kind];
}

function split(path) {
  const names = path.split("/");
  if (names.length < 2 || names.some(part => !part || part === "." || part === ".." ||
      part.includes("\\") || part.includes("\0"))) return null;
  return [names[0], names.slice(1).join("/")];
}

function extendBounds(bounds, regionX, regionZ, mask) {
  for (let i = 0; i < 1024; i++) {
    if (!has(mask, i)) continue;
    const x = regionX * 32 + (i & 31), z = regionZ * 32 + (i >> 5);
    bounds.minX = Math.min(bounds.minX, x);
    bounds.maxX = Math.max(bounds.maxX, x + 1);
    bounds.minZ = Math.min(bounds.minZ, z);
    bounds.maxZ = Math.max(bounds.maxZ, z + 1);
  }
}

async function openWorld(entries) {
  for (const [path, file] of entries) {
    const parts = split(path);
    if (!parts) continue;
    const [root, relative] = parts;
    if (!worldName) worldName = root;
    if (root !== worldName) throw new Error("Choose one world folder at a time.");
    files.set(relative, file);
  }
  if (!files.has("level.dat")) throw new Error("No level.dat found. Choose the root of a Java Edition world.");
  let loaded = 0, skipped = 0;
  for (const [path, file] of files) {
    const pieces = path.split("/");
    const name = pieces.pop();
    const directory = pieces.join("/");
    const external = sidecarPattern.exec(name);
    if (external) {
      const x = Number(external[1]), z = Number(external[2]);
      const bucket = directory + "/" + key(Math.floor(x / 32), Math.floor(z / 32));
      if (!sidecars.has(bucket)) sidecars.set(bucket, []);
      sidecars.get(bucket).push({x, z, file, path});
      continue;
    }
    const match = regionPattern.exec(name);
    if (pieces.pop() !== "region" || !match) continue;
    const dimPath = pieces.join("/");
    if (dimPath && !/^DIM-?\d+$/.test(dimPath) && !/^dimensions\/[^/]+\/.+$/.test(dimPath)) continue;
    const x = Number(match[1]), z = Number(match[2]);
    if (Math.abs(x) > 4194303 || Math.abs(z) > 4194303 || file.size < 8192) {
      skipped++;
      continue;
    }
    try {
      const mask = occupied_chunks(await read(file.slice(0, 8192)));
      if (!mask.some(Boolean)) continue;
      if (!dimensions.has(dimPath)) {
        const [label, kind] = dimInfo(dimPath);
        const bounds = {minX: Infinity, maxX: -Infinity, minZ: Infinity, maxZ: -Infinity};
        dimensions.set(dimPath, {path: dimPath, label, kind, bounds, regions: new Map()});
      }
      const dim = dimensions.get(dimPath);
      extendBounds(dim.bounds, x, z, mask);
      dim.regions.set(key(x, z), {x, z, mask, directory, name, file});
      loaded++;
      if (loaded % 32 === 0) self.postMessage({type: "indexing", loaded});
    } catch {
      skipped++;
    }
  }
  if (!loaded) throw new Error("No populated Anvil (.mca) region files found in this world.");
  self.postMessage({
    type: "world", worldName, skipped,
    dimensions: [...dimensions.values()].sort((a, b) => a.path.localeCompare(b.path)).map(dim => ({
      path: dim.path, label: dim.label, bounds: dim.bounds,
      regions: [...dim.regions.values()].map(({x, z, mask}) => ({x, z, mask})),
    })),
  });
}

async function tile(message) {
  const {dimension, id, scale, request} = message;
  const dim = dimensions.get(dimension);
  const region = dim.regions.get(id);
  const external = sidecars.get(region.directory + "/" + id) ?? [];
  const payloads = await Promise.all(external.map(async ({x, z, file}) => [x, z, await read(file)]));
  const pixels = render_region(region.name, await read(region.file), dim.kind, scale, payloads);
  if (region.terrain === undefined) {
    const {data, width, height} = pixels;
    let x0 = width, x1 = 0, z0 = height, z1 = 0;
    for (let z = 0; z < height; z++) {
      for (let x = 0; x < width; x++) {
        if (!data[(z * width + x) * 4 + 3]) continue;
        x0 = Math.min(x0, x);
        x1 = Math.max(x1, x + 1);
        z0 = Math.min(z0, z);
        z1 = Math.max(z1, z + 1);
      }
    }
    region.terrain = x0 < x1 ? {
      minX: region.x * 32 + Math.floor(x0 * 32 / width),
      maxX: region.x * 32 + Math.ceil(x1 * 32 / width),
      minZ: region.z * 32 + Math.floor(z0 * 32 / height),
      maxZ: region.z * 32 + Math.ceil(z1 * 32 / height),
    } : null;
  }
  const bitmap = await createImageBitmap(pixels);
  self.postMessage({type: "tile", request, dimension, id, scale, bitmap, bounds: region.terrain}, [bitmap]);
}

async function archive(message) {
  const {dimension, selection, remove} = message;
  const dim = dimensions.get(dimension);
  const replacements = new Map();
  const prefix = dimension ? dimension + "/" : "";
  const zip = new Zip();
  const folder = worldName + (remove ? "-edited" : "-selection");
  const add = async (path, contents) => {
    const bytes = contents instanceof Blob ? await read(contents) : contents;
    zip.add(folder + "/" + path, contents, zip_crc32(bytes));
  };
  let done = 0;
  const total = selection.length + (remove ? files.size : 0);
  for (const [id, selected] of selection) {
    const {x, z, name} = dim.regions.get(id);
    for (const kind of ["region", "entities", "poi"]) {
      const directory = prefix + kind;
      const path = directory + "/" + name;
      const file = files.get(path);
      if (!file) continue;
      const keep = remove ? selected.map(byte => byte ^ 255) : selected;
      const [bytes, external] = keep_chunks(await read(file), keep);
      if (remove) replacements.set(path, bytes.length > 8192 ? bytes : null);
      else if (bytes.length > 8192) await add(path, bytes);
      const available = new Map((sidecars.get(directory + "/" + id) ?? []).map(sidecar =>
        [key(sidecar.x, sidecar.z), sidecar]));
      for (let i = 0; i < 1024; i++) {
        if (!has(external, i)) continue;
        const wanted = key(x * 32 + (i & 31), z * 32 + (i >> 5));
        const sidecar = available.get(wanted);
        if (!sidecar) throw new Error("Missing external chunk record c." + wanted.replace(",", ".") + ".mcc.");
        if (!remove) await add(sidecar.path, sidecar.file);
      }
      if (remove) {
        for (const sidecar of available.values()) {
          const i = (sidecar.x - x * 32) + (sidecar.z - z * 32) * 32;
          if (has(selected, i)) replacements.set(sidecar.path, null);
        }
      }
    }
    self.postMessage({type: "packing", done: ++done, total});
  }
  for (const [path, file] of files) {
    if (remove) {
      const contents = replacements.has(path) ? replacements.get(path) : file;
      if (path !== "session.lock" && contents) await add(path, contents);
      if (++done % 16 === 0) self.postMessage({type: "packing", done, total});
    } else if (path === "level.dat" || path === "icon.png" || path.startsWith("datapacks/")) {
      await add(path, file);
    }
  }
  self.postMessage({type: "archive", blob: zip.finish(), name: folder + ".zip", remove});
}

self.onmessage = async ({data: message}) => {
  try {
    await ready;
    if (message.type === "open") await openWorld(message.entries);
    if (message.type === "tile") await tile(message);
    if (message.type === "archive") await archive(message);
  } catch (error) {
    self.postMessage({
      type: "error", scope: message.type, request: message.request,
      dimension: message.dimension, id: message.id, scale: message.scale,
      message: error.message ?? String(error),
    });
  }
};
