# Cookies i sesja sieciowa — alfa.4

CookieStore jest własnym kodem Rust. Wątki nawigacji, zasobów i Fetch oraz
document.cookie korzystają z jednego magazynu danego profilu. Transport HTTP/TLS
pozostaje biblioteką infrastrukturalną; nie korzystamy z jej gotowego cookie jar.
Żadna blokada magazynu nie jest trzymana podczas sieci ani wywołania JavaScript.

Przechowujemy oktety nazw/wartości, domenę, ścieżkę, flagi, czas utworzenia i
wygaśnięcia oraz klucz partycji. HTTP zachowuje surowe bajty; document.cookie
koduje wejście i dekoduje wynik przez UTF-8. Dopasowanie obejmuje host-only,
Domain, granice ścieżek, Secure, HttpOnly, prefiksy __Secure-/__Host-/__Http-/
__Host-Http- oraz schemeful SameSite. Brak atrybutu SameSite oznacza Lax z
dwuminutowym wyjątkiem dla nowych cookies przy nawigacji POST. Cross-site
przekierowania zachowują skażenie kontekstu także po powrocie do klienta.

Do obliczania domen/site i odrzucania publicznych sufiksów służy dołączona,
przypięta Public Suffix List z regułami ICANN i private, wildcard i wyjątkami.
Pochodzenie, licencja i hash są w engine/data/publicsuffix/README.md.
Partitioned wymaga Secure i zachowuje odrębne dane dla site klienta/top-level.
Obecnie dokumenty są top-level; pełna integracja ramek pozostaje do wykonania.

Fetch respektuje credentials omit/same-origin/include przy wysyłaniu i odbiorze
cookies; preflight ich nie wysyła i nie zapisuje. Same-origin przestaje wysyłać
po skażeniu odpowiedzi przez przekierowanie. Set-Cookie jest przetwarzane na
poziomie sieci, przed sprawdzeniem CORS, i nie staje się widocznym nagłówkiem JS.
Spóźniona odpowiedź po zaobserwowaniu anulowania/deadline nie zapisuje cookies.

| Budżet | Wartość |
| --- | ---: |
| Nazwa i wartość jednego cookie | 4096 bajtów |
| Wartość pojedynczego atrybutu | 1024 bajty |
| Magazyn | 3000 wpisów / 16 MiB szacowanych danych |
| Domena i partycja | 180 wpisów |
| Maksymalna trwałość | 400 dni |

Wygaśnięte wpisy są usuwane, a nadmiar jest ograniczany przy zapisie. Ponowne
wczytanie sprawdza wszystkie budżety, prefiksy, klucze i kanoniczne domeny przed
atomowym zastąpieniem stanu; błędne dane nie zastępują bieżącego magazynu.

Cookies z określoną ważnością są zapisywane w Cookies.v1.dpapi pod blokadą
profilu. Windows DPAPI działa w zakresie bieżącego konta, bez interfejsu
interaktywnego; zapis pliku jest atomowy. Cookies sesyjne kończą się po wyjściu
z aplikacji. Cofnięcie do alfy.3 zachowuje oddzielny plik, którego starsza wersja
nie odczytuje. Headless bez --profile jest nietrwały; jawny profil używa tej samej
blokady i ochrony. Testy nie korzystają z domyślnego profilu użytkownika.

Pozostają interfejs zarządzania/usuwania danych witryn, pełna polityka third-party
i uprawnień, uwierzytelnianie HTTP, kalibracja Expires według serwerowego Date,
Priority, automatyczne aktualizacje listy sufiksów i pełna integracja iframe.
DPAPI nie zapewnia izolacji procesu treści ani wiązania magazynu do konkretnego
EXE; takie mechanizmy wymagają osobnego etapu. Transport statycznych zasobów i
Fetch blokuje aktywny mixed content, a dostęp do plików opiera na rzeczywistym
URL dokumentu, niezależnie od modyfikowalnego base.

Testy: moduł cookies, cookies_transport, profil aplikacji oraz
tools/verify-forms.mjs. Próba używa własnego serwera i jawnie syntetycznych
danych, sprawdza formularz POST/303, HttpOnly, Fetch i ponowne otwarcie profilu.
Nie jest to pomiar pełnej zgodności cookies/WPT ani całkowitego RAM.

Źródła semantyki: [RFC6265bis, roboczy projekt HTTP WG](https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html),
[Fetch](https://fetch.spec.whatwg.org/#cookies), [document.cookie](https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie),
[PSL](https://publicsuffix.org/list/), [DPAPI](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata).
