# Przypięte WPT dla IntersectionObserver

Manifest zapisuje ścieżki i SHA-256 plików z rewizji `9e1907a13cbbc338975302097a4bafcbc4d12dc5`. Oryginalne strony obejmują viewport, jawny korzeń będący elementem lub dokumentem, przewijanie, przycinanie, progi, `rootMargin`, cel zerowej wielkości, ponowne obserwowanie po `unobserve()` i pustą kolejkę `takeRecords()` przed pierwszą aktualizacją. [Źródło WPT](https://github.com/web-platform-tests/wpt/tree/9e1907a13cbbc338975302097a4bafcbc4d12dc5/intersection-observer) · [Licencja WPT](upstream/LICENSE.md)

```powershell
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$env:CARGO_INCREMENTAL = '0'
cargo test -p rust-browser-engine --test intersection_observer_wpt -- --nocapture
node tools/verify-intersection-observer-wpt.mjs OUTPUT REA_CLI
```

Test Rust podaje oryginalne skrypty i `intersection-observer-test-utils.js` do `ScriptRuntime` z widocznym obszarem 800 × 600 CSS px. `bridge.js` zapewnia funkcje rejestracji testów i asercje. Wskazane w manifeście identyfikatory są też dostępne jako właściwości `Window`. Zegar, klatki animacji, kolejkę obserwatora i geometrię obsługuje silnik.

Siedem stron używa `calc(100vh + 100px)` do wyznaczenia wysokości pustego bloku. Test wymaga oczekiwanej liczby prób z manifestu, statusu `PASS` dla każdej próby i braku diagnostyki silnika. `cargo test -p rust-browser-engine --test intersection_observer` sprawdza też zagnieżdżone przewijanie, zmianę viewportu i opróżnienie kolejki przez `takeRecords()`.

Porównanie używa nowego procesu Chrome przez REA. Oficjalny `testharness.js` uruchamia te same bajty w osobnych, widocznych ramkach tego samego pochodzenia. Rozmiar każdej ramki i viewportu nadrzędnego to 800 × 600 CSS px. Lokalny `testharnessreport.js` zbiera statusy bez modyfikowania testów.
