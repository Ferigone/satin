# Pomiary, przewijanie i wspólny układ — alfa.5

Host JavaScript korzysta z tego samego Layout, stylów i migawki DOM co renderer.
Zmiana DOM lub stanu kontrolki zwiększa rewizję. Pierwszy odczyt wymuszający układ
buduje go synchronicznie; dalsze odczyty tej samej rewizji zwracają zachowany Arc.
Przekazanie migawki do okna nie kopiuje ponownie całego dokumentu. Fonty mają
wspólne niezmienne dane krojów, a ograniczone cache kształtowania i glifów należą
do instancji. Host tworzy Fonts dopiero po rzeczywistym zapotrzebowaniu na układ.

## Dostępne mechanizmy

- getBoundingClientRect i getClientRects zwracają migawki w pikselach CSS
  względem viewportu, z pozycjonowaniem, przekształceniami i przewijaniem.
  Przycięcie nie zmniejsza tych prostokątów. Element bez pudełka ma pustą listę
  i zerowy prostokąt. Wyniki zawierają toJSON; lista ma item i iterator.
- offsetWidth/Height/Left/Top/Parent używają geometrii bez transformacji i
  przewinięcia. clientWidth/Height/Left/Top uwzględniają obramowania, a
  scrollWidth/Height obszar treści także przy overflow:visible/clip.
- getComputedStyle zwraca żywy obiekt odczytu dla obsługiwanych właściwości:
  rozmiarów, fontu, koloru, marginesów, paddingu, obramowań, pozycjonowania,
  overflow, transformacji i zmiennych CSS przez getPropertyValue.
  Obsługuje nazwy camelCase i CSS; setProperty/removeProperty odrzucają zapis.
- scrollTop/Left, scroll/scrollTo/scrollBy oraz window.scrollX/Y/pageXOffset/
  pageYOffset obsługują natychmiastowe przewijanie. Skuteczne przesunięcie
  zostaje zapisane po ograniczeniu rozmiarem treści; ponowny wzrost nie
  przywraca odrzuconego przesunięcia. Zmniejszenie dokumentu ogranicza także
  viewport, łącznie z ponownym układem elementów fixed.
- Zdarzenia scroll są łączone według celu i odraczane do kolejnego zadania.
  Przewinięcie dokumentu bąbelkuje do window, a elementu pozostaje przy elemencie.
  Zmiana rozmiaru viewportu odracza resize. Działają onscroll/onresize i
  addEventListener. Native wheel przechodzi od trafionego elementu przez
  przodków i przekazuje niewykorzystane przewinięcie dalej.

Nie ma pełnych interfejsów/prototypów DOMRect/CSSStyleProperties, wszystkich
właściwości, skrótów ani kompletu zasad serializacji CSSOM. W szczególności
normal line-height jest przybliżony przez używaną wysokość linii, nie wszystkie
insety auto i procenty zależne od nieokreślonego rozmiaru mają kompletną semantykę,
a pseudoelementy, scrollIntoView, smooth scroll, RTL/pionowe kierunki i pełna
polityka przewijania viewportu pozostają w rozwoju. SVG geometry DOM ma
ograniczone mapy obrysów, opisane w SVG.md. Kolekcje innych API DOM nadal są
migawkami. Zgodność dotyczy opisanego podzbioru.

## Praca i rysowanie

Odczyt układu obciąża budżet bieżącego zadania VM przed kopiowaniem dokumentu,
parsowaniem CSS i obróbką zasobów. Obejmuje źródła zewnętrznych arkuszy; natywna
praca budowy scen SVG jest rozliczana przed udostępnieniem wyniku. Kolejne
odczyty z cache nie powtarzają tych operacji. Fatalny limit nie może być
przechwytywany w nieskończonej pętli przez try/catch.

Renderer utrzymuje stos macierzy i klipów; obrócone klipy testuje przez macierz
odwrotną. Prostokąty osiowe zachowują szybkie wypełnianie wierszy. Niewidoczny
tekst jest odrzucany przed rasteryzacją glifów; widoczne glify są sprawdzane
przed alokacją bitmapy. Praca dokumentu ma limit 64 Mi jednostek. Wywołanie
SVG ma dodatkowy własny limit 16 mln jednostek, więc końcowe wywołanie może
przekroczyć wspólny limit najwyżej o ten ograniczony koszt. Zewnętrzny test
klipów ma osobny licznik, co ogranicza także długie łańcuchy klipów. Przekroczenie
jest jawne w paint_limited; odrębny budżet interfejsu zachowuje pasek adresu i karty.

Testy CSSOM obejmują rzeczywiste pomiary, żywe style, powtórne wykorzystanie
układu, przewijanie, zdarzenia i budżety. Testy integracji SVG sprawdzają razem
obraz, obrys, trafienie i linki. tools/verify-graphics.mjs uruchamia własny
serwer, aplikację bez okna i niewidoczne natywne okno; opcjonalne porównanie
Chrome przez REA używa osobnego procesu i syntetycznego dokumentu.

Źródła: [CSSOM View](https://drafts.csswg.org/cssom-view/),
[getComputedStyle i wartości rozstrzygnięte CSSOM](https://drafts.csswg.org/cssom/).
