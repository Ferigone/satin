# Formularze — zakres alfy 0.1.0-alpha.4

Silnik formularzy jest własnym modułem Rust (`engine/src/script/forms.rs`). Używa istniejącego DOM, tożsamości węzłów i dyspozytora zdarzeń. Stan bieżących wartości pozostaje oddzielony od atrybutów HTML: wpisywanie lub `input.value = ...` nie podmienia `value` w źródle dokumentu. `defaultValue`, `defaultChecked`, `defaultSelected`, reset i klonowanie zachowują ten podział.

Obsługiwany zakres:

- HTML `input`, `textarea`, `select`, `option`, `button`, właściciel `form`, powiązanie przez `form=id`, wyłączone `fieldset` z wyjątkiem pierwszego `legend`, wyłączone grupy opcji.
- Wartości tekstowe i UTF-16, wybór zakresu, kursor, wstawianie tekstu, usuwanie pełnej pary surogatów, zaznaczanie, wielowierszowe pola i ograniczenie użytkowego `maxlength`. Zmiana programowa wartości nie emituje `input`/`change`.
- Checkbox/radio z preaktywacją widoczną w `click`, cofnięciem przy anulowaniu, grupami radio oraz zdarzeniami `input` i `change`.
- Wybór pojedynczy/wielokrotny, aktualne `.value`/`.selectedIndex`, klonowanie stanu, obsługa opcji przez klawiaturę i wskaźnik. Tab i Shift+Tab wysyłają anulowalne `keydown`, a następnie porządkują kontrolki według dodatniego `tabindex` i kolejności drzewa; pomijają wyłączone, `hidden` i ukryte bezpośrednim stylem.
- `focus`, `blur`, `beforeinput`, `input`, `change`, `submit`, `reset`, anulowanie domyślnych działań i `event.submitter`.
- `submit()` pomija walidację i zdarzenie `submit`; `requestSubmit()` oraz przyciski przechodzą zwykłą ścieżkę. Bezpośrednie `submit()` wewnątrz anulowanego handlera jest obsługiwane.
- Udane pola w kolejności drzewa, powtarzające się nazwy, wybrany submitter, wybrane niewyłączone opcje, `_charset_`, normalizacja CRLF i UTF-8. GET zastępuje query adresu akcji; POST zachowuje query i emituje ciało `application/x-www-form-urlencoded`. Wysłanie jest przekazywane do warstwy nawigacji, poza callback JavaScript.

Walidacja obejmuje brak wymaganej wartości, wymagane checkbox/radio, prostą kontrolę email/URL oraz granice liczb. Nie jest jeszcze pełnym Constraint Validation API: m.in. `pattern`, wszystkie odmiany `step`, kompletne reguły długości, dat i adresów email oraz komunikaty walidacyjne wymagają dalszej implementacji. Brakuje pełnych selektorów CSS stanów kontrolki, wszystkich zdarzeń klawiatury/IME, edycji słów, natywnych pickerów, uploadu plików, `FormData`/`formdata`, multipart/plain POST, dialogów, nazwanych celów i nowego okna. Kolekcje DOM są migawkami, a klasyczne nazwane właściwości `form.foo` pozostają do wykonania. Skomplikowane przejścia `input.type` i wybrane skutki przenoszenia kontrolek między drzewami nie mają jeszcze pełnej zgodności.

Budżety: 1 Mi jednostek UTF-16 na zmienianą wartość, 8 MiB współdzielonych danych wartości, do 4096 wpisów i 4 MiB zakodowanej treści jednego zgłoszenia, osiem oczekujących zgłoszeń. Edycje i operacje DOM zużywają wspólny budżet zadania VM. Migawki renderera indeksują formularze, opcje i grupy radio jednym przejściem; tekst do rysowania ma ograniczenie 8192 jednostek na kontrolkę oraz 1 MiB łącznie (plus bieżąca kontrolka). Pełne API wartości pozostaje oddzielne od tych podglądów. Widoczność z zewnętrznych arkuszy przy programowym fokusie wymaga dalszej integracji obliczonych stylów.

Weryfikacja: `cargo test -p rust-browser-engine --test forms --offline`. Zestaw sprawdza faktyczne mutacje, anulowanie i wpisy GET/POST, edycję UTF-16, reentrancję, reset, klonowanie, fokus oraz duży zestaw grup radio. Dodatkowe testy renderera znajdują się w `tests/forms_rendering.rs` i należą do wspólnej weryfikacji wydania.

Punkty odniesienia implementacji: [infrastruktura i wysyłanie formularzy](https://html.spec.whatwg.org/multipage/form-control-infrastructure.html), [input](https://html.spec.whatwg.org/multipage/input.html), [select, option i textarea](https://html.spec.whatwg.org/multipage/form-elements.html). To lista działającego zakresu rozwojowego, nie deklaracja pełnej zgodności HTML Forms.
