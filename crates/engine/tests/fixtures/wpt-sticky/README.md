# Przypięte WPT dla CSS `position: sticky`

Zestaw zawiera 19 stron i 52 synchroniczne podtesty. Oryginalne strony, współdzielony `sticky-util.js` i oficjalny `testharness.js` zachowują bajty rewizji `9e1907a13cbbc338975302097a4bafcbc4d12dc5`. Manifest zapisuje ich ścieżki źródłowe, sumy SHA-256 i liczbę podtestów. [Źródło WPT](https://github.com/web-platform-tests/wpt/tree/9e1907a13cbbc338975302097a4bafcbc4d12dc5/css/css-position/sticky) · [Licencja WPT](../wpt-url/LICENSE.md)

```powershell
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$env:CARGO_INCREMENTAL = '0'
cargo test -p rust-browser-engine --test sticky_wpt -- --nocapture
node tools/verify-sticky-wpt.mjs OUTPUT REA_CLI
```

Test Rust wykonuje oryginalne ciała podtestów i zachowuje ich asercje. Prosty adapter uruchamia każde ciało jako osobne zadanie JavaScript na wczytanym dokumencie. Dla dwóch plików wpisy identyfikatorów elementów na `Window` pochodzą z istniejącego DOM. Przy przypadkach korzystających z `setupStickyTest` adapter przekazuje tekstową wartość `element.style` do właściwości `style.cssText` silnika. Odczyty geometrii i przewijanie wykonuje normalny kod silnika.

Kompilator JavaScript alfy.12 kończy wczytywanie `position-sticky-parsing.html`
na przypisaniu do istniejącej zmiennej w `for...of`. Raport rejestruje ten
podtest jako niewykonany. Rozpoznawanie `sticky` przez CSS i CSSOM sprawdzają
osobna regresja parsera i scenariusz `tools/verify-sticky-browser.mjs`.

Oddzielny proces headless Chrome uruchamia te same strony z oficjalnym `testharness.js`. Jego adapter raportujący zbiera oryginalne wyniki w niezależnych ramkach dokumentu o wymiarach 800 × 600 CSS px. Przypadki testują geometrię, w tym interakcję sticky z wewnętrznym `overflow: hidden`. Wyniki dla testu obejmującego strukturę z deklaracjami CSS tabeli dotyczą asercji geometrii. Zestaw obejmuje przewijanie viewportu, fizyczne insets, ograniczenia bloków zawierających, zmianę stylu i DOM oraz zagnieżdżone elementy sticky.
