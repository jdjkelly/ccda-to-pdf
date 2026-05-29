# ccda-to-pdf

Rust command-line converter for HL7 C-CDA XML documents. It parses the CDA header, extracts patient/document metadata and section narrative, then writes a readable PDF without requiring a browser or external renderer.

## CLI

```sh
cargo run -- samples/real/hl7_ccd.xml out/hl7_ccd.pdf \
  --primary-color '#165762' \
  --secondary-color '#758d98' \
  --logo path/to/logo.jpg
```

Use `-` for stdin/stdout, which is convenient from JavaScript when you want to call the native binary:

```js
import { spawn } from "node:child_process";

const child = spawn("./target/release/ccda-to-pdf", [
  "--primary-color", "#165762",
  "--secondary-color", "#758d98",
  "-", "-"
]);
child.stdin.end(ccdaXml);
child.stdout.pipe(pdfWritableStream);
```

Logo support is intentionally small: JPEG plus non-interlaced 8-bit RGB or grayscale PNG.

## WebAssembly

For a pure JavaScript integration without spawning a process, build the library for `wasm32-unknown-unknown`. The WASM API intentionally skips logos; pass only XML and colors.

```sh
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
```

Node example:

```js
import { readFile, writeFile } from "node:fs/promises";
import { loadCcdaToPdfWasm } from "./js/ccda-to-pdf-wasm.mjs";

const wasm = await readFile("./target/wasm32-unknown-unknown/release/ccda_to_pdf.wasm");
const { render } = await loadCcdaToPdfWasm(wasm);

const xml = await readFile("./samples/real/hl7_ccd.xml", "utf8");
const pdf = render(xml, {
  primaryColor: "#165762",
  secondaryColor: "#758d98",
});

await writeFile("./out/hl7_ccd.pdf", pdf);
```

## Verification Workflow

The repository includes real-world fixture CCDAs in `samples/real`. The test suite parses and renders every XML fixture, with a representative subset asserted more strictly. For manual output review:

```sh
cargo test
cargo run -- samples/real/hl7_ccd.xml out/hl7_ccd.pdf
pdfinfo out/hl7_ccd.pdf
pdftotext out/hl7_ccd.pdf -
```

For the full sample-and-judge loop, run:

```sh
scripts/judge-samples.sh samples/real out/judgement
```

That builds the release binary, converts every sample CCDA to a PDF, extracts PDF metadata/text when `pdfinfo` and `pdftotext` are available, and writes `out/judgement/report.tsv` with page counts, text volume, clinical keyword hits, and pass/fail verdicts.

Synthetic robustness fuzzing is part of the Rust test suite:

```sh
cargo test --test synthetic_fuzz
```

That generator creates randomized C-CDA-like documents with varied headers, sections, narrative tables, lists, long clinical text, Unicode punctuation, empty/null fields, rowspan/colspan combinations, hostile span values, and malformed XML. Valid synthetic documents must render to PDF; invalid inputs must return an error or a valid fallback PDF without panicking.

For a deeper local run:

```sh
CCDA_SYNTHETIC_VALID_CASES=5000 \
CCDA_SYNTHETIC_INVALID_CASES=2500 \
cargo test --test synthetic_fuzz
```

## Robustness

Real-world C-CDAs are messy, and the converter is intended for untrusted input:

- Long unbreakable tokens, such as OIDs and URLs, are split to the available line width using the same Helvetica font metrics as the PDF renderer.
- Narrative tables render as tables while each column has enough usable page width; only tables too wide to remain legible fall back to stacked label/value lines.
- XML deeper than 512 elements is rejected before parsing, which avoids parser stack exhaustion while leaving realistic C-CDA nesting unaffected.
- DTD and entity declarations are rejected before parsing, so entity-expansion inputs fail cleanly.
- CLI input and WASM XML input accept UTF-8 BOM, UTF-16LE BOM, and UTF-16BE BOM. Invalid byte sequences decode lossily, then either parse or fail with a normal XML/C-CDA error.
- The CLI and XML-to-PDF API render through a panic-safe path. If normal table layout panics on extreme input, tables are retried as stacked text; if safe mode also panics, the call returns a PDF error instead of crashing.
- Output is byte-for-byte deterministic for the same XML/options within a process invocation. The PDF writer does not embed timestamps.
