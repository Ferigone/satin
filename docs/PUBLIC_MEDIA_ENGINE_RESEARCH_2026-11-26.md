# YouTube i Twitch w Satin: jak rozwiązują to inne silniki

Stan źródeł i pomiarów: 26 listopada 2026. To notatka badawcza, nie wynik
wdrożenia. Badanie obejmuje Blink/V8, Gecko/SpiderMonkey i WebKit/JavaScriptCore.
Kod upstream opisuje mechanizmy silnika; nie zakładamy, że każdy przełącznik
kompilacji w `main` jest aktywny w przetestowanych wydaniach przeglądarek.

## Wniosek

Nie kopiować limitu z innego silnika ani nie dopasowywać liczby do jednej strony.
Dojrzałe silniki ograniczają **rozmiary wejść, stos, pamięć i czas na różnych
poziomach**, ale zarazem unikają budowania danych, których nie potrzebują,
zwalniają pamięć tymczasową wraz z końcem fazy i oddzielają proces treści od
interfejsu. Dla Satin to kolejność działań, nie gotowe parametry: wspólne wejścia
testowe i pomiar → zmiana konkretnego mechanizmu → test zgodności i przeciążenia
→ ponowna próba na żywych stronach.

## 1. Kompilacja JavaScriptu i pamięć

| Silnik | Co robi przy pierwszym odczycie źródła | Co robi później |
| --- | --- | --- |
| **V8** | Pełny parser i `PreParser` używają wspólnej gramatyki `ParserBase`. `PreParser` nie tworzy pełnego AST zimnej funkcji: sprawdza składnię i wczesne błędy, zbiera deklaracje, użycia i informacje o domknięciach. Heurystyki mogą w pełni kompilować funkcję wyglądającą na natychmiast wywoływaną (IIFE). | Pierwsze potrzebne wywołanie buduje pełny kod. `PreparseData` przechowuje m.in. informacje o zasięgach i podfunkcjach, żeby przy tym nie przetwarzać od nowa wszystkich poziomów zagnieżdżenia. [Opis V8](https://v8.dev/blog/preparser) · [kod `PreParser`](https://chromium.googlesource.com/v8/v8/+/b89397c5aaac96b6a5b2a3a1bf6873f9abfe1829/src/parsing/preparser.cc) |
| **SpiderMonkey** | `SyntaxParseHandler` i `FullParseHandler` obsługują lżejsze oraz pełne parsowanie. Lżejsza ścieżka nadal musi wykryć wczesne błędy. Gdy danej konstrukcji nie da się tak obsłużyć, kod może wrócić do pełnego parsowania. Frontend tworzy `Stencil` (bajtkod i metadane, niezależne od obiektów GC); wykonanie wymaga instancjacji odpowiednich obiektów. | `DelazifyCanonicalScriptedFunction` jest ścieżką pełnej kompilacji funkcji potrzebnej do wykonania. Gecko ma także ścieżkę kompilacji skryptu poza głównym wątkiem. [Dokumentacja](https://firefox-source-docs.mozilla.org/js/) · [frontend](https://github.com/mozilla-firefox/firefox/blob/main/js/src/frontend/BytecodeCompiler.cpp) · [fallback parsera](https://github.com/mozilla-firefox/firefox/blob/main/js/src/frontend/Parser.cpp#L3160-L3195) · [ScriptLoader](https://github.com/mozilla-firefox/firefox/blob/main/dom/script/ScriptLoader.cpp#L2255) |
| **JavaScriptCore** | `Parser` ma ścieżki używające `SyntaxChecker`; kod jawnie zbiera informacje o zmiennych przechwytywanych z zewnętrznych zakresów. Jeżeli `SourceProviderCache` już ma ważne metadane funkcji, parser może przejść na jej koniec bez ponownego parsowania tego ciała. | Gdy potrzebny jest blok funkcji i nie ma go w cache, ścieżka `UnlinkedFunctionExecutable` parsuje ciało i generuje bajtkod. JSC osobno zmniejszył koszt *przechowywanego* bajtkodu, stosując zwarte instrukcje i tablicę metadanych. [Parser](https://github.com/WebKit/WebKit/blob/b7e84a4224b3934868bc08f5c89b583355a6c87a/Source/JavaScriptCore/parser/Parser.cpp#L2359) · [generowanie funkcji](https://github.com/WebKit/WebKit/blob/b7e84a4224b3934868bc08f5c89b583355a6c87a/Source/JavaScriptCore/bytecode/UnlinkedFunctionExecutable.cpp#L50) · [format bajtkodu](https://webkit.org/blog/9329/a-new-bytecode-format-for-javascriptcore/) |

Te strategie mają wspólną istotę: **nie tylko wyrzucać bajtkod po wygenerowaniu,
ale kiedy jest to bezpieczne, w ogóle go nie generować na starcie**. Nie wolno
natomiast po prostu przeskoczyć do następnego `}`. Ścieżka uproszczona musi
respektować [wczesne błędy `ParseScript`](https://tc39.es/ecma262/multipage/ecmascript-language-scripts-and-modules.html#sec-parse-script),
zasięgi, tryb strict, domknięcia, kontekst `super`/`new.target`, literały
szablonowe, rozróżnienie literału RegExp i dzielenia oraz widoczny tekst
`Function.prototype.toString()`. Tryb pełny i uproszczony nie powinny rozjeżdżać
się w gramatyce. Gdy metadane nie pozwalają na pewny wybór, potrzebna jest
ścieżka pełna. Walidacja musi się zakończyć przed efektami wykonywania skryptu.

### Czego dotyczą ich limity

- **Limit wejścia nie jest limitem pamięci kompilatora.** Blink w
  [`V8ScriptRunner`](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/bindings/core/v8/v8_script_runner.cc#L350-L366)
  sprawdza długość tekstu skryptu klasycznego względem `v8::String::kMaxLength`.
  To ograniczenie reprezentacji wejścia, nie obietnica skompilowania skryptu
  o takim rozmiarze. [Dokumentacja ograniczeń SpiderMonkey](https://firefox-source-docs.mozilla.org/js/constraints.html)
  również wyróżnia maksymalną długość ciągu, podając osobny koszt składowania
  reprezentacji jedno- i dwubajtowych.
- **Sterta JS i tymczasowy frontend to różne pule.** V8 ma
  [`ResourceConstraints`](https://github.com/v8/v8/blob/main/include/v8-isolate.h#L69-L137),
  w tym konfigurowanie na podstawie pamięci fizycznej/wirtualnej, limit
  generacji starej oraz GC i callback przy zbliżeniu do limitu. Dla części
  struktur tymczasowych kompilatora używa
  [aren `Zone`](https://chromium.googlesource.com/v8/v8/+/refs/heads/main/docs/heap/zone-allocator.md),
  zwalnianych grupowo po fazie.
  [`AccountingAllocator`](https://github.com/v8/v8/blob/main/src/zone/accounting-allocator.h#L33-L61)
  śledzi bieżące i maksymalne zużycie segmentów. W SpiderMonkey osobnym
  mechanizmem jest [`JSGC_MAX_BYTES`](https://github.com/mozilla-firefox/firefox/blob/main/js/public/GCAPI.h#L67-L84);
  frontend używa m.in. `LifoAllocScope`. Żadna z tych liczb nie jest przelicznikiem
  „tyle bajtów za token” ani limitem prywatnej pamięci całego procesu.
- **Przetwarzanie w tle i cache rozwiązują inne problemy.** Blink może
  [przekazywać fragmenty skryptu do V8 w trakcie pobierania](https://chromium.googlesource.com/chromium/src/+/5666ff4f5077a7e2f72902f3a95f5d553ea0d88d/third_party/blink/renderer/bindings/core/v8/script_streamer.h)
  i [czytać cache kodu poza głównym wątkiem](https://chromium.googlesource.com/chromium/src/+/5666ff4f5077a7e2f72902f3a95f5d553ea0d88d/third_party/blink/renderer/bindings/core/v8/script_cache_consumer.h).
  Cache nie pomaga pierwszemu uruchomieniu, a równoległość sama w sobie nie
  obniża szczytu pamięci: kilka równoległych kompilacji może ten szczyt podnieść.
  [V8 może też odzyskiwać rzadko używany bajtkod](https://v8.dev/blog/v8-release-74#bytecode-flushing),
  co dotyczy czasu po kompilacji, nie samego szczytu pracy parsera.

## 2. Wykonanie JS, gorące operacje i zawieszanie strony

- **Chromium:** [architektura wieloprocesowa](https://www.chromium.org/developers/design-documents/multi-process-architecture/)
  oddziela proces renderera od interfejsu. W kodzie istnieje
  [monitor czasu potwierdzenia wejścia i pingu renderera](https://github.com/chromium/chromium/blob/main/components/input/input_constants.h#L11-L25);
  wartości z tego pliku nie są czasem przyznanym każdemu skryptowi.
  V8 udostępnia osadzającemu osobne
  [`RequestInterrupt` i `TerminateExecution`](https://github.com/v8/v8/blob/main/include/v8-isolate.h#L1297-L1342).
  Samo istnienie tych funkcji nie dowodzi określonej polityki produktu Chrome.
- **Firefox:** [kod `XPCJSContext`](https://github.com/mozilla-firefox/firefox/blob/main/js/xpconnect/src/XPCJSContext.cpp)
  ma watchdog działający przez `JS_RequestInterruptCallback`. Ścieżka
  powolnego skryptu sprawdza czas, kontekst (treść, kod przeglądarki,
  rozszerzenie) i zależnie od ustawień oczekujące wejście użytkownika.
  Może zaproponować przerwanie albo dalsze oczekiwanie. To osobne od
  [izolacji procesów treści](https://firefox-source-docs.mozilla.org/dom/ipc/process_model.html)
  i limitów stosu/GC.
- **JSC/WebKit:** [`Watchdog.cpp`](https://github.com/WebKit/WebKit/blob/main/Source/JavaScriptCore/runtime/Watchdog.cpp)
  wykorzystuje monotoniczny timer do obudzenia VM, ale po obudzeniu sprawdza
  zużyty czas CPU wątku. Callback osadzającego może rozstrzygać o przerwaniu.
  Jest to mechanizm JSC, nie dowód aktywnego, identycznego limitu każdej strony
  w Safari. [Proces WebContent jest odseparowany od UI](https://docs.webkit.org/Deep%20Dive/Architecture/WebKit2.html).

[HTML pozwala przerwać niedający się dokończyć skrypt](https://html.spec.whatwg.org/multipage/webappapis.html#killing-scripts).
To nie znaczy, że monolityczne wykonanie można dowolnie pociąć i wpuścić
zdarzenia DOM w środek synchronicznego kodu. Wewnętrzne punkty kontrolne nie
powinny zmieniać kolejności efektów, mikrozadań, zdarzeń ani observerów.

### Co to mówi o kosztach znalezionych w Satin

- [V8 używa wspólnego kształtu obiektu (hidden class)](https://v8.dev/docs/hidden-classes),
  [SpiderMonkey opisuje `GuardShape` i odczyt slotu w CacheIR](https://firefox-source-docs.mozilla.org/js/cacheir.html),
  a [JSC używa `StructureID` i specjalizacji `get_by_id`](https://webkit.org/blog/9329/a-new-bytecode-format-for-javascriptcore/).
  Sprawdzają poprawność założenia, po czym czytają znany slot. Nie jest to cache
  zapamiętujący wartość dla każdego obiektu. Cache Satin chroniony tożsamością
  może wygrywać dla **tego samego obiektu**, ale nie ma automatycznie zysku
  dla wielu różnych obiektów z tym samym kształtem. Bajtkod `LoadProperty`
  usuwa realne kroki dispatchu; to także nie jest jeszcze cache kształtów.
  Przed projektowaniem takiego cache trzeba zmierzyć częstość trafień i
  zmutowanych obiektów, rozdzielić own/prototype/getter/Proxy/host i testować
  unieważnianie, GC oraz kolejność efektów.
- Rozpoznanie `RegExp` jako lidera **licznika** nie wskazuje jeszcze rodzaju
  wyrażenia, backtrackingu ani lidera **czasu CPU**. V8 rozwija
  [Irregexp](https://v8.dev/blog/speeding-up-regular-expressions) i ma
  [dodatkowy tryb bez backtrackingu](https://v8.dev/blog/non-backtracking-regexp)
  tylko dla kwalifikujących się wzorców i w opisywanej konfiguracji.
  [Flaga fallbacku w sprawdzonym kodzie `main`](https://github.com/v8/v8/blob/main/src/flags/flag-definitions.h#L3642-L3650)
  jest eksperymentalna. Nie wyłączać meteringu ani nie zakładać, że każde
  wyrażenie JS da się bez zmiany semantyki dopasować liniowo. Najpierw zmierzyć
  kompilowanie, skanowanie wejścia, dopasowania, kroki backtrackingu i alokacje
  w osobnych koszykach, bez zapisywania tekstu wzorców z witryny.
- W Satin [domyślny interpreter](../crates/javascript/src/vm.rs#L118)
  ma 1 mln jednostek; instrukcje i praca natywna schodzą z tego samego
  budżetu. [Zadania wejścia i timerów](../crates/engine/src/script/scheduler.rs#L16)
  mają odrębne limity pracy i watchdog czasu. To już pokazuje, że polityka
  zależy od rodzaju zadania. Przerobienie polityki startu strony na ślepą
  kopię limitu timerów nie ma podstaw: potrzebne są czasy CPU, czas pomiędzy
  punktami kontroli oraz pomiary wpływu na cały interfejs i inne karty.

## 3. Dane dla Satin: co potwierdzono, a czego nie

| Próba | Potwierdzony wynik | Czego nie dowodzi |
| --- | --- | --- |
| `7f4e3a7`, `target/public-media/satin-DkHnr7/` | YouTube: odrzucenie skryptu przy 2 536 233 z 10 935 744 punktów kodowych; początkowy budżet strukturalny 88 731 616 bajtów. Bufor źródła zajmował według jego reprezentacji 43 742 976 bajtów. | Licznik 384 bajtów dla większości tokenów nie jest szczytem rzeczywiście zaalokowanej pamięci. Ścieżka wyłącznie ASCII nie zadziałała dla tego wejścia. |
| `efe6c4a`, `target/public-media/satin-GViLAo/` | Nadal dwa typy blokad YouTube; błąd kompilacji przy 3 329 101 z 10 941 666 punktów. Twitch wyczerpał pracę natywną. | Różna długość wejścia i rezerwa między sesjami nie pozwalają porównać pozycji awarii jako czystego A/B na tych samych bajtach. |
| `405d69c`, `target/public-media/satin-eTZLp7/` | YouTube: 51 wykonanych skryptów zamiast 50, bez zgłoszonego błędu pamięci kompilatora w tej próbie. Wciąż limit instrukcji skryptu inline nr 293 i drugi niesklasyfikowany wpis; brak gotowości i klatek. Twitch: 118 skryptów, wyczerpanie pracy natywnej na `core`, bez odtwarzacza lub czatu. | Nie dowodzi rozwiązania limitu kompilacji na identycznym zasobie ani uruchomienia odtwarzania. `Other` nie mówi, czy padł konkretny nowy interfejs. |
| Chrome 158.0.8127.88, `target/public-media/chrome-lpo2Fq/` | Twitch: 28 → 277 klatek; YouTube: 53 → 198. W obu rosły zdekodowane bajty audio, działała pauza/wznowienie; YouTube można było przewinąć. | Nie dowodzi słyszalnego wyjścia, kodeka obrazu ani czatu w tej sesji. To pozytywna kontrola żywego serwisu, nie porównanie tego samego bufora JS. |
| Kontrola natywna `405d69c`, `target/native-media-BAclQg/` | Przeszły cztery lokalne scenariusze MP4/MSE (postęp, pauza, seek, wznowienie, ukończenie lub rebuffer). Przeszły też 172 skupione testy Rust i cztery testy sanitizera. | Znany strumień H.264/AAC nie weryfikuje negocjacji formatu, sieci, polityki autoplay ani kodu obu publicznych serwisów. |

[Lokalny kod wyznaczania budżetu](../crates/javascript/src/vm.rs#L377) dzieli
pamięć inaczej niż opisane silniki: z limitu VM odejmuje oszacowaną pamięć
sterty i rezerwę źródła, a lexer odpisuje szacunkowe koszty strukturalne.
Limit klasycznego wejścia to osobno 12 MiB, a limit księgowy przeglądarkowej
VM to 512 MiB. [Ostatnia poprawka](../crates/javascript/src/parser/cold_functions.rs)
sprawdza i tymczasowo kompiluje kwalifikujące się funkcje, potem odrzuca ich
bajtkod do pierwszego wywołania. Pomogła w kontrolowanej paczce przy
niezmienionym limicie; **nie jest** odpowiednikiem lekkiego preparsera V8.

## 4. Odtwarzanie: za parserem jest osobny problem

W [Chromium](https://github.com/chromium/chromium/blob/main/media/README.md#playback)
odtwarzacz sieciowy `src=` i MSE mają różne demuksery; przy MSE bajty przekazuje
JavaScript. Dekodery sprzętowe i programowe wybiera osobna warstwa; zegar i
renderowanie obrazu są jeszcze dalej. [Dokumentacja diagnostyki Firefox](https://firefox-source-docs.mozilla.org/media/Diagnostics.html)
rozróżnia Windows Media Foundation, typ kodeka i rzeczywisty dekoder (WMF
nie ma tej samej ścieżki programowej dla każdego kodeka).
[WebKit opisuje MSE](https://docs.webkit.org/Deep%20Dive/Modules/MediaSourceExtensions.html)
oraz [MSE/MMS w Workerach](https://webkit.org/blog/15865/webkit-features-in-safari-18-0).
Pięć Blob Workerów zaobserwowanych w jednym przebiegu Twitcha nie mówi, że
każdy z nich użył `MediaSourceHandle` albo WebCodecs.

[MSE](https://www.w3.org/TR/media-source-2/) określa m.in. formaty wejścia,
dołączanie buforów, zakresy, usuwanie klatek i powiązanie z Workerem.
[Media Capabilities](https://www.w3.org/TR/media-capabilities/) osobno ocenia
`supported`, `smooth`, `powerEfficient`. `canPlayType()` lub `isTypeSupported()`
to odpowiedź na pytanie o format, a nie dowód dekodowania próbki bądź
wyrenderowania klatki. [Polityka autoplay w Chrome](https://developer.chrome.com/blog/autoplay)
i [WebKit](https://webkit.org/blog/7734/auto-play-policy-changes-for-macos/)
to osobna przyczyna, widoczna m.in. w wyniku `play()`. Tak samo osobno
klasyfikować stan offline kanału, cookie consent, reklamy, błąd HTTP/CORS,
brak adaptera/konfiguracji dekodera i błąd layoutu.

[YouTube](https://support.google.com/youtube/answer/78358?hl=en) i
[Twitch](https://help.twitch.tv/s/article/supported-browsers) nie wymieniają
Satin jako wspieranego produktu. To ograniczenie gwarancji z ich strony, nie
dowód blokady na podstawie User-Agent. Nie podszywać się pod inny silnik i nie
deklarować obsługi API lub kodeka, którego Satin nie może użyć.

## 5. Plan z warunkami decyzji przed następną zmianą

1. **Punkt odniesienia:** archiwizować SHA binarki i konfiguracji, build
   zoptymalizowany kontra ten sam build z ograniczoną telemetrią, te same
   profile startowe, akcje zgód i rozmiar 1280×800. Zamrozić legalny zestaw
   wejść do replayu (w tym ręczne redukcje). Dla zmiennego serwisu łączyć
   etapy za pomocą rozmiaru i skrótu bajtów, kontekstu Script/Module/Worker
   i kolejności odpowiedzi — nie numeru węzła ani hasha nazwy CDN. Jeżeli
   wejścia nie są zgodne, mówić o dwóch obserwacjach, nie o A/B. Artefakt
   publiczny ma zawierać liczniki i skróty, bez tokenów, czatu i treści kont.
   Po Chrome dodać Firefox na tym samym Windows, jeśli dostępny; Safari/WebKit
   nie traktować jako równoważnego pomiaru Windows.
2. **Kompilator:** osobno mierzyć rozmiar po dekompresji, żywy stan przed/po
   GC, liczbę tokenów, tymczasową pamięć parsera i emitera, trwały bajtkod
   wraz ze źródłem i szczyt całego procesu. Na Windows oddzielić
   [working set od private commit](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex);
   oba różnią się od księgi VM. Zacząć od wielu małych ciał, pojedynczego
   dużego IIFE, mocnego zagnieżdżenia, gęstej składni, długich literałów,
   Unicode i niezależnych dużych modułów. **Wybrać preparser**, jeżeli na
   identycznych wejściach dominuje praca nad nieużywanymi funkcjami;
   **wybrać zmianę struktur lub rozliczeń**, jeżeli dominuje pojemność
   kontenerów, kopiowanie tekstu lub równoczesne trzymanie AST i bajtkodu.
3. **Wykonanie:** w zoptymalizowanym buildzie obok liczby jednostek mierzyć
   czas CPU, wall time, koszt hosta i najdłuższy odcinek bez możliwości
   przerwania. Zestawić wiele powtórzeń i koszyk niezależnych aplikacji;
   [Speedometer 3.1](https://browserbench.org/Speedometer3.1/about.html)
   jest dodatkową miarą responsywności, nie testem YouTube/Twitch.
   **Zmieniać politykę czasu startupu dopiero**, gdy legalna praca jest
   realnie krótka względem limitu, przerwanie pozostaje skuteczne dla pętli,
   RegExp, zagnieżdżonych callbacków i anulowania nawigacji, a zegar UI i
   inne karty nie regresują. Zachować oddzielne bezpieczniki pracy, stosu,
   tablic/buforów i sumy pamięci. Nie przenosić ustawień innego silnika bez
   jego modelu procesów i punktów mierzonych.
4. **Zgodność:** negatywne przypadki parse/early error według
   [Test262](https://github.com/tc39/test262/blob/main/INTERPRETING.md),
   m.in. strict, domknięcia, `eval`/`with`, Annex B, ścieżki async/class
   i reset po odrzuconym wejściu. Następnie
   [WPT dla MSE](https://github.com/web-platform-tests/wpt/tree/master/media-source)
   i inne testy właściwe dla dotkniętej warstwy. Nie zmieniać mianownika ani
   nie zaliczać awarii harnessu jako zaliczonych asercji.
5. **Próg „działa”:** dopiero po uruchomieniu skryptu śledzić żądanie manifestu,
   statusy segmentów, wybraną konfigurację MSE/Worker, demukser, dekoder i
   wynik `play()`. W natywnym oknie wymagane są dwa pomiary tego samego
   odtwarzania z rosnącym czasem **i** klatkami, osobny sygnał aktywnego
   dekodowania audio i próba słyszalnego wyjścia, pauza/wznowienie, seek
   tam, gdzie jest zakres, oraz czat Twitch tylko do odczytu. Po tym
   weryfikować CSS kontrolek, kliknięcia, klawiaturę, focus i rozmiary.
   Kadr, pojedyncza próbka, reklamowany kodek i brak błędu nie spełniają progu.

**Decyzja po researchu:** na razie bez kolejnego patcha silnika, wyjątków
domenowych ani zmiany limitu. Pierwszy następny pomiar to identyczny replay
startu na wersjach `efe6c4a` i `405d69c` oraz pomiar CPU rzeczywistej pracy
`core` Twitcha i inline YouTube. Dopiero te wyniki rozstrzygną, która warstwa
jest następnym wąskim gardłem. [Dziennik dotychczasowych prób](PUBLIC_MEDIA_QA_2026-11-26.md).
