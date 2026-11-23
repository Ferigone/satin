# Cel i etapy

Cel: niezależny silnik WWW w Rust, wzorowany na zachowaniu Chrome, pozwalający docelowo korzystać ze współczesnych stron i aplikacji. Windows jest pierwszą platformą. Optymalizacja ma równoważyć zużycie pamięci i szybkość, przy zachowaniu poprawności.

## Podetap alfa.13

MessageChannel i MessagePort przekazują klonowane dane i przenoszą porty
w obrębie dokumentu. Windows otwiera wybór do pól plikowych, a FileList
i FormData zachowują wybrane obiekty File. ImageBitmap dekoduje, skaluje,
rysuje, klonuje i przenosi obrazy. Fetch przesyła ciało ze strumienia przez
HTTP/2. Strony próbne to `about:messages` i `about:files`.
[Porty wiadomości](MESSAGE_PORTS.md) · [Structured clone](STRUCTURED_CLONE.md)

## Podetap alfa.12

BigInt i 64-bitowe tablice typowane uzupełniają operacje na liczbach
i danych binarnych. Gettery oraz settery w literałach obiektowych
działają również z obliczanymi kluczami. Structured clone przenosi bufory
i kopiuje Blob, File, DOMException oraz ImageData. Oryginalny zestaw
WPT z BigInt i akcesorami jest wykonywany w dokumencie. Ręczna próba
to `about:numbers`.
[Structured clone](STRUCTURED_CLONE.md) · [Weryfikacja wydań](VERIFICATION.md)

## Podetap alfa.11

Canvas 2D rysuje i udostępnia piksele w natywnym rendererze. Web Streams
przekazuje strumienie bajtowe do czytników BYOB i obsługuje potoki z
przekształcaniem i zapisem. DOM korzysta z tego samego EventTarget co zwykłe
cele oraz AbortSignal. StructuredClone umożliwia klonowanie grafów
i przenoszenie buforów. Ręczne próby to `about:canvas` oraz
**Przekształć tekst** na `about:bytes`.
[Structured clone](STRUCTURED_CLONE.md) · [Weryfikacja wydań](VERIFICATION.md)

## Podetap alfa.10

Web Storage trafia do profilu i do sesji karty. Date, prywatne elementy
klas, interwały i kolejka klatek uzupełniają JavaScript. Grid przyjmuje
nazwane linie i obszary oraz powtórzenia zależne od dostępnej szerokości
lub wysokości. Ręczna próba to `about:time`.
[Data i czas](DATE.md) · [Transport i zadania](NAVIGATION.md)

## Podetap alfa.9

Fetch dostarcza nagłówki przed ukończeniem odbioru. Dalsze porcje przechodzą
przez ograniczoną kolejkę i pozostają pod kontrolą czytnika. VM obsługuje dane
binarne oraz generatory. Blob, File i FormData działają w Fetch i w nawigacji
formularza. Ręczna próba `about:bytes` łączy te ścieżki.
[Weryfikacja wydań](VERIFICATION.md) · [Transport](NAVIGATION.md)

## Podetap alfa.8

Kolekcje kluczowane i własny RegExp uzupełniają bibliotekę JavaScript.
MutationObserver jest połączony z parserem i DOM. History i Location
obsługują zmiany adresu w istniejącym dokumencie oraz przywracanie stanu.
`about:routes` łączy te mechanizmy w próbie przejścia między dwoma widokami.
Zakres i limity opisują [historia](HISTORY.md) i [obserwatory DOM](MUTATION_OBSERVERS.md).

## Podetap alfa.7

Asynchroniczny transport przerywa oczekiwanie na I/O. VM wznawia funkcje
async i moduły przez kolejkę mikrozadań. Host wykonuje skrypty dołączane po
wczytaniu, utrzymuje graf dla `import()` i udostępnia AbortSignal oraz URL.
Ręczna próba `about:async` pozwala ukończyć albo przerwać zadanie. Testy z
lokalnym serwerem sprawdzają kolejność skryptów, zachowanie aktywnych połączeń
i kodowanie danych formularza. Wyniki wydania są w VERIFICATION.md.

## Podetap alfa.6

Przyrostowy parser i dekoder HTML są połączone z Fetch i VM w zwykłej nawigacji.
Podetap dodaje wykonywanie skryptów podczas parsowania, document.write, statyczny
graf modułów, klasy, wybrane żywe kolekcje DOM oraz fonty sieciowe.
Ręczna próba to `about:modules`. Launcher z pulpitu uruchomił alfę.6 i pokazał
„Moduły: 1” po kliknięciu. Stan instalacji i dokładne wyniki są w VERIFICATION.md.

## Podetap alfa.5

Kod alfa.5 łączy własne SVG/XML i rasteryzację, pozycjonowanie/warstwy/overflow
oraz macierze 2D z rzeczywistymi odczytami CSSOM. Własna VM dodaje szablony,
destrukturyzację, default/rest/spread obiektów, optional chaining i przypisania
logiczne. Wyniki aktualnego podetapu i jego instalacji są zapisywane w
VERIFICATION.md. Cały cel zgodności WWW pozostaje otwarty.

about:graphics udostępnia ręczne próby przesuwania/obracania, pomiaru,
kliknięcia SVG i przewijania panelu. Wspólny lokalny scenariusz porównuje Rust
oraz osobny Chrome przez REA. Nowe mechanizmy mają ograniczenia opisane w
SVG.md, CSSOM_VIEW.md, CSS_LAYOUT.md i JAVASCRIPT.md.

## 1. Fundament — rozwój trwa

Własne HTML/CSS/DOM, układ blokowy i liniowy, tekst, linki, obrazy, HTTP/HTTPS, historia, karty, profil, tryb bez okna, launcher i wersjonowana instalacja. Wydanie wymaga przejścia testów silnika, prób sieci, kontroli obrazu oraz rzeczywistego uruchomienia zainstalowanej kopii.

Poprzednia alfa 0.1.0-alpha.4 została udostępniona pod ikoną Rust Browser na pulpicie. Przeszła 335 testów Rust, ścisłą kontrolę Clippy, próby HTTPS i 45 kontroli scenariusza formularza/sesji z odniesieniem Chrome przez REA. Zainstalowany launcher zwrócił alfa.4 i uruchomił about:forms z tytułem Formularz: OK po syntetycznym kliknięciu. Cel skrótu, manifest i hash skopiowanego pliku potwierdzono niezależnie od instalatora. Profil i alfa.3 są zachowane; szczegóły w VERIFICATION.md.

Rozbudowa alfa.2 objęła odzyskiwanie błędów HTML, adopcję formatowania i tabele, indeksowane selektory, ograniczoną pamięć kształtowania tekstu i podzbiór Flexbox. Kod alfa.3 dodaje parsowanie fragmentów w kontekście elementu, zagnieżdżone template z nieaktywną zawartością oraz podzbiór Grid z kontrolą rozmiaru i pracy algorytmu.

Strona about:platform łączy Promise/JSON, klonowanie szablonu, mutacje DOM i Grid. Jej wcześniejsza próba przez zainstalowany launcher, spoza katalogu kompilacji i z oddzielnym profilem, potwierdziła przedstawienie klatki oraz tytuł Platforma: 3 po kliknięciu testowym. Alfa.4 dodaje about:forms do ręcznych prób edycji, wyboru opcji, resetu i lokalnej obsługi formularzy.

Kod alfa.4 parsuje obce treści SVG/MathML w dokumentach i fragmentach HTML: przestrzenie nazw elementów i atrybutów, nazwy SVG o mieszanej wielkości liter, CDATA, punkty integracji oraz powrót do reguł HTML. Klonowanie i import fragmentów zachowują metadane przestrzeni nazw i template.content. Wszystkie wcześniej wyłączone przypadki SVG/MathML w przypiętym html5lib są teraz wykonywane; mianowniki, znane błędy i nieobsługiwane przypadki pozostają jawne w crates/engine/data/html5lib/README.md. Parsowanie tych drzew nie zapewnia jeszcze grafiki SVG ani składu MathML.

Pozostałe prace fundamentu obejmują pełniejsze HTML/CSS i typografię, zaznaczanie tekstu strony, przyrostowe przeliczanie, izolację procesu treści z IPC oraz tor GPU. Grid nadal wymaga m.in. subgrid i dalszych reguł rozmiarów. Pełny Flexbox, pozycjonowanie, fonty sieciowe i animacje także pozostają w rozwoju.

## 2. Własny JavaScript i strony interaktywne — rozpoczęto

Zakres etapu obejmuje kompletny język, bibliotekę standardową i platformę stron interaktywnych: tożsamość i żywotność obiektów DOM, harmonogram, mikrozadania, zdarzenia, Promise, moduły, formularze, Fetch, cookies i historię SPA. Zgodność językowa jest mierzona przypiętym Test262 z poprawnymi wariantami i środowiskiem testów; raport zachowuje nieobsługiwane przypadki oraz błędy.

Pierwszy podetap w kodzie alfa.2 obejmuje własny lexer/parser/bajtkod/VM, wartości UTF-16, domknięcia, model obiektów i tracing GC. Host łączy to z podzbiorem DOM, kliknięciami, timerami i mikrozadaniami. Źródła klasyczne wykonują się po zakończeniu parsowania. Powstał osobny proces testowy i runner przypiętego Test262, który zachowuje nieobsługiwane przypadki i błędy w raporcie. ECMAScript i platforma WWW nadal nie są kompletne.

Kod podetapu alfa.3 obejmuje:

- Natywne rekordy Promise, rozwiązywanie thenable, łańcuchy then/catch/finally i jedną śledzoną przez GC kolejkę FIFO reakcji oraz mikrozadań. JSON.parse i JSON.stringify mają własną gramatykę i serializację UTF-16, reviver/replacer oraz kontrolę pracy i alokacji.
- Buforowane Fetch/Headers/Request/Response z text/json, klonowaniem i kontrolą bodyUsed. Transport HTTP(S) działa w ograniczonych pulach wątków, oddzielonych od pętli okna i nawigacji. Obejmuje CORS z preflight, kontrolę nagłówków, przekierowania i nieprzezroczyste odpowiedzi no-cors.
- innerHTML/outerHTML/insertAdjacentHTML, DocumentFragment, template.content, cloneNode, replaceChildren i operacje wstawiania/usuwania. Fragmenty zachowują kontekst, flagę skryptów i tryb dokumentu. Zmiany przechodzą wspólną kontrolę hierarchii, cykli, głębokości, liczby węzłów i danych areny przed zatwierdzeniem. Wstawione w ten sposób skrypty pozostają nieaktywne.
- Próbę about:platform oraz niezależne testy łączące odpowiedź Fetch, Promise/JSON, szablony i mutacje DOM. Podzbiory oraz ich ograniczenia są opisane w JAVASCRIPT.md, CSS_LAYOUT.md i danych testowych HTML.

Kod podetapu alfa.4 obejmuje:

- Odrębny typ Symbol, symboliczne klucze i rejestr Symbol.for/keyFor, synchroniczny protokół iteratorów, iteratory Array/String, Array.from, for-of z deklarowanym identyfikatorem oraz spread tablic i argumentów wywołania/new. Wspólny mechanizm uwzględnia zamykanie iteratorów i budżety pracy; same tożsamości well-known Symbol nie wdrażają wszystkich ich konsumentów.
- Promise.all/race/allSettled/any oparte na rzeczywistym protokole iteratorów, z porządkiem wyników, thenable i AggregateError. Biblioteka Promise/JSON, model obiektów i GC pozostają własną implementacją, opisaną wraz z lukami w JAVASCRIPT.md.
- Host formularzy: dirty/default values, checkboxy i grupy radio, select/options, własność formularza także poza jego poddrzewem, disabled, reset, anulowalne zdarzenia oraz podzbiór walidacji. Natywne okno ma edycję tekstu/hasła, zaznaczanie klawiaturą i schowek, nawigację Tab/Shift+Tab, listę rozwijaną/listbox i uruchamianie submittera. Nawigacja GET oraz URL-encoded POST obsługuje zmianę lub zachowanie metody/ciała według typu przekierowania.
- Własny CookieStore dla nawigacji i Fetch credentials oraz document.cookie. Obejmuje HttpOnly, Secure, SameSite, domeny/ścieżki, prefiksy, reguły ICANN/private z przypiętej PSL i podstawowe partycjonowanie Partitioned według strony głównej. Trwałe cookies na Windows przechodzą przez DPAPI i atomowy zapis pod blokadą profilu; cookies sesyjne nie są odtwarzane po zakończeniu procesu.

Zakres formularzy jest sprawdzany przez 17 testów semantyki oraz 8 testów renderera. Obejmują m.in. anulowanie, reentrancy, własność, radio/select, edycję UTF-16, bieżące migawki wartości, geometrię, maskowanie haseł także przy uproszczonym układzie oraz przycinanie obrazu. tools/verify-forms.mjs łączy handlery wejścia aplikacji z rzeczywistym lokalnym HTTP, POST/303, Origin/Cookie, Promise.all/JSON/Fetch, prywatnością HttpOnly i ponownym otwarciem tego samego profilu. Opcjonalny wzorzec REA uruchamia osobny, należący do próby proces Chrome; raport zachowuje ograniczenia przechwytywania i nie oznacza pełnej równoważności przeglądarek.

Do wykonania pozostają m.in. Intl, Proxy, SharedArrayBuffer, atrybuty importu, zbiory Unicode RegExp i pozostałe części biblioteki standardowej. Fetch wymaga dalszych opcji żądań. Porty wymagają przesyłania między kontekstami, a formularze pełniejszej walidacji, pickerów daty i koloru, dostępności i zaznaczania przeciąganiem. Historia nawigacji wymaga BFCache, pełnej serializacji stanu i ponownego przesłania POST. Pozostają pełne konteksty i polityki prywatności cookies, IndexedDB i magazyny uwierzytelniania, CSP oraz ograniczenia dostępu do sieci lokalnej.

Pozostają destrukcyjne `document.write` i `document.open`, aktualizacje zasobów zależne od mutacji, zdarzenia specjalistyczne, retargeting w Shadow DOM i kolekcje z właściwościami nazwanymi. Plan platformy JavaScript i WWW wymaga szerszych prób na rzeczywistych aplikacjach.

## 3. Platforma WWW — do wykonania

IndexedDB, workery i service workery, aplikacje offline, WebAssembly, pełna grafika SVG i skład MathML, pełniejsze Canvas 2D, OffscreenCanvas, WebGL, WebGPU, audio/wideo, WebRTC, uprawnienia i integracje systemowe. Macierz funkcji wynika ze standardów i porównań z przypiętą wersją Chrome. DRM/CDM i dostępność chronionych usług wymagają osobnych integracji oraz weryfikacji.

## 4. Wydajność i wydanie użytkowe — do wykonania

Profilowanie i kontrola alokacji, przyrostowy layout i rysowanie, kafelkowanie i kompozycja GPU, budżety cache oraz własny JIT. WPT, Test262 i rzeczywiste scenariusze witryn, wraz z testami bezpieczeństwa i długich sesji. Pomiary obejmują cały zestaw procesów, RAM, GPU, CPU, start i opóźnienia interakcji.

## Zasada kończenia etapów

Każdy ukończony etap dostaje działającą, sprawdzoną instalację pod tą samą ikoną pulpitu. Profil i poprzedni build pozostają dostępne. Wersja wskazuje rzeczywistą obsługę funkcji i jej braki.

Cały projekt pozostaje nieukończony do czasu wdrożenia i weryfikacji pełnego zakresu. Pierwsza alfa, zielone testy podzbioru ani wyniki kilku witryn nie oznaczają realizacji całego celu.
