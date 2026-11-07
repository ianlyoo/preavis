// Lists a wasm module's imports ("module.name") by reading its import section. No dependencies.
// Usage as a script: node scripts/wasm-imports.mjs <file.wasm>
import { readFileSync } from "node:fs";

export function wasmImports(bytes) {
  const b = Buffer.from(bytes);
  if (b.readUInt32BE(0) !== 0x0061736d) throw new Error("not a wasm module");
  let pos = 8;
  const leb = () => {
    let result = 0, shift = 0, byte;
    do { byte = b[pos++]; result += (byte & 0x7f) * 2 ** shift; shift += 7; } while (byte & 0x80);
    return result;
  };
  const name = () => { const len = leb(); const s = b.toString("utf8", pos, pos + len); pos += len; return s; };
  while (pos < b.length) {
    const id = b[pos++];
    const size = leb();
    const end = pos + size;
    if (id === 2) {
      const imports = [];
      for (let n = leb(); n > 0; n--) {
        const module = name(), field = name();
        const kind = b[pos++];
        if (kind === 0) leb();                                   // function: type index
        else if (kind === 1) { pos++; const f = leb(); leb(); if (f & 1) leb(); } // table
        else if (kind === 2) { const f = leb(); leb(); if (f & 1) leb(); }        // memory
        else if (kind === 3) pos += 2;                            // global: type, mutability
        imports.push(`${module}.${field}`);
      }
      return imports;
    }
    pos = end;
  }
  return [];
}

if (import.meta.url === `file://${process.argv[1]}`) {
  for (const i of wasmImports(readFileSync(process.argv[2]))) console.log(i);
}
