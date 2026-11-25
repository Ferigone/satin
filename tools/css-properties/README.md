# CSS property inventory

Run the comparison offline from the repository root.

```sh
cargo run --locked -q -p rust-browser-engine --example css_property_inventory > css-properties.jsonl
```

The example emits one JSON record per property on stdout and counts on stderr. Pass `-- --strict` to exit unsuccessfully when at least one tested inline edit or distinguishing computed-value comparison fails. The default exit status allows collecting the complete inventory while support is incomplete. A broken harness always fails, using a known opacity change and an unknown property as controls.

## Scope

The inventory contains all **823 property names** in the W3C Webref package `@webref/css@8.7.10`, including 70 prefixed names. Webref collects definitions across specification drafts, including legacy aliases and specialized media. Its at-rules, descriptors, selectors, functions and types are separate collections. Author-defined custom property names are open-ended and have their own tests in `crates/engine/tests/css_variables*.rs`.

The same HTML, viewport and JavaScript function run in Satin and the reference Chromium **158.0.8127.0**. For each property the probe clears inline style, reads the initial computed value, sets an important declaration and reads the specified and live computed values. It then tries an invalid declaration and checks that the value and priority survived, removes the declaration and checks that the property is empty again. The fixture supplies a fixed-size containing block. Solid border and outline styles make width changes observable.

## Reading the report

- `inline: matched` requires the reference's specified-value serialization, priority preservation across a malformed write, and removal. `different-serialization` distinguishes accepted edits from rejected ones.
- `computed: matched-change` requires a nonempty Chromium value that differs from its control, a change in Satin, and the same final CSSOM string. Each row includes both engines' values before and after. Exact serialization can expose numeric rounding differences.
- `missing-readback`, `unchanged`, and `different-value` identify three distinct failures against a distinguishing reference. A blank computed property can coexist with working layout or paint.
- In **625** cases Chromium supplies a distinguishing change. In **5** it accepts an example but that example has no distinguishing computed value. In **193** the generator has no accepted reference sample. The latter still exercise the valid CSS-wide keyword `initial` in Satin, with null reference fields. Neither group enters the computed comparison denominator.

Each row probes one chosen value on a connected HTML element. Use geometry, interaction and pixel tests for property effects. The inventory can serve as a work list for additional value grammars, element types and media contexts.

## Refreshing the reference

Ordinary runs read the checked-in `properties.jsonl`. Regeneration needs Python Playwright, the pinned Chromium executable and the `css.json` file from the [Webref npm archive](https://registry.npmjs.org/@webref/css/-/css-8.7.10.tgz). The generator checks the input SHA-256 and browser version. The captured headless executable came from [the Chromium 158 package](https://registry.npmjs.org/@sparticuz/chromium/-/chromium-158.0.0.tgz).

```sh
python tools/css-properties/refresh.py /path/to/webref/css.json /path/to/chromium
```

The generator tests candidate values in Chromium and prefers the first that changes the computed property. Each sorted reference row contains the property name, test input, specified serialization, and computed values before and after the edit. The Webref package documents its [spec-extraction method and scope](https://github.com/w3c/webref/tree/main/packages/css).
