# CSS, układ i tekst w alfie 6

Selektory oraz deklaracje są kompilowane raz na budowę układu. Indeks prawej
części selektora wybiera kandydatów po id, klasie i tagu. Kaskada rozwija skróty
na niezależne właściwości i uwzględnia specificity, important, dziedziczenie,
wartości CSS-wide oraz ograniczony podzbiór zmiennych.

Układ obsługuje model pudełkowy, min/max i border-box, część procentowych
rozmiarów, sąsiednie pionowe marginesy, inline-block oraz podzbiór Flexbox:
kierunki, grow/shrink, wrap, gap, order, auto-margins i wyrównanie. ElementBox,
element_at i rect_for łączą geometrię z tożsamością DOM. Głębokie albo nadmiernie
kosztowne układy przechodzą na iteracyjny, uproszczony przepływ treści;
Layout.limited ujawnia ten stan. Próby 256 poziomów bloków, inline i flex
przeszły na wątkach ze stosem dokładnie 1 MiB.

Pamięć kształtowania tekstu jest ograniczona liczbą wpisów i rozmiarem danych.
Awaryjne dzielenie długiego słowa korzysta z klastrów ukształtowanego tekstu,
zamiast wielokrotnie kształtować rosnące prefiksy.
Alfa 6 używa obliczonych rodzin i krojów CSS, wspólnych danych fontów sieciowych
i awaryjnego dopasowania według zakresu znaku. [Obsługa fontów](FONTS.md).

## Grid i granice tokenów CSS — alfa.3

Własny algorytm Grid obsługuje display:grid/inline-grid, jawne oraz cykliczne
niejawne tory, rozmiary stałe i procentowe, auto/min-content/max-content/fr,
minmax() i repeat() z liczbą całkowitą. Pozycjonowanie obejmuje dodatnie i ujemne
numery linii, span, grid-area bez nazw, row/column flow, dense/sparse i order.
Wspierane są gap, min/max-height kontenera, rozmiary procentowe elementów,
wyrównanie, rozciąganie i auto-marginesy. Niejawne tory przed początkiem
jawnej siatki otrzymują prawidłową fazę cyklicznego wzorca auto-tracks.

Rozmieszczanie najpierw zajmuje jawne obszary, następnie ustalone wiersze,
a pozostałe elementy obsługuje kursor auto-placement. Rozmiary wewnętrzne są
rozwiązywane według rosnących spanów; planowane przyrosty torów zapobiegają
uzależnieniu wyniku od kolejności elementów z takim samym spanem. Tory fr
zamrażają minima przed ponownym podziałem miejsca. Procentowe wiersze i odstępy
cykliczne są najpierw auto/zero podczas wyznaczania rozmiaru kontenera, a potem
rozwiązywane względem otrzymanego rozmiaru bez ponownego powiększania kontenera.
Komentarze i ukryte węzły nie rozdzielają ciągłego tekstu na osobne anonimowe
elementy siatki. Scalony tekst i wynik sprawdzenia whitespace są pamiętane tylko
na czas budowy układu.

Limity to 128 torów na oś i 4096 elementów siatki. Każda próba zajęcia komórki
zużywa wspólny budżet pracy układu; przekroczenie powoduje widoczny,
uproszczony przepływ z Layout.limited=true. Nie tworzy się macierzy o wymiarach
wynikających z dowolnie dużych numerów linii. Przypadek linii 32000 oraz 256
poziomów zagnieżdżonego Grid przeszedł kontrolę zachowania treści/linków;
próba głębokości korzystała ze stosu dokładnie 1 MiB. Zwykłe listy torów są
współdzielone między wyliczonymi stylami; jednostki rem/vw są obliczane przed
układem.

Komentarz nie jest już zamieniany na token whitespace. Prywatna granica tokenu
rozróżnia p/**/.x od p /**/ .x i nie pozwala scalić di/**/v ani 10/**/px.
Publiczne reguły i deklaracje zapisują takie granice jako komentarze, bez
wycieku znaków sterujących. Podstawienia var() także zachowują granice tokenów:
var(--n)px nie staje się wymiarem, jeśli --n zawiera tylko liczbę. Surowe NUL
wejścia jest normalizowane do U+FFFD i nie może udawać prywatnej granicy.

Algorytmy opracowano na podstawie
[CSS Grid Level 1](https://www.w3.org/TR/css-grid-1/#layout-algorithm) oraz
[CSS Syntax Level 3](https://www.w3.org/TR/css-syntax-3/#consume-comments).
To nadal podzbiór standardów, a nie pełny tokenizer ani pełne Grid.

## Pomiary referencyjne

Alfa.3: Chrome 157.0.8103.51, osobny profil, tryb headless, skala 1, viewport
984 × 905 CSS px. 38 własnych fixture'ów Grid dało 145 pudełek i 580
porównywanych współrzędnych. Maksymalna różnica wyniosła 0.010422 CSS px.
Test pinned_chrome_157_grid_reference zachowuje cały dokument wejściowy oraz
otrzymane wartości; tolerancja wynosi 0.05 CSS px. Zawiera także wewnętrzne
rozmiary zagnieżdżonych pudełek inline, mieszane funkcje minmax, wielotorowe
spany, implicit tracks przed początkiem siatki, cykliczne procenty i limity
min/max-height. W fixture'ach nie ma zależności od renderowania glifów: font-size
i line-height są zerowe, a rozmiary wewnętrzne wynikają z jawnych pudełek.
Dodatkowa własna próba Chrome potwierdziła grupowanie tekstu oddzielonego
komentarzem/display:none oraz brak dziedziczenia paddingu do anonimowego itemu.

Wspólny ukierunkowany zestaw CSS/layout ma 35 zaliczonych testów i jeden
świadomie pominięty benchmark opt-in. Próby Flexbox poprzedniego wydania
pozostają niezmienione i nadal przechodzą.

Chrome 157.0.8103.51, viewport 784 × 505 CSS px, skala 1, oddzielny profil i
własny lokalny dokument. Jedenaście prostokątów dało 44 porównywane współrzędne;
maksymalna różnica wyniosła 0.005219 CSS px. Test
pinned_chrome_157_box_and_flex_reference zachowuje oczekiwane wartości.
To sprawdzenie określonych pudełek i Flexbox, bez oceny renderowania fontów
ani pełnego pokrycia CSS. Źródło próby i wynik Chrome są w docs/evidence.

Historyczny benchmark alfy.2 release: 4000 reguł klasowych i 4000 akapitów, siedem próbek,
rozgrzana pamięć fontów. Ostatni przebieg: parse+compile 14.761 ms, mediana
wyliczania stylu 2.692 ms, mediana Layout::build 39.000 ms. Odwiedzono 4000
kandydatów; pełne sprawdzenie wszystkich par oznaczałoby 16000000 wizyt.
To porównanie liczby operacji, bez zmierzonego czasu starego algorytmu.
Dzielenie 20000 / 200000 znaków ASCII trwało 3.099 / 34.994 ms. Poprzedni
przebieg miał odpowiednio 20.447 / 5.083 / 34.066 ms i 2.946 / 29.553 ms,
więc pojedynczych czasów nie należy traktować jako stałej wydajności.

Powtórzenie: cargo test --release -p rust-browser-engine --test
layout_performance -- --ignored --nocapture. To syntetyczny pomiar podsystemu;
nie mierzy całkowitego RAM/CPU aplikacji i nie dowodzi przewagi nad Chrome.

## Pozycjonowanie, przepełnienie i transformacje — alfa.5

Układ oddziela przepływ dokumentu od geometrii wizualnej. `position:relative`
przesuwa pudełko i jego zawartość bez zmiany miejsca zajętego w przepływie.
`absolute` i `fixed` nie uczestniczą w rozmiarach bloków, Flexbox ani Grid.
Ich pozycje powstają po wyznaczeniu wymiarów bloków odniesienia: padding box
najbliższego pozycjonowanego przodka dla absolute, viewport dla fixed,
albo przodek z transformacją, który ustanawia właściwy blok odniesienia.
Obsługiwane są fizyczne top/right/bottom/left, inset, procenty, rozciągnięcie
między przeciwległymi krawędziami, min/max i automatyczne marginesy.

`overflow`, `overflow-x` i `overflow-y` obsługują visible/hidden/clip/auto/scroll
oraz regułę obliczania par osi. Zakresy przewijania mają ograniczone przesunięcia;
przewijanie zawartości nie przesuwa ramki ani jej tła. Klipy i transformacje
korzystają ze wspólnych, zrównoważonych poleceń PushState/PopState.
Trafienia kontrolek, elementów i linków używają macierzy odwrotnej i tych samych
klipów. Prostokąty DOM pozostają nieprzyciętymi obwiedniami wizualnymi;
osobna geometria offsetów pomija transformacje i przewijanie.

Lista transformacji 2D obejmuje matrix, translate/translateX/translateY,
scale/scaleX/scaleY, rotate, skew/skewX/skewY i transform-origin.
Transformacje składają się w kolejności CSS; osobliwe macierze nie tworzą
aktywnego obszaru trafień. Konteksty stosu dla transformacji, fixed i z-index
pozycjonowanych elementów oraz elementów Flex/Grid są rysowane atomowo.
Kolejność trafień odpowiada kolejności warstw, a przykryty link nie jest aktywny.

Zachowano obliczone style we współdzielonym Arc, geometrię viewportu oraz
przesunięcia zagnieżdżonych obszarów. Flaga depends_on_viewport_scroll pozwala
przewijać zwykłe strony bez ponownego układania i przebudowywać strony fixed,
gdy zmieni się viewport. Obliczone właściwości SVG przekazują rzeczywistych
zwycięzców kaskady do osobnego modułu SVG; podstawowe pudełko SVG korzysta
z tej samej ścieżki rozmiarowania treści zastępowanej co obrazy.

Własna próba headless Chrome 157.0.8103.51, skala 1, viewport 984 × 905 CSS px,
obejmuje 20 przypadków i 45 pudełek (180 współrzędnych): bloki odniesienia,
insety, shrink-to-fit, auto-marginesy, min/max, transformacje i fixed wewnątrz
transformowanego przodka. Maksymalna różnica wyniosła 0.00001145 CSS px;
tolerancja testu to 0.05 CSS px. Niezmieniony dokument wejściowy i wynik są
w tests/fixtures/css_position_reference.json. Dodatkowe próby sprawdzają
klipy w pikselach, przewijanie, kolejność z-index, dokładne trafienia obróconych
pudełek oraz 256 poziomów elementów na stosie 1 MiB.

Adres linku jest współdzielony między fragmentami tekstu i rozwiązywany raz
na element. Metadane linków mają limit 64 KiB na adres oraz 8 MiB łącznie;
wyczerpanie oznacza Layout.limited zamiast nieograniczonego kopiowania adresów.
Nowy etap wizualny przechodzi po drzewie i kontekstach iteracyjnie; emisja
klipów i poleceń ma osobny budżet miliona jednostek pracy. Stylowane funkcje
transformacji są ograniczone do 32 operacji.

Algorytmy oparto na [CSS Positioned Layout Level 3](https://www.w3.org/TR/css-position-3/),
[CSS Transforms Level 1](https://www.w3.org/TR/css-transforms-1/) i
[CSS Overflow Level 3](https://www.w3.org/TR/css-overflow-3/).
To implementacja ograniczonego podzbioru poziomego układu LTR: sticky,
logiczne insety, RTL/writing modes, pełne statyczne pozycje absolute w Flex/Grid,
fragmentacja pozycjonowanych elementów, 3D/perspective, maski, clip-path CSS,
zaokrąglone klipy i natywne paski przewijania wymagają dalszej pracy.

## Pozostałe ograniczenia

Grid nie obsługuje jeszcze nazw linii/obszarów, template-areas, auto-fit/auto-fill,
fit-content(), subgrid/masonry, baseline alignment, pionowych writing modes,
fragmentacji ani pełnych zależności rozmiaru elementów zastępowanych od
proporcji i obu osi. Nie zaimplementowano całego CSS Syntax/Selectors ani całej
semantyki CSSOM. Pozostają pełny Flexbox i pełne warianty pozycjonowania,
transformacji oraz clippingu, relacje marginesów rodzic–dziecko, kompletny bidi/Unicode,
pełne dopasowanie fontów i cały CSS Font Loading API. Wyniki referencyjne dotyczą wybranych
geometrii; nie oznaczają zgodności całego CSS, przewagi nad Chrome ani pomiaru
całkowitego RAM/CPU przeglądarki.
