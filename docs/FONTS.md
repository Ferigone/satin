# Fonty w alfie 6

CSS zachowuje listę rodzin w kolejności autora. Rozróżnia nazwy z cudzysłowami
od rodzin ogólnych, takich jak `serif`, `monospace` i `system-ui`.
Grubość i styl są niezależnymi dziedziczonymi właściwościami. Skrót `font`
ustawia też rozmiar i wysokość linii. Dopasowanie bierze pod uwagę styl,
grubość oraz zakres znaków. Brak glifu kieruje wybór do kolejnej rodziny,
a następnie do ograniczonej listy rodzin systemowych odpowiednich dla
danego zakresu Unicode. Wybór awaryjny wykorzystuje klastry grafemów, aby
utrzymać zwykłe znaki bazowe i akcenty w jednym foncie.

`@font-face` odczytuje listę źródeł `local()` i `url()` z podpowiedziami
`format()`, `unicode-range`, styl i zakres grubości. Dekoder otwiera
pliki sfnt z konturami TrueType oraz ich kontenery WOFF i WOFF2. Instancje
fontu zmiennego używają tych samych parametrów osi `wght`, `ital` i `slnt`
w kształtowaniu oraz rysowaniu. Dla pochylonej odmiany osi `slnt` używany
jest kąt 14 stopni. Zainstalowane fonty są dopasowywane po rodzinie lub
konkretnej nazwie twarzy w `local()`.

Każdy ukształtowany fragment tekstu zachowuje użyte twarze. Glif wskazuje
pozycję twarzy w tym fragmencie. Dzięki temu osobne instancje układu i
renderera korzystają z tych samych bajtów i parametrów, również przy
rysowaniu formularzy. Fonty systemowe z jednego pliku współdzielą bajty.
Pamięci podręczne tekstu i glifów mają słabe referencje do twarzy, więc
zamknięta strona może zwolnić dane fontów.

Pobierany plik ma limit 4 MiB przed dekodowaniem i 8 MiB w formacie sfnt.
Zestaw strony mieści 32 twarze i 32 MiB danych sfnt. Walidacja przed
przekazaniem danych do dekodera sprawdza rozmiary tabel, dane konturów
WOFF2 i sumy strumieni. Walidacja sfnt ogranicza liczbę punktów, głębokość
grafu składowych, jego rozwinięcie, cykle i pracę osi zmiennych.
Katalog fontów systemowych ogranicza żywe dane do 64 MiB i 128 twarzy.
Pamięć ukształtowanego tekstu ma budżet 4 MiB, plany kształtowania do 2 MiB
szacowanego kosztu, a mapy bitowe glifów 16 MiB.
Rasteryzacja pobiera budżet przed rozwinięciem konturu i alokacją bitmapy.

Pokrycie obejmuje kształtowanie i rysowanie własnych plików TTF, WOFF
i WOFF2, dobór rodzin i zakresów znaków, osie zmienne, użycie tych samych
twarzy przez niezależny renderer oraz zwalnianie pamięci po ostatnim
użyciu. Próbki powstają z prostokątnych konturów z repozytorium projektu.
Własna próba Chrome 157 z tymi samymi plikami dała szerokość 32 CSS px
dla WOFF2, 42 CSS px przy użyciu drugiej rodziny i 16, 24 oraz 32 CSS px
dla grubości 400, 550 i 700 fontu zmiennego.

Zakres alfy obejmuje jednobarwne kontury i poziomy układ tekstu. Pobierane
kolekcje oraz CFF i CFF2 wymagają dalszej pracy. Pozostają pełny algorytm
bidi i łamania wiersza, kolorowe glify, synteza krojów, wszystkie osie
OpenType, dokładne kąty `oblique`, CSS Font Loading API i czasy
`font-display`. Wyniki zależą od fontów dostępnych w Windows w tych
przypadkach, w których strona korzysta z rodzin systemowych.

Algorytmy oparto na specyfikacjach CSS Fonts oraz WOFF. [CSS Fonts Level 4](https://www.w3.org/TR/css-fonts-4/), [WOFF 1](https://www.w3.org/TR/WOFF/), [WOFF 2](https://www.w3.org/TR/WOFF2/).
