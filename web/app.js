const $ = id => document.getElementById(id);
const canvas = $("map");
const ctx = canvas.getContext("2d");
const stage = $("stage");
const picker = $("folder");
const dimSelect = $("dimension");
const number = new Intl.NumberFormat();
const key = (x, z) => x + "," + z;
const has = (mask, i) => (mask[i >> 3] & (1 << (i & 7))) !== 0;
const bit = (mask, i, on) => {
  if (on) mask[i >> 3] |= 1 << (i & 7);
  else mask[i >> 3] &= ~(1 << (i & 7));
};
const dimensions = new Map();
const bitmaps = new Map();
const broken = new Set();
const view = {x: 0, z: 0, zoom: 1};
let worker, dim, pending, gesture, frame, packing = false, space = false, tool = "chunks";
let request = 0, count = 0, width = 0, height = 0;

function status(message, error = false) {
  $("status").textContent = message;
  $("status").classList.toggle("error", error);
}

function schedule() {
  if (!frame) frame = requestAnimationFrame(draw);
}

function fit() {
  if (!dim) return;
  const {minX, maxX, minZ, maxZ} = dim;
  view.x = (minX + maxX) / 2;
  view.z = (minZ + maxZ) / 2;
  view.zoom = Math.min(width / (maxX - minX), height / (maxZ - minZ)) * .88;
  view.zoom = Math.min(view.zoom, 16);
  schedule();
}

function changeDimension() {
  dim = dimensions.get(dimSelect.value);
  gesture = null;
  $("coords").textContent = "X — : Z —";
  status(number.format(dim.regions.size) + " regions indexed · " + dim.label);
  fit();
  updateCount();
}

function updateCount() {
  count = 0;
  if (dim) {
    for (const mask of dim.selection.values()) {
      for (let byte of mask) {
        while (byte) { byte &= byte - 1; count++; }
      }
    }
  }
  $("count").textContent = number.format(count);
  $("clear").disabled = !count;
  $("export").disabled = !count || packing;
  $("delete").disabled = !count || packing;
  schedule();
}

function open(entries) {
  worker?.terminate();
  for (const {bitmap} of bitmaps.values()) bitmap.close();
  bitmaps.clear();
  broken.clear();
  dimensions.clear();
  dim = pending = gesture = null;
  packing = false;
  dimSelect.replaceChildren(new Option("—", ""));
  for (const id of ["dimension", "zoom-in", "zoom-out", "fit"]) $(id).disabled = true;
  $("world-name").textContent = "Indexing world…";
  $("welcome").hidden = true;
  $("coords").textContent = "X — : Z —";
  status("Reading local region headers…");
  updateCount();
  canvas.classList.remove("ready");
  const current = new Worker(new URL("./world-worker.js", import.meta.url), {type: "module"});
  worker = current;
  current.onmessage = ({data}) => {
    if (worker === current) onmessage(data);
    else data.bitmap?.close();
  };
  current.onerror = () => {
    if (worker !== current) return;
    status("The map worker could not start. Reload and check the browser console.", true);
    $("welcome").hidden = false;
    $("world-name").textContent = "No world loaded";
  };
  current.postMessage({type: "open", entries});
}

function onmessage(message) {
  switch (message.type) {
    case "indexing":
      status("Indexing · " + number.format(message.loaded) + " regions");
      break;
    case "world":
      $("world-name").textContent = message.worldName;
      for (const data of message.dimensions) {
        const dimension = {...data, minX: Infinity, maxX: -Infinity, minZ: Infinity, maxZ: -Infinity,
          regions: new Map(), selection: new Map()};
        for (const region of data.regions) {
          const id = key(region.x, region.z);
          dimension.regions.set(id, {...region, id});
          dimension.minX = Math.min(dimension.minX, region.x * 32);
          dimension.maxX = Math.max(dimension.maxX, (region.x + 1) * 32);
          dimension.minZ = Math.min(dimension.minZ, region.z * 32);
          dimension.maxZ = Math.max(dimension.maxZ, (region.z + 1) * 32);
        }
        dimensions.set(data.path, dimension);
      }
      dimSelect.replaceChildren(...[...dimensions.values()].map(d => new Option(d.label, d.path)));
      for (const id of ["dimension", "zoom-in", "zoom-out", "fit"]) $(id).disabled = false;
      canvas.classList.add("ready");
      changeDimension();
      if (message.skipped) status("World ready · " + message.skipped + " invalid regions skipped.", true);
      break;
    case "tile": {
      if (pending === message.request) pending = null;
      const id = message.dimension + "|" + message.id;
      bitmaps.get(id)?.bitmap.close();
      bitmaps.delete(id);
      bitmaps.set(id, {bitmap: message.bitmap, scale: message.scale});
      if (bitmaps.size > 96) {
        const oldest = bitmaps.keys().next().value;
        bitmaps.get(oldest).bitmap.close();
        bitmaps.delete(oldest);
      }
      schedule();
      break;
    }
    case "packing":
      status("Preparing ZIP · " + message.done + " / " + message.total);
      break;
    case "archive": {
      const url = URL.createObjectURL(message.blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = message.name;
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 60000);
      packing = false;
      status(message.remove ? "Edited copy downloaded. Original world unchanged." : "Selection downloaded.");
      updateCount();
      break;
    }
    case "error":
      status(message.message, true);
      if (message.scope === "tile") {
        if (pending === message.request) pending = null;
        broken.add(message.dimension + "|" + message.id + "|" + message.scale);
      } else if (message.scope === "archive") packing = false;
      else {
        $("world-name").textContent = "No world loaded";
        $("welcome").hidden = false;
      }
      updateCount();
      break;
  }
}

const screenX = x => (x - view.x) * view.zoom + width / 2;
const screenZ = z => (z - view.z) * view.zoom + height / 2;
const worldX = x => (x - width / 2) / view.zoom + view.x;
const worldZ = z => (z - height / 2) / view.zoom + view.z;

function drawMask(region, mask, color) {
  ctx.fillStyle = color;
  const left = screenX(region.x * 32), top = screenZ(region.z * 32);
  for (let i = 0; i < 1024; i++) {
    if (has(mask, i)) ctx.fillRect(left + (i & 31) * view.zoom, top + (i >> 5) * view.zoom,
      view.zoom, view.zoom);
  }
}

function draw() {
  frame = null;
  const ratio = window.devicePixelRatio || 1;
  const pixelsWide = Math.round(width * ratio), pixelsHigh = Math.round(height * ratio);
  if (canvas.width !== pixelsWide || canvas.height !== pixelsHigh) {
    canvas.width = pixelsWide;
    canvas.height = pixelsHigh;
  }
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  ctx.clearRect(0, 0, width, height);
  if (!dim || !width || !height) return;
  ctx.imageSmoothingEnabled = false;
  const scale = Math.max(1, Math.min(32, 16 / (2 ** Math.floor(Math.log2(view.zoom * ratio)))));
  const tileSize = 32 * view.zoom;
  const visible = [];
  for (const region of dim.regions.values()) {
    const x = screenX(region.x * 32), z = screenZ(region.z * 32);
    if (x < width && z < height && x + tileSize > 0 && z + tileSize > 0) visible.push(region);
  }
  const priority = [...visible].sort((a, b) =>
    Math.hypot(a.x * 32 + 16 - view.x, a.z * 32 + 16 - view.z) -
    Math.hypot(b.x * 32 + 16 - view.x, b.z * 32 + 16 - view.z)).slice(0, 80);
  for (const region of priority) {
    const id = dim.path + "|" + region.id;
    const cached = bitmaps.get(id);
    if (cached) {
      bitmaps.delete(id);
      bitmaps.set(id, cached);
    }
    if (!pending && !packing && cached?.scale !== scale && !broken.has(id + "|" + scale)) {
      pending = ++request;
      worker.postMessage({type: "tile", request, dimension: dim.path, id: region.id, scale});
    }
  }
  for (const region of visible) {
    const x = screenX(region.x * 32), z = screenZ(region.z * 32);
    const cached = bitmaps.get(dim.path + "|" + region.id);
    if (cached) ctx.drawImage(cached.bitmap, x, z, tileSize, tileSize);
    else if (tileSize < 8) {
      ctx.fillStyle = "#51594b";
      ctx.fillRect(x, z, Math.max(1, tileSize), Math.max(1, tileSize));
    } else drawMask(region, region.mask, "#343e32");
    if (tileSize >= 16) {
      ctx.strokeStyle = "rgba(230,230,230,.18)";
      ctx.strokeRect(x, z, tileSize, tileSize);
    }
    if (view.zoom >= 10) {
      ctx.beginPath();
      ctx.strokeStyle = "rgba(0,0,0,.26)";
      for (let i = 1; i < 32; i++) {
        const o = i * view.zoom;
        ctx.moveTo(x + o, z); ctx.lineTo(x + o, z + tileSize);
        ctx.moveTo(x, z + o); ctx.lineTo(x + tileSize, z + o);
      }
      ctx.stroke();
    }
    const mask = dim.selection.get(region.id);
    if (mask) drawMask(region, mask, "rgba(255,115,0,.67)");
  }
  if (gesture && !gesture.pan) {
    const [x, z, w, h] = selectionBox(gesture);
    const left = screenX(x), top = screenZ(z);
    ctx.fillStyle = gesture.remove ? "rgba(233,92,78,.20)" : "rgba(255,115,0,.18)";
    ctx.fillRect(left, top, w * view.zoom, h * view.zoom);
    ctx.strokeStyle = gesture.remove ? "#e95c4e" : "#ff7300";
    ctx.strokeRect(left, top, w * view.zoom, h * view.zoom);
  }
}

function selectionBox(g) {
  const unit = g.tool === "regions" ? 32 : 1;
  const x = Math.floor(Math.min(g.x, g.endX) / unit) * unit;
  const z = Math.floor(Math.min(g.z, g.endZ) / unit) * unit;
  const endX = (Math.floor(Math.max(g.x, g.endX) / unit) + 1) * unit;
  const endZ = (Math.floor(Math.max(g.z, g.endZ) / unit) + 1) * unit;
  return [x, z, endX - x, endZ - z];
}

function select(g) {
  const [x, z, w, h] = selectionBox(g);
  const click = w === h && w === (g.tool === "regions" ? 32 : 1);
  for (const region of dim.regions.values()) {
    const rx = region.x * 32, rz = region.z * 32;
    const x0 = Math.max(0, x - rx), z0 = Math.max(0, z - rz);
    const x1 = Math.min(32, x + w - rx), z1 = Math.min(32, z + h - rz);
    if (x0 >= x1 || z0 >= z1) continue;
    const mask = dim.selection.get(region.id) ?? new Uint8Array(128);
    const remove = g.remove || (click && (g.tool === "regions"
      ? mask.every((byte, i) => byte === region.mask[i]) : has(mask, x0 + z0 * 32)));
    for (let dz = z0; dz < z1; dz++) {
      for (let dx = x0; dx < x1; dx++) {
        const i = dx + dz * 32;
        if (has(region.mask, i)) bit(mask, i, !remove);
      }
    }
    if (mask.some(Boolean)) dim.selection.set(region.id, mask);
    else dim.selection.delete(region.id);
  }
  updateCount();
}

function zoom(x, z, change) {
  if (!dim) return;
  const wx = worldX(x), wz = worldZ(z);
  view.zoom = Math.max(.0001, Math.min(96, view.zoom * change));
  view.x += wx - worldX(x);
  view.z += wz - worldZ(z);
  schedule();
}

function mapPoint(event) {
  const rect = canvas.getBoundingClientRect();
  return [event.clientX - rect.left, event.clientY - rect.top];
}

canvas.addEventListener("pointerdown", event => {
  if (!dim || gesture || event.button > 1) return;
  const [mx, mz] = mapPoint(event);
  const pan = space || tool === "pan" || event.button === 1;
  const x = worldX(mx), z = worldZ(mz);
  gesture = {x, z, endX: x, endZ: z, mx, mz, pan, tool, id: event.pointerId, remove: event.altKey};
  canvas.focus({preventScroll: true});
  canvas.setPointerCapture(event.pointerId);
  if (pan) canvas.classList.add("dragging");
  schedule();
});
canvas.addEventListener("pointermove", event => {
  if (!dim) return;
  const [mx, mz] = mapPoint(event);
  if (gesture?.id === event.pointerId) {
    if (gesture.pan) {
      view.x -= (mx - gesture.mx) / view.zoom;
      view.z -= (mz - gesture.mz) / view.zoom;
      gesture.mx = mx;
      gesture.mz = mz;
    } else {
      gesture.endX = worldX(mx);
      gesture.endZ = worldZ(mz);
    }
    schedule();
  }
  $("coords").textContent = "X " + Math.floor(worldX(mx)) + " : Z " + Math.floor(worldZ(mz));
});
function finish(event) {
  if (gesture?.id !== event.pointerId) return;
  if (event.type === "pointerup" && !gesture.pan) {
    const [mx, mz] = mapPoint(event);
    gesture.endX = worldX(mx);
    gesture.endZ = worldZ(mz);
    select(gesture);
  }
  gesture = null;
  canvas.classList.remove("dragging");
  schedule();
}
for (const event of ["pointerup", "pointercancel", "lostpointercapture"]) canvas.addEventListener(event, finish);
canvas.addEventListener("wheel", event => {
  if (!dim) return;
  event.preventDefault();
  zoom(...mapPoint(event), Math.exp(-Math.sign(event.deltaY) * .2));
}, {passive: false});

window.addEventListener("keydown", event => {
  if (!dim || (event.target !== canvas && event.target !== document.body)) return;
  if (event.code === "Space") {
    space = true;
    event.preventDefault();
  } else if (event.key.startsWith("Arrow")) {
    const step = 48 / view.zoom;
    if (event.key === "ArrowLeft") view.x -= step;
    if (event.key === "ArrowRight") view.x += step;
    if (event.key === "ArrowUp") view.z -= step;
    if (event.key === "ArrowDown") view.z += step;
    event.preventDefault();
    schedule();
  } else if (event.key === "+" || event.key === "=" || event.key === "-") {
    zoom(width / 2, height / 2, event.key === "-" ? .8 : 1.25);
  }
});
window.addEventListener("keyup", event => { if (event.code === "Space") space = false; });
window.addEventListener("blur", () => { space = false; });
new ResizeObserver(([{contentRect}]) => {
  width = contentRect.width;
  height = contentRect.height;
  schedule();
}).observe(stage);

for (const button of document.querySelectorAll("[data-open]")) button.onclick = () => picker.click();
for (const button of document.querySelectorAll("[data-tool]")) button.onclick = () => {
  tool = button.dataset.tool;
  for (const option of document.querySelectorAll("[data-tool]")) {
    option.setAttribute("aria-pressed", String(button === option));
  }
  canvas.classList.toggle("pan", tool === "pan");
};
picker.onchange = () => {
  if (picker.files.length) open([...picker.files].map(file => [file.webkitRelativePath, file]));
  picker.value = "";
};
dimSelect.onchange = changeDimension;
$("fit").onclick = fit;
$("zoom-in").onclick = () => zoom(width / 2, height / 2, 1.25);
$("zoom-out").onclick = () => zoom(width / 2, height / 2, .8);
$("clear").onclick = () => { dim.selection.clear(); updateCount(); };

function archive(remove) {
  if (!dim || !count || packing) return;
  if (remove && !window.confirm("Delete " + number.format(count) + " selected chunks in a downloadable copy? Your original files won't change.")) return;
  packing = true;
  status("Preparing ZIP…");
  updateCount();
  worker.postMessage({type: "archive", dimension: dim.path, selection: [...dim.selection], remove});
}
$("export").onclick = () => archive(false);
$("delete").onclick = () => archive(true);
