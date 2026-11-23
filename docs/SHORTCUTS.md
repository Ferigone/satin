# Browser keyboard shortcuts

Open **Satin menu → Keyboard shortcuts**, or enter `about:shortcuts` in the
desktop app. The help page is generated from the same bindings that handle
browser commands. Text editing, page focus, and Windows shortcuts are separate.

## Current gaps

- `Ctrl+F` and `F3` do not implement find-in-page. `Alt+F` opens the Satin menu.
- `Ctrl+1`–`Ctrl+9` do not select tabs by number.
- `Ctrl+Shift+T` opens a new tab just like `Ctrl+T`; it does not restore a closed tab.
- `Ctrl+Shift+R` and `Ctrl+F5` use normal reload, not a separate hard reload.

## Verify on Windows

From the repository root:

```powershell
cargo +1.99.1 test --locked -j 1 -p rust-browser shortcuts::tests
cargo +1.99.1 build --locked -j 1 -p rust-browser
$profile = Join-Path $PWD ("target\shortcut-profile-" + [Guid]::NewGuid().ToString("N"))
.\target\debug\rust-browser.exe --url about:demo --profile $profile `
    --smoke-window target\shortcuts.png --smoke-actions crates\browser\tests\fixtures\shortcuts.json
```

The native smoke sends synthetic input through the app's handlers, not the OS
keyboard. It verifies address focus and navigation (`Ctrl+L`), opening a tab
(`Ctrl+T`), switching to the next tab (`Ctrl+Tab`), closing the active tab
(`Ctrl+W`), and reaching help with the menu (`Alt+F`, then Up and Enter). Its
final URL and title are `about:shortcuts` and `Keyboard shortcuts`.
