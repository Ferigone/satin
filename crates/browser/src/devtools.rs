//! Desktop DevTools contract for App integration (`mod devtools` in `main.rs`):
//!
//! - Store one `DevTools::default()` per window. Call `toggle()` on F12;
//!   `is_open()` reports whether App should allocate a dock. App owns placement,
//!   viewport sizing, focus, and event routing.
//! - With the same logical `Rect` for paint and input, call
//!   `paint(canvas, fonts, bounds, scale, active_tab_id)`,
//!   `click(bounds, active_tab_id, x, y)` on primary activation and
//!   `scroll(bounds, active_tab_id, x, y, delta_y)` for the wheel (positive
//!   moves down the list). `contains(bounds, x, y)` lets App isolate other
//!   pointer events. Click and scroll return whether the dock consumed them.
//! - `begin_navigation(tab_id, navigation_id)` at the real navigation boundary,
//!   before dispatching any work, clears the previous attempt. Never call it
//!   for same-document history/fragment changes, tab switches, or dock toggles.
//!   The error-page replacement advances App's job generation but is not a new
//!   inspection attempt: keep the failed attempt and its real outcome visible.
//! - `close_tab(tab_id)` drops that tab's data. Ingestion uses the originating
//!   tab and navigation IDs even for an inactive tab; unregistered, closed,
//!   and superseded scopes must not be created from events.
