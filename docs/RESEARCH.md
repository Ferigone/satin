# Browser reference observations from November 10, 2026

The reference run used the official `rea-agents` CLI, version 6.8.0, and Chrome 157.0.8103.51 with an isolated test profile. The test closed its browser and local server after the run.

## Initial observations

- `list_browser_targets` identified Chrome 157.0.8103.51 and V8 15.7.112.23. Evidence ID `ev_37e57fea695c81a94469f5e63fa933bbff0fcc0bf35fcc6529af15d0df808d36`.
- The local scenario completed all six scripted actions, including waiting for page readiness, typing, clicking to change the layout and trigger Fetch, waiting for the response, and navigating to a new document. Evidence ID `ev_985bdd19b16a41d8dc7e0bcebffb3fcdda1f5082790345df265626964a24bdd4`.
- In the test document, the observed order was inline script → microtask → defer script → module → `DOMContentLoaded`. The observation applies to that document's script setup.
- The DOM snapshot showed repaired misnested `<b>` and `<i>` tags. A synchronous layout read after the style change returned a width of 300 CSS px. Fetch carried the test cookie, local storage kept its value, and `pushState` and document navigation advanced session history.
- The run opened a public GitHub repository, navigated to its `docs` directory, and waited for a visible `main` element. Evidence ID `ev_803c2b277fc6887b23c83918838351adbd3d802ed15021b6c4424964383d867e`.

The step snapshots contained the requested artifacts. The scenario reports marked event history as incomplete, so the observations cover the recorded steps. REA elapsed times include the measurement harness. A separate run exercises the same behavior in Satin.

## Alpha.5 comparisons

`tools/verify-graphics.mjs` ran the same synthetic scenario in a release build and in a separate Chrome 157.0.8103.51 process through REA. All five steps, including startup, completed. The transformed rectangle measured 52 × 112 CSS px, with `offsetWidth` of 112 and `clientWidth` of 108. The SVG circle bounds measured 60 × 60 CSS px. Absolute insets measured 15 and 10 CSS px, and the scrolled panel returned 30, 100, and −30 CSS px. The synthetic page sent the same values from Satin headless, Satin's native window, and Chrome. Hit testing found the circle. Evidence ID `ev_843c28a52f6eda91621c7152a780ee909d2aa87e7e06d10be75150e12f14032e`.

The form and session scenario passed 45 checks, including matching POST encoding, POST/303/GET navigation, and persistent cookies. Evidence ID `ev_1d8325938a2fc99da672be90579c28cc22c44e01bd6b5de50cd856da8b0aef96`. Both captures reported `completeness=incomplete` and `equality_eligible=false` because parts of the event history were unavailable. The comparisons cover the listed behaviors and measurements.

A separate Chrome run provided geometry for 45 CSS boxes. The maximum measured difference was 0.00001145 CSS px. Seven unmodified SVG reftest pairs from a pinned WPT revision produced matching test and reference images in Satin. [CSS layout](CSS_LAYOUT.md) · [SVG](SVG.md)

CSSOM View and CSSOM define measurement and event behavior. The feature notes document the implemented subset and approximations. [CSSOM View standard](https://drafts.csswg.org/cssom-view/) · [CSSOM standard](https://drafts.csswg.org/cssom/) · [CSSOM View implementation](CSSOM_VIEW.md) · [SVG implementation](SVG.md)

## Primary sources

- [REA CLI](https://github.com/morluto/rea)
- [Chromium rendering pipeline](https://developer.chrome.com/blog/inside-browser-part3)
- [Blink document lifecycle](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/dom/document_lifecycle.h)
- [Chromium multiprocess architecture](https://www.chromium.org/developers/design-documents/multi-process-architecture/)
- [HTML event loop](https://html.spec.whatwg.org/multipage/webappapis.html)
- [Web Platform Tests](https://web-platform-tests.org/)
- [Interpreting Test262 and its harness requirements](https://github.com/tc39/test262/blob/main/INTERPRETING.md)
- [JavaScript MIME types](https://mimesniff.spec.whatwg.org/#javascript-mime-type)
- [Fetch MIME and nosniff handling](https://fetch.spec.whatwg.org/#should-response-to-request-be-blocked-due-to-mime-type)

Satin's HTML parser uses entity data from the HTML standard. [HTML named character references](https://html.spec.whatwg.org/entities.json)
