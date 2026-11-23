# Pinned WPT data URL cases

`data-urls.json` is retained byte for byte from the WPT revision and path in
`manifest.json`. The downloaded bytes were verified against the Git blob id
returned by the primary repository contents API. The manifest also records
SHA-256 and length. WPT's license is retained in `src/svg/fixtures/LICENSE.md`.

`tests/data_url_conformance.rs` runs every one of the 72 original input/MIME/body
tuples through the URL parser and our own data-URL processor. Expected failures
stay failures and there are no case filters. This is a decoder subset test,
not a full Fetch/WPT result; browser integration has separate tests.
