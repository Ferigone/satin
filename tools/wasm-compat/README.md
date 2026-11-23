# Offline WebAssembly compatibility inventory

From the repository root, use Node.js (tested on v26.13.0):

```powershell
node tools/wasm-compat/inventory.mjs 'C:\path\to\local-capture.wasm'
node --test tools/wasm-compat/inventory.test.mjs
```

Supply a raw local `.wasm` file you are authorized to inspect. The tool reads
it without modification, makes no network requests, and does not execute
WebAssembly or run imports or a start function. Use only lawful public
captures; do not commit third-party binaries, capture identifiers, credentials,
or cookies. Tests create and remove only their own authored fixtures under `target/`.

## Report

On success, stdout is JSON with:

- Module byte size, total/standard/custom section counts, and ordered sections
  with payload sizes and vector counts where applicable.
- Function import, definition, and body counts; parameter and result types and
  usage counts per type index (the index in `functions.types`).
- Imports and exports counted by kind. Global imports include value type and
  mutability; tag imports include their signature's type index. Symbol names
  and custom section payloads are not printed.
- Separate imported and defined table/memory shapes. Memory limits are given in
  pages and bytes; one page is 65,536 bytes. `null` means the module declares no
  maximum. For an imported memory, these are the import's declared limits, not
  necessarily the size supplied by the host.
- Structural gaps relative to the checkpoint named in `satinCheckpoint`, such
  as memory/table sections, non-function imports/exports, non-i32 signatures,
  and multiple results. `observedGaps: []` is **not** a compatibility verdict:
  the tool does not inventory instructions (including control flow, `call_indirect`,
  or bulk-memory operations), locals, or Satin's stack and execution budgets.
  It does not test the JS embedding, worker transport, or media playback.

## Validation and limits

This inventories core binary version 1, plain function types, value types
`i32`/`i64`/`f32`/`f64`/`v128`/`funcref`/`externref`, i32-addressed tables,
and 32-bit memories with standard pages (including shared memories).
Global, element, data and tag definitions and code bodies are counted rather
than analyzed. Node's `WebAssembly.validate` checks the entire input, including
the contents of these sections. This gate is host-version dependent and does
not imply Satin support. Unknown sections and unsupported metadata layouts
(such as memory64, typed GC signatures, or table initializer expressions) are
rejected, not skipped. Malformed, unsupported, over-budget, or unreadable input
exits nonzero with stderr and no partial JSON.

Reads are limited to a regular file of at most 16 MiB (plus an EOF check).
The inventory caps the number of sections at 1,024, type/import/export/table/
memory entries at 8,192 each, other counted vectors at 100,000 each, and the
total signature values at 65,536. Declared linear memory is reported without
allocating it.

## IVS evidence still needed

The Chrome baseline observed a 32 MiB IVS Wasm memory. The authored fixture
proves the inventory reports a **declared** minimum of 512 pages as
33,554,432 bytes. No IVS Wasm capture was available here. The observation alone
does not establish IVS's declared minimum, whether the memory is imported,
shared or grown, its declared maximum, or IVS's functions, tables, imports
and instruction requirements. It does not establish why public playback failed.
