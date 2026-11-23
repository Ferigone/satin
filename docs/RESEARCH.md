# Rozpoznanie referencyjne — 10 listopada 2026

REA 6.8.0 uruchomiono przez oficjalny pakiet CLI rea-agents, bez rejestrowania nowego serwera MCP w Codex. Badany Chrome 157.0.8103.51 używał osobnego profilu testowego. Tymczasową przeglądarkę i lokalny serwer zamknięto po próbach.

## Zaobserwowano

- REA list_browser_targets: Chrome/157.0.8103.51, V8 15.7.112.23. Evidence: ev_37e57fea695c81a94469f5e63fa933bbff0fcc0bf35fcc6529af15d0df808d36.
- Sześć zadanych akcji lokalnego scenariusza ukończonych: gotowość strony, wpisanie tekstu, kliknięcie zmieniające układ i wywołujące Fetch, oczekiwanie na odpowiedź oraz przejście do nowego dokumentu. Evidence: ev_985bdd19b16a41d8dc7e0bcebffb3fcdda1f5082790345df265626964a24bdd4.
- W tym konkretnym dokumencie kolejność wynosiła inline → microtask → defer → module → DOMContentLoaded. Nie jest to reguła porządkująca wszystkie możliwe kombinacje skryptów.
- Zapisany DOM naprawił błędne zagnieżdżenie b/i. Szerokość po zmianie stylu i synchronicznym odczycie układu wynosiła 300 px. Fetch przekazał testowe ciasteczko; localStorage zachowało wartość; pushState i nawigacja zwiększały historię.
- Nawigacja do publicznego repozytorium GitHub i do jego katalogu docs oraz oczekiwanie na widoczny element main zostały ukończone. Evidence: ev_803c2b277fc6887b23c83918838351adbd3d802ed15021b6c4424964383d867e.

Migawki kroków miały pełne dane w zakresie wybranych artefaktów. Całe raporty scenariuszy oznaczały historię zdarzeń jako niepełną, więc nie stanowią dowodu kompletnego przebiegu przyczynowego. Czasy REA nie są benchmarkiem wydajności przeglądarki. Te obserwacje dotyczą Chrome; własny silnik musi osobno przejść te same próby.

## Podetap alfa.5

Wspólny syntetyczny scenariusz tools/verify-graphics.mjs uruchomiono w wydaniu
release Rust i osobnym Chrome 157.0.8103.51 przez REA. Pięć kroków, łącznie
ze startem, ukończono. Transformowany prostokąt miał 52×112 px, offsetWidth
112 px, clientWidth 108 px, obrys koła SVG 60×60 px, insety absolute 15/10 px,
a przewinięty panel 30/100/-30 px. Wyniki wysłane przez stronę HTTP były takie
same w Rust bez okna, natywnym oknie i Chrome. Kliknięcie trafiło w węzeł koła.
Evidence: ev_843c28a52f6eda91621c7152a780ee909d2aa87e7e06d10be75150e12f14032e.

Ponowiono także scenariusz formularza/sesji: 45 kontroli, identyczne kodowanie
POST oraz przejście POST/303/GET i trwałość cookies. REA:
ev_1d8325938a2fc99da672be90579c28cc22c44e01bd6b5de50cd856da8b0aef96.
Oba przechwycenia mają completeness=incomplete i equality_eligible=false
z powodu braków historii zdarzeń. To zgodność konkretnych obserwacji, bez
wniosku o pełnej równoważności przeglądarek lub o przewadze wydajności.

Poza REA zachowano 45 referencyjnych pudełek CSS z osobnego Chrome. Test
porównawczy wykazał maksymalną różnicę 0,00001145 piksela CSS. Siedem
niezmodyfikowanych par reftestów SVG z przypiętego WPT daje identyczne obrazy
źródła i referencji w naszym rendererze. Te podzbiory nie są pełnym WPT.

Normatywne reguły odczytów i zdarzeń wynikają z
[CSSOM View](https://drafts.csswg.org/cssom-view/) oraz
[CSSOM](https://drafts.csswg.org/cssom/). Zakres implementacji i przybliżenia
są opisane w CSSOM_VIEW.md i SVG.md.

## Źródła pierwotnego rozpoznania

- REA: https://github.com/morluto/rea
- Potok renderowania Chromium: https://developer.chrome.com/blog/inside-browser-part3
- Cykl dokumentu Blink: https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/dom/document_lifecycle.h
- Architektura procesów: https://www.chromium.org/developers/design-documents/multi-process-architecture/
- HTML i pętla zdarzeń: https://html.spec.whatwg.org/multipage/webappapis.html
- WPT: https://web-platform-tests.org/
- Test262 i wymagania harnessu: https://github.com/tc39/test262/blob/main/INTERPRETING.md
- Typy MIME JavaScript: https://mimesniff.spec.whatwg.org/#javascript-mime-type
- Blokowanie zasobów skryptowych według MIME i nosniff: https://fetch.spec.whatwg.org/#should-response-to-request-be-blocked-due-to-mime-type

Dane encji HTML pobrano z normatywnego pliku https://html.spec.whatwg.org/entities.json. Nie osadzono gotowego parsera HTML.
