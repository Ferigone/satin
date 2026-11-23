# Satin

Satin to otwartoźródłowa przeglądarka WWW w Rust. HTML, CSS, DOM, układ, renderer i JavaScript są implementowane w tym repozytorium. Chrome jest wzorcem zachowania do porównań; aplikacja nie osadza Chromium, Blink, WebView ani gotowej VM JavaScript. Biblioteki infrastrukturalne dostarczają okno, transport/TLS, fonty i dekodery obrazów.

Docelowo: zgodność ze współczesnym WWW oraz równowaga między RAM i szybkością. Windows jest pierwszą platformą. Pełna zgodność i końcowe optymalizacje pozostają otwartym celem; wydania alfa udostępniają kolejne sprawdzone podzbiory.

## Uruchamianie

Po instalacji kliknij **Rust Browser** na pulpicie. Program działa niezależnie od środowiska programistycznego i rozmowy. Stały launcher wybiera build z %LOCALAPPDATA%\RustBrowser\current.json. Wersje mają osobne katalogi, a profil pozostaje w User Data. Poprzedni build jest zachowywany do wycofania aktualizacji.

Skróty: Ctrl+L — adres, Ctrl+T/W — nowa/zamknij kartę, Ctrl+Tab — następna karta, Ctrl+R/F5 — odśwież, Alt+strzałki — historia, Ctrl+plus/minus/0 — zoom. Można przeciągnąć plik HTML na okno lub wpisać jego pełną ścieżkę.

Strony próbne:

- about:messages — przekazanie bufora i odpowiedź przez przeniesiony port.
- about:files — wybór pliku z dysku i wyświetlenie lokalnego obrazu.
- about:numbers — dokładny kwadrat BigInt oraz zapis 64-bitowy z DataView.
- about:canvas — rysowanie, czyszczenie i odczyt piksela Canvas 2D.
- about:bytes — odczyt BYOB, potok przekształcający tekst i odtworzenie pliku z formularza multipart.
- about:time — lokalny zegar, zmiana układu przy zmianie szerokości i znaczniki klatek.
- about:routes — zmiana widoku bez przeładowania, Wstecz i Dalej oraz observer DOM.
- about:async — async/await, pobranie danych, anulowanie i skrypt dołączany do gotowej strony.
- about:modules — skrypt w trakcie parsowania, document.write, import i licznik oparty na klasie.
- about:graphics — SVG, kliknięcie w kształt, pozycjonowanie, obrót, pomiary i przewijanie panelu.
- about:forms — edycja tekstu/hasła, wybór opcji, reset i lokalna obsługa formularza.
- about:platform — Grid, template, Promise, JSON i przycisk próby Fetch HTTPS.
- about:javascript — licznik, zdarzenia i timer własnej VM.
- about:demo — tekst, CSS, obrazy, linki i kotwice.
- about:home oraz about:version — start i rzeczywisty zakres wersji.

## Alfa.13

Na `about:messages` kliknij **Prześlij bajty**. W `about:files` wybierz
obraz PNG, JPEG, WebP lub GIF, żeby wyświetlić jego podgląd w karcie.

- MessageChannel i MessagePort przekazują dane w zadaniach dokumentu.
  Operacje `postMessage` i `structuredClone` przenoszą porty.
  [Kolejka i przenoszenie portów](docs/MESSAGE_PORTS.md)
- Pola plikowe korzystają z wyboru plików Windows. FileList przechowuje
  wybrane pliki, a FormData i formularze multipart korzystają z tych plików.
- createImageBitmap przyjmuje m.in. Blob, File, ImageData, płótno 2D i
  załadowany element img. Dekodowanie Blob odbywa się w tle. Wynik
  można narysować, sklonować, przenieść albo zamknąć.
  [Klonowanie i przenoszenie](docs/STRUCTURED_CLONE.md)
- Fetch przesyła ReadableStream z `duplex: "half"` przez HTTP/2.
  Klient odrzuca transmisję po połączeniu HTTP/1 przed przesłaniem ciała.

## Alfa.12

Na `about:numbers` wpisz liczbę całkowitą i kliknij **Oblicz**.
Strona pokazuje dokładny kwadrat i obie interpretacje tych samych ośmiu bajtów.

- JavaScript obsługuje BigInt, operacje na dużych liczbach całkowitych
  oraz typy `BigInt64Array` i `BigUint64Array`. DataView czyta
  i zapisuje liczby 64-bitowe w obu porządkach bajtów.
- Literały obiektów mają gettery i settery, również pod kluczami
  obliczanymi. Klonowanie przez `structuredClone` obsługuje Blob,
  File, DOMException i ImageData. [Zakres klonowania](docs/STRUCTURED_CLONE.md)

## Alfa.11

Na `about:canvas` kliknij **Rysuj** lub **Wyczyść**, aby narysować
krajobraz i sprawdzić piksel. Na `about:bytes` wpisz własny tekst i kliknij
**Przekształć tekst**, aby sprawdzić odczyt do podanego bufora i potok.

- Canvas 2D rysuje ścieżki, tekst, gradienty, wzory i cienie. Obsługuje
  ImageData, kopiowanie obrazów i eksport PNG, JPEG i WebP. Zmiana pikseli
  odświeża obraz bez ponownego liczenia układu strony.
- Web Streams obsługuje WritableStream, TransformStream, pipeTo,
  pipeThrough, asynchroniczną iterację oraz czytniki BYOB dla strumieni
  bajtowych, także z Fetch i Blob.
- Window, Document i węzły DOM używają EventTarget. Obsługują własne
  zdarzenia CustomEvent, fazy propagacji, obiekty handleEvent,
  opcje passive, once i AbortSignal.
- `structuredClone` kopiuje współdzielone i cykliczne grafy wybranych
  typów JavaScript oraz przenosi ArrayBuffer. [Obsługiwane typy](docs/STRUCTURED_CLONE.md)

## Alfa.10

Otwórz `about:time` i kliknij **Uruchom zegar**. Data i godzina korzystają
ze strefy systemu, a znacznik klatki używa zegara monotonicznego.

- Web Storage udostępnia własności i metody `localStorage` oraz
  `sessionStorage`. Profil zapamiętuje localStorage osobno dla każdego origin.
  sessionStorage jest oddzielny dla każdej karty i przechodzi przez nawigacje
  w jej obrębie. Wspólne dane wywołują `StorageEvent` w innych kartach tego samego origin.
- Własny JavaScript ma `Date` ze strefą czasu Windows, prywatne elementy klas
  i sprawdzanie `#nazwa in obiekt`. Karty mają `setInterval`,
  `requestAnimationFrame` i `performance.now()`.
- Grid obsługuje nazwane linie i obszary oraz tory powtarzane przez
  `auto-fit` i `auto-fill`.

## Alfa.9

Otwórz `about:bytes`, wpisz tekst i kliknij **Sprawdź bajty**. Strona odczytuje
dwukrotnie ten sam strumień, a następnie pokazuje znak po znaku tekst odzyskany z
pliku przesłanego przez kodowanie multipart.

- Fetch udostępnia nagłówki podczas pobierania i przekazuje porcje `Uint8Array`.
  Czytniki i ich klony mogą anulować pracę. Kolejka sieć–VM przechowuje do dwóch
  porcji po 32 KiB na odpowiedź. Całościowe metody odczytu mają limit 4 MiB.
- Blob, File, FormData i FormDataEvent obsługują zawartość binarną, multipart
  i dane URL-encoded. Formularze mogą wysłać plik utworzony przez JavaScript.
- Własna VM uruchamia zwykłe i asynchroniczne generatory oraz `for await … of`.
  Ma natywny ArrayBuffer, numeryczne tablice typowane, DataView i osobne widoki
  wspólnej pamięci. Host udostępnia TextEncoder i TextDecoder.

## Alfa.8

`about:routes` pozwala otworzyć **Szczegóły** i wrócić do **Początku** przyciskiem
Wstecz lub skrótem Alt+strzałka w lewo. Strona aktualizuje adres, widok i tytuł
w obrębie jednego dokumentu.

- Własna VM ma `Map`, `Set`, `WeakMap`, `WeakSet` i silnik wyrażeń regularnych z
  literałami oraz metodami `String`.
- MutationObserver odbiera zmiany w DOM, także podczas parsowania strumienia.
- History i Location aktualizują adres oraz odtwarzają stan wpisu przy powrocie.
  [Historia i Location](docs/HISTORY.md) · [MutationObserver](docs/MUTATION_OBSERVERS.md)

## Alfa.7

Na stronie `about:async` przycisk **Uruchom** wykonuje zadanie w JavaScript. Przycisk **Anuluj** przerywa je podczas oczekiwania. W Chrome i w Rust zachowanie sieci sprawdza wspólny scenariusz z lokalnym serwerem.

- VM wznawia funkcje async i moduły z `await` w kolejce mikrozadań. Strona może dołączać elementy `script` po załadowaniu dokumentu i pobierać moduły za pomocą `import()`.
- Fetch przyjmuje `AbortSignal`, zachowuje przekazany powód anulowania i przerywa oczekiwanie na odpowiedź lub dane. Nawigacja, zasoby i Fetch współdzielą transport wykonujący asynchroniczne I/O w ograniczonych kolejkach.
- `URL` i `URLSearchParams` mają powiązane zapytanie i parametry. Fetch może wysłać parametry jako ciało formularza URL-encoded.

Wyniki prób i stan instalacji są w [VERIFICATION.md](docs/VERIFICATION.md). Szczegóły transportu, cyklu życia skryptów i obecne limity opisuje [NAVIGATION.md](docs/NAVIGATION.md).

## Alfa.6

- HTML jest przetwarzany w miarę odbierania odpowiedzi HTTP. Skrypty widzą bieżący DOM, działają w kolejności blokującej, defer lub async i mogą użyć document.write, gdy parser ma punkt wstawiania.
- Własna VM uruchamia klasy z polami publicznymi i dziedziczeniem oraz statyczne moduły z żywymi eksportami. Moduły korzystają z import map, sprawdzenia CORS, typu pliku i integralności; kolekcje po tagu, klasie i nazwie reagują na zmianę DOM.
- CSS obsługuje rodziny fontów systemowych i sieciowych, WOFF/WOFF2, zakresy znaków i osie wagi oraz pochylenia. Po otrzymaniu fontu layout i rysowanie używają tej samej wersji danych.
- Nawigacja, zasoby i Fetch obsługują adresy data:. Statyczne strony bez skryptów, handlerów i kontrolek formularzy nie potrzebują VM.

Architekturę opisują [NAVIGATION.md](docs/NAVIGATION.md), [HTML_STREAMING.md](docs/HTML_STREAMING.md), [JAVASCRIPT.md](docs/JAVASCRIPT.md) i [FONTS.md](docs/FONTS.md). [VERIFICATION.md](docs/VERIFICATION.md) rozróżnia wyniki prób od pełnej zgodności z Chrome.

## Zakres wcześniejszych wydań

Nowe mechanizmy:

- Szablony tekstowe i tagowane z UTF-16 i tożsamością miejsca, String.raw, destrukturyzacja, wartości domyślne i rest, spread/rest obiektów, optional chaining oraz przypisania logiczne. Własne iterator closing, środowiska parametrów, GC i limity nadal obowiązują.
- SVG: własny parser XML dla plików, kształty i wszystkie komendy ścieżek, krzywe/łuki, kontury i dash, gradienty, clipPath, use/symbol, viewBox oraz rysowanie bez osobnej bitmapy dla każdego SVG. Trafienia stosują rzeczywistą geometrię; obrysy DOM są niezależne od stroke i przycięcia. Obrazy SVG nie uruchamiają skryptów ani nie pobierają zewnętrznych zasobów.
- CSS relative/absolute/fixed, bloki zawierające, insety, z-index i konteksty warstw, overflow, zagnieżdżone przewijanie i przekształcenia 2D. Renderer i trafienia wykorzystują te same macierze oraz klipy.
- Pomiary i częściowy CSSOM: getBoundingClientRect/getClientRects, getComputedStyle, offset/client/scroll oraz zdarzenia resize/scroll. Odczyty współdzielą układ i migawkę DOM z oknem. Niewidoczny tekst nie powoduje alokacji bitmap glifów; niezmienne dane fontów są współdzielone.

Wcześniejszy zakres obejmuje HTTP/HTTPS z kontrolą certyfikatów, CORS, przekierowania, Fetch/Headers/Request/Response, Promise/JSON, symbole i iteratory, fragmenty HTML/template, podzbiory Flexbox/Grid, formularze GET/POST i cookies. Cookies trwałe są chronione DPAPI; sesyjne kończą się z procesem. Statyczne strony bez skryptów, handlerów i kontrolek formularzy nie alokują VM.

Alfa.5 przeszła 417 testów Rust, ścisłą kontrolę Clippy, próby HTTPS, scenariusze grafiki i formularzy z Chrome przez REA oraz niezależne uruchomienie przez zainstalowany launcher. Wyniki tych prób są zachowane w historii wydań.

## Pozostałe prace

Brakuje pełnego ECMAScript i biblioteki standardowej, w tym Intl, Proxy, SharedArrayBuffer, atrybutów importu i składni zbiorów Unicode RegExp. Pozostają pełne formularze, FileReader, CSP, IndexedDB, service workery, multimedia, OffscreenCanvas, WebGL, WebGPU, WebAssembly i skład MathML. Canvas 2D, SVG/XML, CSS i CSSOM mają częściową obsługę. Tekst, filtry i maski SVG, sticky, transformacje 3D, pseudoelementy i pełne reguły overflow viewportu wymagają dalszej pracy.

Potrzebne są dynamiczne aktualizacje wszystkich typów zasobów, pozostałe operacje na żywych kolekcjach, pełna typografia, przyrostowy układ i rysowanie, izolacja procesu treści oraz GPU. Limity mogą zatrzymać skrypt lub zgłosić uproszczony układ albo rasteryzację.

## Budowanie i kontrola instalacji

Wymagane: Windows x64, Rust 1.99.1 MSVC i C++ Build Tools / Windows SDK. Skrypty wykrywają prywatne narzędzia w %LOCALAPPDATA%\RustBrowserDev.

    .\scripts\Build.ps1 -Release -Test
    .\scripts\Verify.ps1 -Network
    .\scripts\Install.ps1 -SkipBuild

Instalator sprawdza skopiowanego kandydata, w tym niewidoczne natywne okno i kliknięcie SVG, przed przełączeniem manifestu. Zachowuje profil i poprzedni build. Otwartej sesji nie zamyka przymusowo. Wycofanie:

    & "$env:LOCALAPPDATA\RustBrowser\Rollback.ps1"

Scenariusze lokalne uruchamiają własny serwer. W porównaniach z Chrome
`PATH_TO_REA_MJS` wskazuje na plik `scripts/rea.mjs` z instalacji REA.

    node tools/verify-graphics.mjs target/release/rust-browser.exe target/graphics-verification --chrome-reference
    node tools/verify-forms.mjs target/release/rust-browser.exe target/forms-verification --chrome-reference --timeout-ms 45000
    node tools/verify-web.mjs target/release/rust-browser.exe target/web-verification
    node tools/verify-navigation.mjs target/release/rust-browser.exe target/navigation-verification --chrome-reference
    node tools/verify-stream-resources.mjs target/release/rust-browser.exe
    node tools/verify-fonts.mjs target/release/rust-browser.exe target/font-verification --chrome-reference --rea PATH_TO_REA_MJS
    node tools/verify-dynamic-scripts.mjs target/release/rust-browser.exe target/dynamic-verification --chrome-reference --rea PATH_TO_REA_MJS
    node tools/verify-async-network.mjs target/release/rust-browser.exe target/async-verification --chrome-reference --rea PATH_TO_REA_MJS
    node tools/verify-history.mjs target/release/rust-browser.exe target/history-verification --rea PATH_TO_REA_MJS
    node tools/verify-keyed-collections.mjs target/release/rust-browser.exe target/collections-verification --chrome-reference --rea PATH_TO_REA_MJS
    node tools/verify-fixtures.mjs

Wejście natywne trafia do wewnętrznych handlerów aplikacji. Próby nie sterują myszą/klawiaturą systemu ani otwartymi kartami użytkownika. Node orkiestruje testy; skrypty strony i Test262 wykonuje własna VM Rust. Na Windows GUI należy uruchamiać z oczekiwaniem i przekierowaniem wyjścia, np. przez scripts/Process.ps1.

Opis mechanizmów i ograniczeń: [JavaScript](docs/JAVASCRIPT.md), [CSS/układ](docs/CSS_LAYOUT.md), [SVG](docs/SVG.md), [CSSOM View](docs/CSSOM_VIEW.md), [formularze](docs/FORMS.md). Cały zakres projektu i kryteria etapów: [ROADMAP](docs/ROADMAP.md).
