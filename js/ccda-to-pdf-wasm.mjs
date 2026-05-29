const encoder = new TextEncoder();
const decoder = new TextDecoder();

export async function loadCcdaToPdfWasm(source) {
  const instance = await instantiate(source);
  const exports = instance.exports;

  for (const name of [
    "memory",
    "ccda_alloc",
    "ccda_dealloc",
    "ccda_render",
    "ccda_result_ptr",
    "ccda_result_len",
    "ccda_error_ptr",
    "ccda_error_len",
  ]) {
    if (!(name in exports)) {
      throw new Error(`ccda-to-pdf wasm export '${name}' is missing`);
    }
  }

  function copyIn(value) {
    const bytes = typeof value === "string" ? encoder.encode(value) : value;
    if (!bytes || bytes.length === 0) {
      return { ptr: 0, len: 0 };
    }
    const ptr = exports.ccda_alloc(bytes.length);
    new Uint8Array(exports.memory.buffer).set(bytes, ptr);
    return { ptr, len: bytes.length };
  }

  function copyOut(ptr, len) {
    return new Uint8Array(exports.memory.buffer, ptr, len).slice();
  }

  function render(xml, options = {}) {
    const input = copyIn(xml);
    const primary = copyIn(options.primaryColor || "");
    const secondary = copyIn(options.secondaryColor || "");

    try {
      const status = exports.ccda_render(
        input.ptr,
        input.len,
        primary.ptr,
        primary.len,
        secondary.ptr,
        secondary.len,
      );

      if (status !== 0) {
        const err = copyOut(exports.ccda_error_ptr(), exports.ccda_error_len());
        throw new Error(decoder.decode(err));
      }

      return copyOut(exports.ccda_result_ptr(), exports.ccda_result_len());
    } finally {
      exports.ccda_dealloc(input.ptr, input.len);
      exports.ccda_dealloc(primary.ptr, primary.len);
      exports.ccda_dealloc(secondary.ptr, secondary.len);
    }
  }

  return { instance, render };
}

async function instantiate(source) {
  if (source instanceof WebAssembly.Instance) {
    return source;
  }
  if (source instanceof WebAssembly.Module) {
    return WebAssembly.instantiate(source, {});
  }
  if (typeof Response !== "undefined" && source instanceof Response) {
    const result = await WebAssembly.instantiateStreaming(source, {});
    return result.instance;
  }

  const result = await WebAssembly.instantiate(source, {});
  return result.instance;
}
