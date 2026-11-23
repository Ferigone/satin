# Historia, Location i adres dokumentu

Karta i uruchomiony dokument współdzielą historię sesji. `pushState` dodaje
wpis, a `replaceState` aktualizuje bieżący. Zmiany są widoczne od razu w
`location`, `document.URL`, `document.documentURI`, rozwiązywaniu odnośników,
formularzach i względnych żądaniach Fetch. Aktualizacja adresu zachowuje
bieżący dokument, jego VM i pobrane zasoby. Pierwszy aktywny `<base href>`
zachowuje swój rozwiązany adres przy zmianie samego adresu dokumentu.

Zmiana fragmentu przez `location.hash` aktualizuje adres i może wywołać
`popstate` jeszcze w bieżącym wywołaniu. `hashchange` jest zadaniem kolejki,
więc zaplanowane wcześniej mikrozadania mogą wykonać się przed zdarzeniem.
`history.go`, `back` i `forward` zlecają nawigację przeglądarce. Przejście
wpisów tego samego dokumentu przywraca jego bieżący stan. Skróty w oknie
wykorzystują ten sam mechanizm. `PopStateEvent` i `HashChangeEvent` dziedziczą
po `Event` i korzystają ze wspólnej obsługi zdarzeń DOM.

Zapis stanu historii kopiuje graf zwykłych obiektów i tablic. Zachowuje
cykle, współdzielone elementy, puste indeksy tablic, `undefined`, liczby
specjalne i tekst UTF-16. Getter `history.state` zwraca ten sam obiekt przez
czas aktywności wpisu. Kolejne odtworzenie wpisu sięga do zapisanego stanu.
Funkcje, symbole i węzły DOM zgłaszają `DataCloneError`. Typy specjalne takie
jak `Map`, `Set`, `RegExp`, bufory i daty pozostają do dodania do serializacji.

Zmiana adresu przez History sprawdza reguły przepisywania komponentów URL.
Inny schemat, host, port lub dane uwierzytelnienia są niedozwolone. Jeśli
odtwarzany dokument przekierowuje na inną domenę, nowy dokument otrzymuje
odrębną tożsamość i pusty stan historii. Zmiany `Location` między dokumentami
korzystają z tego samego transportu co kliknięcia odnośników.

Sesja przechowuje maksymalnie 100 wpisów. Limity danych stanu wynoszą 512 KiB
na wpis i 4 MiB na kartę. Sesja po restarcie przechowuje adresy kart. Historia
poprzednich wpisów, BFCache, odtworzenie ciała POST i Navigation API wymagają
dalszej pracy. Wydanie obsługuje podstawowe przewijanie do fragmentu i zapis
położenia strony przy poruszaniu się po historii. Pozostałe tryby restauracji
dokumentów i złożone kotwice tekstowe są w rozwoju.

Próba `about:routes` zmienia widok po `pushState` i `popstate`. Map przechowuje
widoki, RegExp odczytuje fragment, a MutationObserver aktualizuje tytuł karty.
`tools/verify-history.mjs` uruchamia wspólny scenariusz w trybie bez okna,
w natywnym oknie oraz w Chrome przez REA. Serwer sprawdza żądania względem
przepisanego i zamrożonego `baseURI`, dokładną kolejność zdarzeń oraz przejście
`location.replace` z ponownym wczytaniem przez `history.go(0)`.

Specyfikacja definiuje reguły adresu, stanu i zdarzeń.
[HTML History](https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-history-interface) ·
[HTML Location](https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface)
