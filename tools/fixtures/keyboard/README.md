# Native keyboard smoke

From the repository root on Windows, run the check with an existing Satin binary:

```powershell
.\scripts\Verify-Keyboard.ps1 -Executable .\target\release\rust-browser.exe
```

The script opens two local HTML fixtures in hidden native windows with fresh,
separate profiles. It feeds only Tab, Shift+Tab and Enter to Satin's smoke
handlers. Title assertions check `document.activeElement` during focus events,
positive `tabindex` ordering, skipped controls, a canceled Enter `keydown`,
and exactly one button click after the second Enter. The link case checks
focus in both directions, cancellation of the first link `click`, and actual
navigation on the second Enter without JavaScript setting the location.
The hidden link and the placeholder without `href` are skipped.

Logs, screenshots and a report are saved in a unique run directory under
`target/keyboard-verification`. The screenshots confirm frame presentation,
not focus-indicator appearance. This smoke does not use OS keyboard or mouse
input and does not test browser chrome focus or assistive technology.
