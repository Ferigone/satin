# Test262 dla własnego silnika JavaScript

Runner uruchamia kod testów wyłącznie w `rb-js`, czyli naszym interpreterze
napisanym w Rust. Node obsługuje YAML, wybór plików i proces roboczy. Nie wykonuje
testowanego JavaScript przez V8, `eval`, `vm` ani import testowanych skryptów.

Repozytorium [Test262](https://github.com/tc39/test262) musi wskazywać commit
`48221d7c1f7745dc13e420a2e850a248c0b2afb4`. Źródła i niezmieniony harness są
odczytywane z obiektów Git, z weryfikacją hashy blobów. Ustawienia CRLF Windows
nie wpływają na wejście parsera. Test262 nie jest dołączanym silnikiem JS.

Przykład z katalogu głównego projektu:

```powershell
cargo build -p rust-browser-js-harness --release --locked
npm --prefix tools/test262 ci --ignore-scripts --no-audit --no-fund
node tools/test262/runner.mjs --test262 PATH_TO_TEST262 --worker target/release/rb-js.exe --path language/expressions/addition --path language/expressions/multiplication --path language/expressions/subtraction --path language/expressions/strict-equals --path language/statements/if --output target/test262/report.json
```

Każde żądanie otrzymuje nową VM i oddzielny stan globalny. Zwykłe testy mają
wariant bez strict i ze strict, z uwzględnieniem `noStrict`, `onlyStrict`, `raw`
oraz `module`. Warianty `raw` nie dostają harnessu; pozostałe dostają oryginalne
`sta.js`, `assert.js` i wskazane `includes`. Warianty inne niż `raw` z flagą `async` dostają także
oryginalny, niezmieniony `doneprintHandle.js`. Błędy parse/early są sprawdzane
bez wykonywania testu; błąd w harnessie nie zalicza negatywnego testu runtime.
Budżety, timeout, awaria procesu, nieobsługiwany host i błędne
metadane mają osobne wyniki.

## Moduły źródłowe

Wariant `module` używa odrębnej, zawsze ścisłej gramatyki modułowej. Runner
pobiera z parsera uporządkowane żądania importu z dokładnymi jednostkami UTF-16.
Odczytuje względne źródła z przypiętych blobów Git i przekazuje workerowi graf
powiązań. Linkowanie i ewaluację wykonuje VM z żywymi wiązaniami, obiektami
przestrzeni nazw, cyklami i import.meta. Skrypty harnessu pozostają osobnymi
skryptami w tej samej VM.

Negatywne testy fazy `resolution` uruchamiają linkowanie i wymagają
oczekiwanej klasy błędu z tej fazy. Błędy przygotowania hosta, brak pliku
fixture i błędy składni modułu wejściowego nie zaliczają takiego testu. Negatywne
testy runtime wykonują zlinkowany graf. Top-level await wstrzymuje moduł do
czasu wykonania odpowiednich zadań Promise. Graf nadal oczekujący po
opróżnieniu kolejki ma wynik `async_incomplete`.

Dla testów używających importu dynamicznego lub top-level await runner
przekazuje zestaw oryginalnych względnych plików `_FIXTURE.js`. Worker rozwiązuje
żądanie po jego wykonaniu w VM i współdzieli rekord modułu dla tego samego pliku.
Zestaw importu dynamicznego mieści do 1023 plików i 4 MiB tekstu źródłowego.
Statyczny graf ma limit 1023 plików fixture, 16384 krawędzi i 8 MiB tekstu.
Import źródłowy i odroczony, atrybuty importu, moduły JSON, zewnętrzne URL-e
oraz host agentów pozostają nieobsługiwane.

## Testy asynchroniczne

Worker obsługuje zakończenie testów `async` przez `$DONE`, dostarczane przez
upstreamowy `doneprintHandle.js`. Po wykonaniu źródła opróżnia natywną kolejkę
VM: reakcje Promise, asymilacja thenable i mikrozadania korzystają ze wspólnego
porządku FIFO. Kod testu i cały checkpoint współdzielą jeden budżet wykonania;
budżet nie jest odnawiany dla każdej reakcji.

Zgodnie z protokołem Test262 worker obserwuje pierwszy argument `print`.
Zaliczenie wymaga dokładnie jednego `Test262:AsyncTestComplete` i braku
`Test262:AsyncTestFailure:`. Powtórzone zakończenie lub znacznik błędu oznaczają
niepowodzenie. Obserwacja protokołu działa także po zapełnieniu ograniczonego
logu diagnostycznego. Jeżeli kolejka stanie się pusta bez znacznika zakończenia,
wynik to `async_incomplete`, a nie zaliczenie ani automatyczny timeout.

Po opróżnieniu kolejki sprawdzane są nieobsłużone odrzucenia. Worker wcześniej
zachowuje i rootuje oryginalny prototyp `Test262Error`; porównuje rzeczywisty
łańcuch prototypów przyczyn odrzucenia, bez polegania na modyfikowalnym `name`,
getterach czy hookach `instanceof`. Wyłącznie w testach z flagą `async`
(`async_test`) nieobsłużona asercja z tym prototypem oznacza niepowodzenie,
także po `$DONE()`. W testach synchronicznych wszystkie nieobsłużone odrzucenia
pozostają diagnostyką (`unhandled_rejections`): testy Promise dotyczące m.in.
zamykania iteratorów celowo używają `Test262Error` jako przyczyny odrzucenia.
Pozostałe przyczyny odrzucenia są diagnostyczne również w testach `async`.
Handler dołączony przed checkpointem usuwa wpis źródłowego Promise z listy
nieobsłużonych odrzuceń.

Checkpoint flagi `async` obsługuje także wznowienia funkcji asynchronicznych.
Timery i zewnętrzne hosty workerów/agentów pozostają nieobsługiwane.
Nieobsługiwane produkcje mają wynik `unsupported`. Błąd podczas wykonywania
oryginalnego harnessu pozostaje `harness_error`.

Raport zawiera przypiętą rewizję, SHA-256 pliku roboczego, zakres wyboru, wyniki
wszystkich wariantów i ograniczenia. Udział zaliczeń wybranego zakresu nie jest
wynikiem pełnego Test262. Dotyczy to także łącznego uruchomienia katalogów JSON
i Promise: nie jest to cały zestaw. Kod wyjścia 1 oznacza co najmniej jeden wynik inny
niż zaliczenie; 2 oznacza błąd konfiguracji lub runnera. Nie należy zastępować
oryginalnego harnessu uproszczonymi asercjami w celu podniesienia wyniku.

Opcje `--path` można powtarzać; ścieżki są względne do katalogu `test`.
`--limit` i `--offset` tworzą jawny podzbiór. `--timeout-ms` domyślnie wynosi
3000, a `--instructions` 1000000. Pliki `_FIXTURE` nie są testami.

## Granice procesu roboczego

Runner wysyła najwyżej jedno oczekujące żądanie do własnego procesu `rb-js`.
Żądanie JSONL ma limit 16 MiB. Worker ogranicza zadany budżet instrukcji do
zakresu 1–100000000, liczy również natywną pracę i kopiowanie tekstu oraz kończy
checkpoint po najwyżej 10000 zadaniach. Domyślna kolejka VM mieści do 4096
zadań; obowiązują także limity reakcji i nieobsłużonych odrzuceń. Log obejmuje
do 64 wpisów, z podglądem do 2048 jednostek UTF-16 na wpis.

VM ma domyślnie 64 MiB szacowanego budżetu alokacji i 100000 komórek sterty,
a także osobne limity źródła, tekstu i zagnieżdżenia. Budżet alokacji jest
konserwatywnym oszacowaniem, nie pomiarem RSS. Szczegóły i ograniczenia parsera,
Promise oraz JSON opisuje [dokumentacja silnika](../../docs/JAVASCRIPT.md).
Przekroczenie zasobów jest raportowane jako `resource_limit`. Zewnętrzny
deadline zatrzymuje wyłącznie proces roboczy utworzony przez runner; wynik
`timeout` lub `crash` pozostaje w raporcie i nigdy nie zmienia się w zaliczenie.

Sam runner ma niezależne testy klasyfikacji wyników:

```powershell
npm --prefix tools/test262 test
```
