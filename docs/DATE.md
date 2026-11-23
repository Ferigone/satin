# Date in the JavaScript engine

`Date` represents time in UTC milliseconds. Calendar arithmetic uses the proleptic Gregorian calendar, and `TimeClip` limits values to ±100 million days from the Unix epoch. The engine implements construction, UTC methods, field getters and setters, and ISO serialization in Rust. [ECMAScript Date objects](https://tc39.es/ecma262/multipage/numbers-and-dates.html#sec-date-objects)

`Date.parse` accepts the ECMAScript format, including extended years, UTC offsets, and date-only values. It also supports the forms returned by `toString` and `toUTCString`, and selected dates with English month names or numeric components. Parsing accepts up to 512 UTF-16 code units.

## Local time

Windows provides the current time zone and year-specific daylight saving rules. During a repeated hour, the constructor and local methods choose the earlier instant. During a skipped hour, they use the offset before the transition. A Windows test covers both cases using Eastern Standard Time rules in isolation. [Win32 time zone conversion](https://learn.microsoft.com/windows/win32/api/timezoneapi/nf-timezoneapi-systemtimetotzspecificlocaltimeex)

Windows time zone data determines the accuracy and coverage of historical offsets. Outside the year range 1602–30826, conversion uses an equivalent year with the same weekdays and leap-year status. Other platforms currently use UTC. The `toLocale*` methods use the engine's basic text format. `Intl.DateTimeFormat` support and `Temporal` integration remain open.

## Verification

```powershell
node tools/test262/runner.mjs --test262 PATH_TO_TEST262 --worker target/release/rb-js.exe --path built-ins/Date --output target/date-verification/test262.json
node tools/verify-date.mjs target/release/rust-browser.exe target/date-verification/reference --chrome-reference
```

The Test262 runner uses unmodified files from revision `48221d7c1f7745dc13e420a2e850a248c0b2afb4`. The optional REA comparison runs the same page in a separate Chrome process and checks winter and summer offsets and constructors near daylight saving transitions. The script uses Node's system time zone for the Chrome session. Set `DATE_REFERENCE_TIMEZONE` to choose an IANA time zone identifier. [Date verifier](../tools/verify-date.mjs)
