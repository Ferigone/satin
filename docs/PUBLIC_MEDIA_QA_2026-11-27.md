# Satin na Windows x64 · weryfikacja 26/27 listopada 2026

**Na końcowej wersji Satin oba publiczne odtwarzacze nadal zatrzymują się podczas startu JavaScript.** Kompilator przeszedł próg, który wcześniej odrzucał duży skrypt YouTube. Naprawione zostały również kodowanie skryptów klasycznych i brak rzeczywistego interfejsu HTMLTemplateElement. Kod silnika znajduje się na lokalnej gałęzi `satin/public-media-compat`, w commicie `8f54325a4859aeaa1785c17329465bdd48f0d36c`.

## Rzeczywiste strony

W zwykłym buildzie okno próbkowano po dwóch kolejnych opóźnieniach 8 s i 5 s. W osobnej anonimowej nawigacji wykonano trzy odczyty po kolejnych opóźnieniach 12 s, 10 s i 10 s. Każda seria zachowała ten sam identyfikator nawigacji.

**Twitch.** W diagnostycznej nawigacji Satin wykonano 118 skryptów. Skrypt `core` wyczerpał budżet pracy natywnej VM. We wszystkich pięciu odczytach z dwóch sesji okna było zero elementów wideo i zero zdekodowanych klatek. Tekst `core` miał 408697 bajtów i SHA-256 `06e2a96c16f867bcca3666e8cf123668db7171875ad5e46943e22c80d27288ec`. To ten sam tekst, który pobrano z anonimowego Chromium. W DOM było 0 znaczników wiadomości czatu.

**YouTube.** W diagnostycznej nawigacji Satin wykonano 50 skryptów. Skrypt inline i zewnętrzna paczka wyczerpały budżet instrukcji podczas wykonania. We wszystkich pięciu odczytach z dwóch sesji okna było zero elementów wideo i zero zdekodowanych klatek. Na wcześniejszym buildzie ten sam duży skrypt o SHA-256 `3de22fade2f09836a30cb0a75db78f3eb72288b795ffdfe9f9ef08f81da129e5` najpierw kończył się limitem kompilatora, a po poprawie pamięci doszedł do odwołania do `HTMLTemplateElement`. Nowy interfejs korzysta z istniejącego parsera szablonów i odłączonego fragmentu DOM, obejmując również głębokie klonowanie i import. [HTML — element template](https://html.spec.whatwg.org/multipage/scripting.html#the-template-element)

Satin nie udostępnił działających publicznych kontrolek pauzy, wznowienia ani przewijania. W pomiarze renderowania audio żaden z tych dwóch procesów Satin nie dostarczył próbki potwierdzającej sygnał. CORS, Workers, MSE, kodeki i CSS nie zostały ustalone jako przyczyna tych konkretnych zatrzymań.

W dodatkowych sesjach po naprawie prototypu skrypt inline YouTube nadal miał dokładnie 1145 bajtów i SHA-256 `3e033849bb8218c4d82825569af753876005483fb73b8578654dd9fca2ed0e94`. Dwa duże skrypty o różnych skrótach doszły do limitu wykonania. W innej sesji wykonano 51 skryptów i zarejestrowano tylko błąd inline. Nie ustalono identyczności wszystkich innych zasobów i stanu dokumentu między tymi sesjami.

**Kontrola Chrome 158.0.8127.88.** Test rozpoczął się o `2026-11-26T23:54:41.860Z` UTC, test okna Satin o `2026-11-26T23:52:17.498Z`. Wyniki z jednego odtwarzacza w każdej nawigacji podano poniżej.

| Strona | Czas filmu [s] | Zdekodowane klatki | Zdekodowane bajty audio | Pauza / wznowienie | Przewijanie | Znacznik reklamy podczas próby |
| --- | --- | --- | --- | --- | --- | --- |
| Twitch | 0,406 → 5,256 | 16 → 259 | 7596 → 91274 | potwierdzone | brak dostępnej kontrolki z zakresem | brak w obu próbkach |
| YouTube | 1,541 → 6,342 | 50 → 194 | 25686 → 95379 | potwierdzone | 317,28 s | brak w obu próbkach |

W dodatkowej świeżej sesji Chrome miernik Windows przypisany do procesów tej instancji wykazał dodatni sygnał na obu stronach podczas gry i po wznowieniu oraz zero podczas pauzy. Przy pierwszej próbie dźwięku YouTube tester przekroczył czas oczekiwania. Powtórzenie przeszło po odrzuceniu opcjonalnych ciasteczek i sprawdzeniu stanu odtwarzacza. To pomiar sygnału cyfrowego w mikserze. Nie wykonano odsłuchu na głośnikach. [Odczyt miernika Windows](https://learn.microsoft.com/en-us/windows/win32/api/endpointvolume/nf-endpointvolume-iaudiometerinformation-getpeakvalue)

W kontrolnej sesji Chrome maksymalna liczba widocznych znaczników wiadomości czatu Twitch wyniosła 0. Pomiar dotyczy odczytu. Nie używano konta ani nie wysyłano wiadomości.

W pierwszej z końcowych prób Chrome Twitch pozostał w pauzie po kliknięciu. Czas utrzymał się na 0 s, licznik na czterech klatkach, a liczba widocznych wiadomości czatu wzrosła do 11. Zachowano wynik tej sesji. W kolejnej próbie tester mógł raz ponowić kliknięcie, jeżeli po ustabilizowaniu odtwarzacza jego stan nadal wskazywał pauzę.

## Powtarzalne wyniki niezależne od sesji sieciowej

| Próba | Punkt odniesienia | Wybrany wariant | Znaczenie |
| --- | --- | --- | --- |
| 10,36 MB komentarza ASCII, kompilacja Satin | 118,9 ms [115,1–124,0] | 6,4 ms [6,1–7,4] | Cztery przeplatane rundy. Szczyt w obu wynosił 10,361 MB. |
| 90 000 prostych instrukcji | 74,489 MB | 51,071 MB | Szczyt alokacji mniejszy o 31,4%. Parsowanie nadal osiągało 34,419 MB. |
| Zachowany skrypt YouTube, pełna sterta | 183,198 MB | 133,340 MB | Szczyt niższy o 27,2% na tym samym pliku o rozmiarze 10,94 MB. Cztery sukcesy każdego wariantu. |
| Ten sam plik, maks. 416 MiB w pustej VM próbnika | 0/6, `HeapLimit` | 6/6, `ok` | Rozliczenie gotowego źródła i tokenów bez zwrotu zapasu literałów też dało 0/6. W udanym wariancie mediana wyniosła 3,282 s [3,061–3,971]. Limit wybrano jako przybliżenie dostępnej pamięci dokumentu. |
| RegExp z obowiązkowym początkiem, 80 wyszukań i kontrola trafienia | 2,49 ms [1,62–3,29] | 0,98 ms [0,90–2,96] | Osiem rund, obie wersje zwróciły prawidłowy wynik. Mediany przypadków kontrolnych z flagą wielowierszową lub alternatywą pozostały blisko 1 ms. |

**Porównanie z V8 ma wąski zakres.** Na identycznym źródle z jednym komentarzem ASCII o rozmiarze 10,36 MB i wyrażeniem `true` kompilacja zajęła w Satin 6,30 ms [5,99–7,34], a w Node 15,23 ms [14,87–19,36]. Osiem świeżych procesów na silnik, kolejność zmieniana co rundę. Przy rozłożonych znakach Unicode Satin potrzebował 27,46 ms [26,38–28,58], a Node 4,28 ms [4,05–5,87]. Przy 90 000 instrukcji było to odpowiednio 198,34 ms [188,00–223,46] i 18,96 ms [18,25–21,22]. Różne strategie odraczania funkcji mają znaczenie szczególnie w ostatniej próbie.

**Regresje i rozrzut.** W końcowym czterorundowym A/B mediana dla płaskiej tablicy wzrosła ze 141,7 do 156,4 ms, przy spadku szczytu z 40,896 do 28,313 MB. W pliku `youtube-asset-2` czas wzrósł z 50,4 do 64,2 ms. Dla statycznego `core` Twitcha mediana zmieniła się ze 171,3 do 172,2 ms, mimo spadku szczytu z 26,311 do 19,125 MB. Dla gęstego kodu kierunek różnicy czasu zmieniał się między seriami. Główną podstawą wyboru emitera jest powtarzalny spadek pamięci.

Przeplatany replay oryginalnych binarek potwierdził koszt `d9571f5`: dla komentarza ASCII mediana wyniosła 62,2 ms [54,8–81,0] przed zmianą i 135,8 ms [128,1–144,4] po zmianie. Dla mieszanej paczki funkcji szczyt spadł z 16,070 do 2,982 MB, a czas wzrósł z 1365,6 ms [1282,6–1515,8] do 1534,5 ms [1444,0–2050,7].

Syntaktycznie błędna nieużywana funkcja zachowała klasę `SyntaxError`. Nadmiernie zagnieżdżony przykład zachował `ExecutionLimit` w Satin, podczas gdy V8 go skompilował. Testy Rust sprawdziły m.in. domknięcia po GC, wartości zakończenia bloków, ramy generatorów i dalsze wykonanie po odmowie kompilacji. Te wyniki mają osobne klasy w tabelach.

## Czas CPU i jednostki VM na publicznych stronach

**Twitch, core.** Całe zadanie zajęło 421,9 ms CPU wątku i 419,1 ms czasu ściennego. Matcher RegExp — 2,5% jednostek VM i 0,4% cykli. Odczyty nazwanych właściwości — 9,8% jednostek VM i 12,0% cykli. Wywołania hosta — 0,0% jednostek VM i 0,2% cykli.

**YouTube, skrypt inline.** Całe zadanie zajęło 203,1 ms CPU wątku i 203,4 ms czasu ściennego. Matcher RegExp — 21,9% jednostek VM i 8,2% cykli. Odczyty nazwanych właściwości — 12,8% jednostek VM i 27,7% cykli. Wywołania hosta — 0,2% jednostek VM i 0,6% cykli.

Pełna tabela w załączniku do walidacji uwzględnia też wejściową kompilację i kompilację wzorców. Granice kategorii i koszt instrumentacji opisano niżej.

## Testy i kontrola lokalnych mediów

Przeszło 182 skupionych testów JavaScript, 11 testów obsługi scenariuszy okna, dwa testy dotyczące kodowania, 33 testów DOM, elementów niestandardowych i fragmentów oraz pięć testów sanitizera w Node. Lokalny serwer potwierdził kolejność BOM, nagłówka, atrybutu i kodowania dokumentu. Dla dynamicznego elementu zmiana atrybutu po przygotowaniu nie zmieniła wybranego kodowania.

Na końcowej binarce przeszły cztery rzeczywiste scenariusze lokalne MP4/MSE. progressive — 24 klatek i 2,53 s, open — 14 klatek i 1,64 s, finalized — 91 klatek i 4,83 s, rebuffer — 106 klatek i 5,37 s. W tych czterech scenariuszach autorska strona testowa wycisza element i sprawdza m.in. obecność ścieżki audio. W osobnym natywnym oknie próbka MP4 pokazała 6 → 22 klatek, pauzę przy 0,97 → 0,97 s, seek do 3,12 s i wznowienie do 3,61 s. Miernik procesu nie dostarczył dodatniej próbki. Obecność lokalnej ścieżki audio i postęp zegara są potwierdzone, wyjście akustyczne Satin pozostaje niepotwierdzone.

## Dlaczego wybrałem te zmiany

Historyczne A/B porównuje `1e9c903` z `d9571f5` w trzech świeżych procesach na wariant. Przy jednym znaku BMP albo spoza BMP w źródle o długości 10,36 MB szczyt żądanych alokacji spadł z 82,880 do 10,684 MB. Przy znakach rozłożonych w całym komentarzu pozostał blisko 30,24 MB. Szczyt dla gęstego kodu pozostał na poziomie 74,489 MB. W prostym komentarzu ASCII czas ścienny wzrósł z 73,5 [66,3–75,1] do 121,9 [120,6–129,7] ms. W paczce funkcji z nielicznymi szerokimi znakami szczyt spadł z 16,07 do 2,98 MB, lecz mediana czasu kompilacji wzrosła. W publicznym `core` Twitcha zmiana źródła obniżyła szczyt z 27,528 do 26,311 MB. W pozostałych czterech plikach statycznych szczyty były takie same. Pełne wyniki obejmują także CPU i klasy błędów.

W świeżym porównaniu `base` to ponowny build kompilatora z logiką `d9571f5`. Każdą z pięciu binarek uruchomiłem pięć razy na tych samych bajtach. Kolejność była rotowana. Dla gęstego kodu sam zwrot nadmiarowej pojemności gotowych wektorów dał szczyt 62,968 MB. Emisja prostych wyrażeń bez osobnego bloku dała 63,655 MB. Po połączeniu obu metod przeniosłem duży operand klasycznej pętli `for` poza wartość Rust `Op`. Rzadka pętla wymaga dodatkowej alokacji. Każda instrukcja zajmuje za to krótszy slot, a księgowanie VM uwzględnia operand. Powstał szczyt 51,071 MB przy identycznym szczycie parsowania 34,419 MB. Po zwolnieniu AST zostało 19,082 zamiast 52,671 MB. `StatementValue` zachowuje wartość zakończenia instrukcji także we wznawianych ramach generatorów.

Skaner komentarzy wykorzystuje dostarczony wcześniej szkic. Zintegrowałem go z granicami wycinków źródła i stronami o różnej szerokości. Końce linii, separatory Unicode, zamknięcie komentarza i pozycje błędów nadal rozstrzyga lekser. W próbach na pięciu wariantach mediana dla komentarza ASCII spadła ze 134,1 [131,1–149,5] do 7,8 [5,8–7,8] ms. Dla pojedynczego znaku BMP spadła ze 178,6 [164,7–224,5] do 10,7 [9,0–23,0] ms. Zegar CPU procesu w Windows miał skok około 15,6 ms. Zero w wynikach bardzo krótkiego procesu jest skutkiem rozdzielczości tego pomiaru.

Dla pliku YouTube o rozmiarze 10 935 726 bajtów SHA-256 tekstu przekazanego przez natywny Satin zgadza się z SHA-256 ponownie pobranych bajtów. Przy stercie 512 MiB wcześniejsza i zwarta reprezentacja przeszły wszystkie cztery kompilacje offline. Szczyt spadł z 183,198 do 133,340 MB, a stan po odrzuceniu AST z 133,882 do 83,395 MB. Mediany ścienne wyniosły 3095,6 [2838,7–3197,3] oraz 3027,2 [2967,4–3366,4] ms. Najważniejszy efekt dotyczy pamięci. Skrót tego pliku to `43d09ba053ee2df21de4ec877111225184d59281eae212e2b0656ebca732be7c`. Sesje stron potrafią pobrać inne bajty nawet przy tej samej długości.

W rozliczeniu pamięci skrypt nadal musi przejść początkową kontrolę zapasu na budowę źródła i dekodowanie literałów. Po utworzeniu bufora księgowana jest jego rzeczywista pojemność. Gdy parser zwolni AST, literały i tymczasowy bajtkod sprawdzonej funkcji, oddaje przedpłacony zapas przypadający na jej wycinek. Pozostałe metadane i tokeny wyprzedzenia obciążają budżet. Monotoniczna granica uniemożliwia ponowny zwrot za te same pozycje. Pierwsze wywołanie ponownie rezerwuje pamięć i kompiluje ciało funkcji. Pozostaje koszt pierwszej walidacji i ponownej kompilacji wywołanych funkcji.

W matcherze RegExp kompilator sprawdza, czy pierwsza obowiązkowa instrukcja po otwarciach grup to początek całego wejścia. W tym przypadku nieudana pierwsza próba kończy wyszukiwanie. Flagi lokalne, wielowierszowość, alternatywy i semantyka `lastIndex` mają oddzielne próby kontrolne. Zmiana usuwa niemożliwe pozycje, zachowując opłaty za wykonywane instrukcje.

Na Twitchu ta sama odpowiedź CDN o długości 408 697 bajtów dawała w dawnym Satin tekst UTF-8 o długości 408 761 po błędnym przejściu przez Windows-1252. Odtworzenie tej konwersji dało identyczny SHA-256. Poprawka obejmuje klasyczne skrypty zewnętrzne przygotowane przez parser i przez DOM. Wybór kodowania uwzględnia BOM, nagłówek odpowiedzi, rozpoznany `charset` elementu i kodowanie dokumentu. [HTML — przygotowanie elementu](https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element) · [HTML — pobranie skryptu klasycznego](https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-classic-script)

## Co wynika z kodu innych silników

V8 zachowuje informacje o zagnieżdżonych funkcjach i zakresach, które pozwalają odtworzyć część pracy preparsera. W Satin sam przedział źródła nie wystarczyłby do zachowania wczesnych błędów. Areny V8 zwalniają pamięć grupowo, podczas gdy w obecnym AST Satin destruktory `Rc` i bufory `Vec` nadal wymagają rozliczenia. Wybrana zmiana usuwa zmierzony koszt reprezentacji po parsowaniu. [V8 PreparseData](https://github.com/v8/v8/blob/main/src/parsing/preparse-data.h) · [V8 Zone](https://github.com/v8/v8/blob/main/src/zone/zone.h)

JavaScriptCore rozdziela strumień instrukcji i metadane, a SpiderMonkey wydziela sekcję bajtkodu. W Satin zachowałem typowane instrukcje Rust. SpiderMonkey może odczytać slot po sprawdzeniu kształtu obiektu. Własny pomiar Satin obejmuje szerszą kategorię nazwanych odczytów i nie rozstrzyga, ile dałby cache według kształtu. [WebKit — format bajtkodu](https://webkit.org/blog/9329/a-new-bytecode-format-for-javascriptcore/) · [SpiderMonkey BytecodeSection](https://github.com/mozilla-firefox/firefox/blob/main/js/src/frontend/BytecodeSection.h) · [CacheIR](https://firefox-source-docs.mozilla.org/js/cacheir.html)

Watchdog JSC łączy sprawdzenie czasu CPU i decyzję programu osadzającego. Dla Satin dodałem zegar CPU całego zadania i rozłączne kategorie cykli wątku obok jednostek VM. Cykli nie przeliczałem na milisekundy. Wersja diagnostyczna ponosi koszt odczytu zegara. Kod upstream opisuje możliwe mechanizmy. W użytym Chrome rejestrowano zachowanie odtwarzacza i dekoderów. [JSC Watchdog](https://github.com/WebKit/WebKit/blob/main/Source/JavaScriptCore/runtime/Watchdog.cpp) · [Microsoft QueryThreadCycleTime](https://learn.microsoft.com/en-us/windows/win32/api/realtimeapiset/nf-realtimeapiset-querythreadcycletime)

## Rejestr zmian na gałęzi roboczej

| Commit lub prototyp | Zmiana i dowód |
| --- | --- |
| `d9571f5` | Powtórzony po przerwanym teście punkt odniesienia. Oba serwisy zakończyły nawigację. Zapis w `target/public-media/satin-TBhZ7M/summary.json`. W `history/` zachowano wyniki przed i po zmianie magazynu źródła. |
| `e431eb8` | Pomiar czasu wątku, cykli i jednostek wykonania oraz identyfikator błędnego wejścia. Sanitizer zachowuje wybrane klasy i liczniki. Surowe zdarzenia w podsumowaniach wersji diagnostycznej. |
| `trim` | Osobny prototyp ograniczający pojemność ukończonych wektorów. Dokładna różnica w `patches/prototyp-trim.patch`, binarka w lokalnym katalogu QA. |
| `19dd76e` | Płaska emisja instrukcji wyrażeń. Osobna binarka `inline.exe` umożliwia porównanie jej z samym zmniejszeniem pojemności. |
| `59d41a6` | Dwa odczyty mediów w tym samym natywnym oknie. Pierwsze zapisy w `target/public-media/window-YNqZOd/` i `target/public-media/satin-82HihM/`. |
| `c6ba313` | Połączenie płaskiej emisji, krótszego slotu instrukcji i zwrotu zbędnej pojemności. Osobne dane `compact` w pięciowariantowym A/B. |
| `e7bea82` | Włączenie istniejącego szkicu skanera po dopasowaniu go do wycinków i stron źródła. Próby `scanner`, testy separatorów i końców stron. |
| `60c84ee` | Dziedziczenie kodowania dla klasycznych skryptów. Test dekodera, nawigacji, lokalny serwer z dwoma kodowaniami dokumentu i identyfikacja rzeczywistego skryptu Twitcha. |
| `ba5e208` | Obniżany wyłącznie w próbniku offline limit pamięci. Właściwy build przeglądarki zachowuje limity. Wariant przed poprawą rozliczenia ma etykietę `scanner` w próbie 416 MiB. |
| `dff6df2` i `c8cafa6` | Rozliczenie pojemności gotowego źródła i faktycznego okna tokenów, następnie bezpieczne zakończenie wyszukiwania po obowiązkowej kotwicy RegExp. Próbnik `account.exe` zawiera oba commity. |
| `b2336d4` | Zwrot zapasu po zwolnieniu literałów zimnej funkcji, rejestracja rzeczywistych metadanych, zachowanie kontroli pierwszego wywołania. Próby `retire`, finalne okno i osobny build ze śladem CPU. |
| `8f54325` | Wystawienie istniejącego elementu HTML template i jego odłączonego fragmentu na właściwym prototypie Web IDL po odtworzeniu błędu YouTube. Dodatkowe testy klonowania, dziedziczenia i nieprawidłowych odbiorców gettera. |

W archiwum `indeks.json` zawiera pełne identyfikatory Git i SHA-256 binarek. Patch całości obejmuje jedenaście lokalnych commitów od `d9571f5`. Commit `d9f01a5` zmienia wyłącznie tester Chrome. Jego osobna różnica znajduje się w `patches/ponowienie-pauzy-chrome.patch`. Dwa zastane nieśledzone pliki, `docs/JS_COMPILER_BENCHMARK.md` i `tools/compare-js-compiler.mjs`, pozostały w drzewie roboczym. Oryginalny plik `comment_scan.rs` zachował SHA-256 `90110174ffbcbf7bb8f7c9139a50cca9985d7cae8ff968a618f4f7db0bbc7474`.

## Odtworzenie

Repozytorium znajduje się w `C:\Users\admin\Documents\Codex\2026-11-10\do\work\satin-public-media`. Lokalne wyniki tej serii znajdują się w `target/media-engine-ab-0f89da3b/`, historyczne w `target/compiler-research-53b493485b114628ad529b0688155a87/`, a przebiegi stron w `target/public-media/`. Dokładne nazwy katalogów każdej nawigacji, pełne SHA commitów i binarek oraz skróty zarchiwizowanych plików zawiera `indeks.json` w paczce dowodowej.

Windows x64, przypięty Rust 1.99.1, blokada zależności repozytorium i zapisane w projekcie `+crt-static`. Kompilator JS zbudowano z `opt-level=2` w profilu `dev`. Pozostałe pakiety używają tego profilu. Okno końcowe korzysta z binarki bez funkcji pomiarowych VM, odrębna binarka zbiera ślady awarii. Porównania czasowe obejmują identyczne ustawienia wewnątrz każdej serii. Małe serie podają medianę, minimum i maksimum. Nie wyznaczano przedziałów ufności. Wspólny komputer, zależności sieciowe i różnice w odpowiedziach serwisów ograniczają porównania czasowe całych nawigacji. Chrome działał headless, a natywny test Satin używał pętli zdarzeń okna. Nie wyrównywano zachowania przez zmianę identyfikatora przeglądarki.

Skrócony zapis poleceń PowerShell dla lokalnego checkoutu z docelowym commitem. Do powtarzania używa się nowych nazw plików wynikowych. W tej sesji pliki wykonywalne kopiowano do katalogu serii po każdym buildzie. `$qa` oznacza katalog serii, a `$old` katalog pomiarów historycznych.

```powershell
$qa = 'target/media-engine-ab-0f89da3b'
$old = 'target/compiler-research-53b493485b114628ad529b0688155a87'
$env:CARGO_TARGET_DIR = Join-Path (Get-Location) 'target/media-diagnostic-dev'
$opt = 'profile.dev.package.rust-browser-javascript.opt-level=2'
cargo --config $opt build --locked -p rust-browser-javascript --features compiler-qa --example profile_compiler
cargo --config $opt build --locked -p rust-browser --features media-startup-diagnostics
cargo --config $opt build --locked -p rust-browser
node tools/compare-js-compiler.mjs "$old/before.json" "$old/after.json"
node tools/compare-js-compiler.mjs "$old/public-before.json" "$old/public-after.json"
node tools/profile-js-compiler.mjs paired "$old/fixtures" "$qa/replay-authored.json" 4 base "$qa/base.exe" retire "$qa/retire.exe"
node tools/profile-js-compiler.mjs paired "$qa/public" "$qa/replay-public.json" 4 base "$qa/base.exe" retire "$qa/retire.exe"
$env:SATIN_QA_HEAP_BYTES = '436207616'
node tools/profile-js-compiler.mjs paired "$qa/satin-static-7yLLKT" "$qa/replay-quota.json" 6 scanner "$qa/scanner-quota.exe" account "$qa/account.exe" retire "$qa/retire.exe"
Remove-Item Env:\SATIN_QA_HEAP_BYTES
node tools/profile-js-compiler.mjs paired "$qa/regexp-fixtures" "$qa/replay-regexp.json" 8 scanner "$qa/scanner-quota.exe" retire "$qa/retire.exe"
cargo test --locked -p rust-browser-javascript --lib --test parser_budget --test semantics --test generators --test async_generators --test for_iteration_scope --test regexp --test regexp_search_prefix
cargo test --locked -p rust-browser-engine --lib classic_script_selects_bom
cargo test --locked -p rust-browser-engine --lib parser_and_dynamic_classic_requests_snapshot
cargo test --locked -p rust-browser --bin rust-browser testing::tests
node --test tools/public-media-diagnostics.test.mjs
node tools/verify-public-media.mjs window "$qa/b233-browser.exe" b2336d418608e393bda0aac83eaf9c4617cca027
node tools/verify-public-media.mjs satin "$qa/b233-diag.exe" b2336d418608e393bda0aac83eaf9c4617cca027
$rea = Join-Path $env:TEMP 'rb-youtube-qa-20261123/rea/node_modules/rea-agents/scripts/rea.mjs'
node tools/verify-public-media.mjs chrome 'C:/Program Files/Google/Chrome/Application/chrome.exe' $rea
node tools/verify-native-media.mjs "$qa/b233-browser.exe"
cargo test --locked -p rust-browser-engine --test dom_mutations --test custom_elements --test html_fragments
node tools/verify-public-media.mjs window "$qa/template-browser.exe" 8f54325a4859aeaa1785c17329465bdd48f0d36c
node tools/verify-public-media.mjs satin "$qa/template-diag.exe" 8f54325a4859aeaa1785c17329465bdd48f0d36c
node tools/verify-native-media.mjs "$qa/template-browser.exe"
```

W tej sesji użyto istniejącego `cargo.exe` z `%LOCALAPPDATA%/RustBrowserDev/cargo/bin` oraz sąsiedniego katalogu `rustup`. Przy tworzeniu raportu ponownie sprawdzono manifesty, rozmiary plików i SHA-256 wejść oraz wszystkich porównywanych próbników. Skrypty publiczne pozostały w wyłączonym z Git katalogu QA. W archiwum są wyniki i identyfikatory pozwalające zweryfikować ich tożsamość. Skrypt `cross-runtime.mjs` wykonuje przeplatane porównanie Satin z `node:vm`. Źródło `audio-peak.cpp` odczytuje wyłącznie mierniki poziomu sesji wskazanych procesów. Zostało skompilowane lokalnie poleceniem `cl /O2 /EHsc /std:c++17 audio-peak.cpp /link ole32.lib`.
