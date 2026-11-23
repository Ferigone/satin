# Date w silniku JavaScript

Obiekt `Date` przechowuje własną wartość czasu UTC w milisekundach. Arytmetyka
używa proleptycznego kalendarza gregoriańskiego, a `TimeClip` ogranicza wartość do
±100 milionów dni od epoki Unix. Konstruktor, metody UTC, odczyt i modyfikacja
składowych oraz serializacja ISO działają bez wywoływania zewnętrznego silnika
JavaScript. [ECMAScript 21.4](https://tc39.es/ecma262/multipage/numbers-and-dates.html#sec-date-objects)

`Date.parse` czyta format ECMAScript, w tym rozszerzone lata, przesunięcia UTC
i daty bez składowej czasu. Obsługuje też postacie zwracane przez `toString`
i `toUTCString` oraz wybrane daty z angielskimi nazwami miesięcy i zapisem
numerycznym. Odczyt jest ograniczony do 512 jednostek UTF-16.

## Czas lokalny

Windows dostarcza strefę systemową i reguły czasu letniego dla poszczególnych
lat. Gdy zegar powtarza godzinę, konstruktor i metody lokalne wybierają
wcześniejszą chwilę. Gdy zegar pomija godzinę, używają przesunięcia sprzed zmiany.
Test Windows sprawdza obie sytuacje na regułach strefy Eastern Standard Time
bez zmiany strefy systemowej. [Konwersja czasu w Win32](https://learn.microsoft.com/windows/win32/api/timezoneapi/nf-timezoneapi-systemtimetotzspecificlocaltimeex)

Przesunięcia historyczne mają dokładność i pokrycie danych Windows. Poza
zakresem lat 1602–30826 konwersja używa reguł równoważnego roku o takim samym
układzie dni tygodnia i przestępności. Inne platformy korzystają obecnie z UTC.
Metody `toLocale*` zwracają podstawowy format tekstowy silnika. Obsługa
`Intl.DateTimeFormat` i integracja z `Temporal` pozostają otwarte.

## Weryfikacja

```powershell
node tools/test262/runner.mjs --test262 PATH_TO_TEST262 --worker target/release/rb-js.exe --path built-ins/Date --output target/date-verification/test262.json
node tools/verify-date.mjs target/release/rust-browser.exe target/date-verification/reference --chrome-reference
```

Test262 czyta niezmienione pliki z rewizji
`48221d7c1f7745dc13e420a2e850a248c0b2afb4`. Porównanie przez REA uruchamia
osobny Chrome i sprawdza wynik tej samej strony, w tym zimowe i letnie
przesunięcia strefy systemowej oraz konstruktory w pobliżu zmian czasu. Skrypt
ustawia w sesji Chrome strefę systemową odczytaną przez Node. Zmienna
`DATE_REFERENCE_TIMEZONE` pozwala wskazać jej identyfikator IANA.
