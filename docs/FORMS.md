# Forms in 0.1.0-alpha.4

The Rust forms module uses the DOM, node identity, and shared event dispatcher. Current control values are stored separately from HTML attributes. User edits and assignments to `input.value` update current state while the `value` attribute records the default. `defaultValue`, `defaultChecked`, `defaultSelected`, reset, and cloning preserve the distinction. [Forms implementation](../crates/engine/src/script/forms.rs)

## Supported behavior

- `input`, `textarea`, `select`, `option`, and `button`. Form ownership includes the `form` attribute, disabled fieldsets with the first-legend exception, and disabled option groups.
- UTF-16 text values, selection ranges, caret movement, insertion, full surrogate pair deletion, multiline editing, and user editing limits from `maxlength`. Programmatic value updates are silent.
- Checkbox and radio pre-activation visible in `click`, rollback on cancellation, radio groups, and `input` and `change` events.
- Single and multiple selection, live `.value` and `.selectedIndex` reads, state cloning, and keyboard and pointer selection. Tab and Shift+Tab dispatch cancelable `keydown` and then follow positive `tabindex` and tree order. They skip disabled controls, the `hidden` attribute, and controls hidden by inline styles.
- `focus`, `blur`, `beforeinput`, `input`, `change`, `submit`, `reset`, cancelable default actions, and `event.submitter`.
- Direct `submit()` bypasses validation and the `submit` event. `requestSubmit()` and submit buttons use the regular path. A direct submission inside a canceled submit handler also works.
- Successful controls in tree order, repeated field names, the chosen submitter, enabled selected options, `_charset_`, CRLF normalization, and UTF-8. GET replaces the action URL's query. POST preserves the query and sends an `application/x-www-form-urlencoded` body. The browser navigates outside the JavaScript callback.

Validation covers missing required values, required checkboxes and radios, basic email and URL checks, and numeric bounds. At this stage the remaining Constraint Validation API work included `pattern`, some `step` cases, full length, date, and email rules, and validation messages. Other gaps included control-state CSS selectors, complete keyboard and IME events, word editing, native pickers, file uploads, `FormData` and `formdata`, multipart and plain-text POST, dialogs, named targets, and opening new windows. DOM collections used snapshots. Named properties such as `form.foo`, complex `input.type` transitions, and some effects of moving controls between trees also remained open. [Later form and file milestones](ROADMAP.md)

A mutable value allows up to 1 Mi UTF-16 code units, with 8 MiB of shared value data. Each submission allows up to 4,096 entries and 4 MiB of encoded data, with eight pending submissions. Editing and DOM changes use the same VM task budget. Renderer snapshots index forms, options, and radio groups in one pass. Display text is capped at 8,192 code units per control and 1 MiB total, with a separate allowance for the active control. The full value API is independent of the paint preview. Programmatic focus needs broader computed-style integration to account for visibility from external stylesheets.

## Verification

```powershell
cargo test -p rust-browser-engine --test forms --offline
```

The form tests cover mutations, cancellation, GET and POST entries, UTF-16 editing, reentrancy, reset, cloning, focus, and a large radio group fixture. Rendering tests run with the release checks. [Renderer tests](../crates/engine/tests/forms_rendering.rs)

The implemented subset follows the HTML form algorithms. [Form control infrastructure and submission](https://html.spec.whatwg.org/multipage/form-control-infrastructure.html) · [Input](https://html.spec.whatwg.org/multipage/input.html) · [Select, option, and textarea](https://html.spec.whatwg.org/multipage/form-elements.html)
