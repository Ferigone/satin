# Local bookmarks

Bookmarks are a list of deliberately saved page links. Start-page shortcuts
are tiles pinned to a particular surface. Firefox makes the same distinction
between [bookmarks](https://support.mozilla.org/en-US/kb/bookmarks-firefox) and
[New Tab shortcuts](https://support.mozilla.org/en-US/kb/customize-items-on-firefox-new-tab-page).
Saving or removing a Satin bookmark does not add, move, or remove a Start tile.

Use **Ctrl+D** to save the current page. In the **… → Bookmarks** menu you can
also choose **Bookmark this page**, open a saved page in the current tab, or
choose **Remove bookmark** for the current page. Open the menu with **Alt+F**
and select **Bookmarks** with the arrow keys and **Enter**. In the list, use
**Up/Down** and **Enter** to open a link, **Delete** to remove the selected
saved link even if it cannot be opened, and **Esc** to close.

The app saves up to 128 entries, newest first, to `bookmarks.json` in the
owning profile directory. Only absolute HTTP(S) URLs without embedded
credentials are eligible. Entries are keyed by canonical URL; query strings
and fragments are retained. Saving the same URL twice neither adds a duplicate
nor changes its name. Titles and URLs are local, plain text in the profile.
A failed save leaves the prior list intact; malformed or unsupported files
are left untouched without preventing normal browsing.

## Verification

```powershell
cargo test -p rust-browser --locked -j 1 app::bookmarks::tests
cargo build -p rust-browser --locked -j 1
node tools/verify-bookmarks.mjs target/debug/rust-browser.exe
```

The Rust tests use disposable directories and check persistence, duplicate
handling, URL validation, version handling, and failed writes. The Node
verifier serves only two authored pages on loopback. It uses three hidden
native runs and a fresh profile below `target/bookmark-verification`, checking
keyboard/menu save and removal, reopening saved pages after restart, and the
exact contents of the bookmark file. It does not use desktop input or a real
profile.
