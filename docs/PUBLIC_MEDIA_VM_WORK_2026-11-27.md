# Budżet JavaScript przy starcie publicznych odtwarzaczy

Pomiar z 27 listopada 2026 r. na Windows x64. Kod silnika ma rewizję `95b342f`, końcowe testy `02f35d0`. Oba publiczne odtwarzacze nadal zatrzymują się na starcie JavaScript przy dotychczasowym limicie miliona jednostek na skrypt.

| Wejście | Koszt w bazie | Stan końcowy |
|---|---|---|
| Twitch, `core` o SHA-256 `06e2a96c16f867bcca3666e8cf123668db7171875ad5e46943e22c80d27288ec` | Przepisanie wzorców przez `split` i `join` zużyło 199 613 jednostek w 43 wywołaniach. Stos kończył się w parserze UA. | Ta sama funkcja zużyła 100 048 jednostek w 43 wywołaniach. Następna odmowa nastąpiła w funkcji rejestrującej eksporty webpack przez `Object.defineProperty`. |
| YouTube, inline o SHA-256 `3e033849bb8218c4d82825569af753876005483fb73b8578654dd9fca2ed0e94` | Inline wywołał `createPlayer` z `base.js`. Sprawdzanie flag zużyło 338 760 jednostek w 2112 wywołaniach. | Sprawdzanie flag zużyło 96 897 jednostek w 2114 wywołaniach. Przeszło zerowanie tablic kompresora i obliczenie CRC32. Na stosie odmowy była pętla DEFLATE przetwarzająca okno dopasowań. |
| YouTube, zewnętrzny skrypt około 10,94 MB | Komparator nazw encji HTML został wywołany 12 648 razy. | W końcowej sesji komparator został wywołany 11 339 razy. Budżet skończył się podczas przebudowy tabeli po sortowaniu. Warianty tej paczki miały różne sumy SHA-256. |

Rejestracja kosztu funkcji jest dostępna w wersji diagnostycznej. Zapisuje zakres źródłowy, krótki odcisk, liczbę wywołań, własną pracę oraz stos w chwili odmowy. Pełne SHA skryptów oraz rozkład pracy i czasu CPU znajdują się w katalogu próby `target/vm-work-df0c50e914`. Na identycznych wejściach w pięciu przeplatanych rundach mediana pracy sortowania nazw spadła z 280 088 do 122 193. Wypełnienie i sprawdzenie 65 536 liczb 16-bitowych zakończyło się poprawnie za 4187 jednostek. Baza wyczerpywała milion. Mały test obserwowalnych efektów sortowania i łączenia miał medianę 1,106 ms wobec 1,101 ms w bazie.

## Odtwarzanie

Trzy próbki zwykłego okna Satin z jednej nawigacji dały na Twitchu zero elementów wideo i zero wierszy czatu z tekstem i rozmiarem w układzie. Na YouTube podsystem mediów miał wpis z czasem 0 s, stanem gotowości 0 i zerem klatek we wszystkich trzech próbkach. Pomiar WASAPI dla PID procesu nie odnalazł sesji audio w żadnej publicznej próbie Satin.

W anonimowej kontroli Chromium klatki Twitcha wzrosły z 24 do 273, a liczba wierszy czatu z jednego do pięciu. Na YouTube liczba klatek wzrosła z 40 do 184. Osobny pomiar WASAPI wykrył sygnał podczas odtwarzania i wznowienia oraz ciszę podczas pauzy w obu serwisach. Lokalny Satin przeszedł próby progresywnego MP4, MSE, pauzy, przewinięcia i wznowienia.

## Uruchomienie

Katalog próby zawiera końcową binarkę zwykłą i diagnostyczną oraz dwa programy A/B. Binarka zwykła ma SHA-256 `6654533c957618c93f5c68791ac329ee47cd62eaa913ec489693951e1698cceb`. Windows Media Foundation, skala buforów i limity są takie jak na początku próby.

```powershell
$trial = 'target/vm-work-df0c50e914'
$env:SATIN_QA_COUNT_CHAT = '1'
node tools/verify-public-media.mjs window "$trial/seventh-native.exe" 95b342f
node tools/verify-public-media.mjs satin "$trial/seventh.exe" 95b342f
node tools/verify-native-media.mjs "$trial/seventh-native.exe"
```

Sesje referencyjne znajdują się w `target/public-media/window-DTmqhh`, `satin-rZum2c` i `chrome-t0a4gu`. Wyniki A/B są w `ab-base-seventh.json`. Testy zaliczyły 244 przypadki Rust VM, 12 przypadków diagnostyki okna i 6 przypadków importera Node. Każde z 10 wejść A/B zwróciło również oczekiwany wynik w odrębnej kontroli poprawności w Node/V8.
