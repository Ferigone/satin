# Satin alpha.19 integration

This release integrates the desktop application, engine compatibility and
Worker Fetch work staged after alpha.18. It preserves the existing start page
and browser chrome design.

| Source | Disposition |
| --- | --- |
| `satin/app-integration` | Desktop glyphs, keyboard focus, crowded tabs, address editing, startup settings, GET retry, bookmarks, shortcut help, per-tab history, and preservation of URL fragments. |
| `satin/engine-next`, including `satin/compat-integration` | Bounded parser changes, viewport sizing, MSE rebuffering, Wasm memory, and browser QA measurements. |
| `satin/worker-fetch-integration` | Bounded worker task delivery and Blob Worker Fetch, with CORS, CSP, cancellation, and loopback tests. |
| Tasks 03 and 12–16 | Shared DOM prototypes, clone and Blob URL regressions, User Timing retention, media volume events, and import-map behavior. |
| Tasks 18–20 | Link keyboard navigation, native keyboard checks, a deterministic paint benchmark, and a loopback WebSocket chat fixture. |
| Task 28 | Offline download row model and tests only. There is no Downloads panel or production download service. |
| Tasks 01–02, 04–11, 17, 21–27, 29–30 | Already represented by the alpha.18 base or one of the integration branches above; duplicate implementations were not reapplied. |
| Pre-alpha.18 compatibility and recovery refs | Kept intact. The structured-clone packet implementation was carried forward through the alpha.18 checkpoint. The separate recovery snapshot is an older tree, not a forward change. |

The Worker Fetch and media fixtures verify local paths; they do not establish
Twitch or YouTube playback. The browser is still an alpha with partial website
compatibility.
