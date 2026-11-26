const max32 = 0xffffffff;

export class Zip {
  #items = [];
  #central = [];
  #size = 0;
  #directorySize = 0;

  add(path, contents, crc) {
    const name = new TextEncoder().encode(path);
    const size = contents.size ?? contents.byteLength;
    const local = new Uint8Array(30 + name.length);
    const central = new Uint8Array(46 + name.length);
    if (name.length > 65535 || this.#central.length >= 65535 ||
        this.#size + local.length + size + this.#directorySize + central.length + 22 > max32) {
      throw new Error("This ZIP exceeds the browser export limit (4 GiB or 65,535 files).");
    }
    const localHeader = new DataView(local.buffer);
    const centralHeader = new DataView(central.buffer);
    const put = (view, offset, value, wide = false) => {
      if (wide) view.setUint32(offset, value, true);
      else view.setUint16(offset, value, true);
    };

    put(localHeader, 0, 0x04034b50, true);
    put(localHeader, 4, 20);
    put(localHeader, 6, 0x800);
    put(localHeader, 12, 0x21);
    put(localHeader, 14, crc, true);
    put(localHeader, 18, size, true);
    put(localHeader, 22, size, true);
    put(localHeader, 26, name.length);
    local.set(name, 30);

    put(centralHeader, 0, 0x02014b50, true);
    put(centralHeader, 4, 20);
    central.set(local.subarray(4, 28), 6);
    put(centralHeader, 42, this.#size, true);
    central.set(name, 46);
    this.#items.push(local, contents);
    this.#central.push(central);
    this.#size += local.length + size;
    this.#directorySize += central.length;
  }

  finish() {
    const end = new Uint8Array(22);
    const footer = new DataView(end.buffer);
    footer.setUint32(0, 0x06054b50, true);
    footer.setUint16(8, this.#central.length, true);
    footer.setUint16(10, this.#central.length, true);
    footer.setUint32(12, this.#directorySize, true);
    footer.setUint32(16, this.#size, true);
    return new Blob([...this.#items, ...this.#central, end], {type: "application/zip"});
  }
}
