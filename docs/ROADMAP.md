# Roadmap

Satin is building an independent web engine in Rust for modern sites and applications. Windows is the first platform. Performance work balances memory use and responsiveness while preserving web compatibility.

The current compatibility priority is daily use of popular sites, starting with YouTube and Twitch. Verification covers page startup, usable controls, and actual playback as separate milestones.

## Alpha.16

The new tab has an address and search field and saves custom site shortcuts in the profile.

Satin plays fragmented MP4 on Windows through Media Source Extensions. It can buffer separate H.264 and AAC tracks and start playback before the final segment. Try `about:streaming`. The JavaScript engine handles larger data-heavy scripts within its existing memory budget. Pages can read classic response timing and create and initialize legacy DOM events. YouTube startup progresses further, with player compatibility still in development. [Release verification](VERIFICATION.md)

## Alpha.15

Satin adds progressive H.264/AAC playback on Windows, with pause and seek. Windows builds are distributed through GitHub Releases. Installed copies check for newer releases, verify the payload, and activate it at the next launch. The local Satin Preview shortcut opens a build from the current checkout with a separate profile.

## Alpha.13

`MessageChannel` and `MessagePort` pass cloned data and transfer ports within a document. Windows opens a file picker for file inputs, and `FileList` and `FormData` preserve user-selected `File` objects. `ImageBitmap` decodes, scales, paints, clones, and transfers images. Fetch sends a streamed request body over HTTP/2. Try `about:messages` and `about:files`. [Message ports](MESSAGE_PORTS.md) · [Structured clone](STRUCTURED_CLONE.md)

## Alpha.12

BigInt and 64-bit typed arrays add numeric and binary operations. Object literal getters and setters also work with computed property keys. Structured cloning transfers buffers and copies `Blob`, `File`, `DOMException`, and `ImageData`. The browser runs unmodified BigInt and accessor WPT cases in a document. Try `about:numbers`. [Structured clone](STRUCTURED_CLONE.md) · [Release verification](VERIFICATION.md)

## Alpha.11

Canvas 2D paints and exposes pixels in the native renderer. Web Streams supports BYOB readers for byte streams and pipes for transforming and writing data. The DOM uses the same `EventTarget` as standalone targets and `AbortSignal`. `structuredClone` clones object graphs and transfers buffers. Try `about:canvas` or the text transform on `about:bytes`. [Structured clone](STRUCTURED_CLONE.md) · [Release verification](VERIFICATION.md)

## Alpha.10

Web Storage persists local storage in the profile and keeps session storage with the tab. The engine adds `Date`, private class elements, interval timers, and animation frame callbacks. Grid supports named lines and areas and repeats based on available width or height. Try `about:time`. [Date and time](DATE.md) · [Transport and tasks](NAVIGATION.md) · [Grid](GRID.md)

## Alpha.9

Fetch delivers headers before receiving the full body. Body chunks go through a bounded queue under the reader's control. The VM supports binary data and generators. `Blob`, `File`, and `FormData` work with Fetch and form navigation. Try `about:bytes` to exercise these paths together. [Release verification](VERIFICATION.md) · [Transport](NAVIGATION.md)

## Alpha.8

The JavaScript standard library adds keyed collections and a native `RegExp` implementation. `MutationObserver` integrates with the parser and DOM. History and Location support URL updates within the active document and restore history state. Try `about:routes` to move between two views. [History](HISTORY.md) · [DOM observers](MUTATION_OBSERVERS.md)

## Alpha.7

Asynchronous transport can cancel pending I/O. The VM resumes async functions and modules through the microtask queue. The host runs dynamically inserted scripts, maintains the module graph for `import()`, and exposes `AbortSignal` and `URL`. Try `about:async` to complete or cancel a task. A local server checks script ordering, active connection behavior, and form encoding. [Dynamic scripts](DYNAMIC_SCRIPTS.md) · [Release verification](VERIFICATION.md)

## Alpha.6

Regular navigation uses the incremental HTML decoder and parser with Fetch and the VM. This release adds script execution while parsing, `document.write`, a static module graph, classes, selected live DOM collections, and web fonts. The desktop launcher ran alpha.6, and clicking in `about:modules` updated the module counter in the title to 1. [Streaming HTML](HTML_STREAMING.md) · [Fonts](FONTS.md) · [Release verification](VERIFICATION.md)

## Alpha.5

The engine combines SVG and XML parsing and rasterization, positioned layers and overflow, 2D matrices, and CSSOM measurements. The VM adds templates, destructuring, defaults, rest and object spread, optional chaining, and logical assignment.

Use `about:graphics` to move and rotate an element, measure geometry, click an SVG shape, and scroll a panel. A shared local scenario compares the graphics paths with a reference browser. [SVG](SVG.md) · [CSSOM View](CSSOM_VIEW.md) · [CSS layout](CSS_LAYOUT.md) · [JavaScript](JAVASCRIPT.md) · [Reference observations](RESEARCH.md)

## 1. Browser foundation

The foundation includes Rust implementations of HTML, CSS, DOM, block and inline layout, text, links, and images. The app provides HTTP and HTTPS, browsing history, tabs, a profile, headless mode, a launcher, and versioned installation. Release checks cover engine tests, HTTPS requests, rendered output, and launching the installed copy.

Alpha.4 was available through the desktop shortcut. It passed 335 Rust tests, strict Clippy, HTTPS checks, and 45 reference checks for a form and session scenario. The installed launcher reported alpha.4. A synthetic click on `about:forms` set the title to the form success state. The shortcut target, manifest, and installed binary hash were verified independently of setup. The profile and previous alpha.3 build were preserved. [Release verification](VERIFICATION.md) · [Reference observations](RESEARCH.md)

Alpha.2 expanded HTML error recovery, formatting element adoption, and table parsing. It added selector indexing, a bounded text shaping cache, and a Flexbox subset. Alpha.3 added contextual fragment parsing, nested templates with inert contents, and Grid with limits on allocation and layout work.

An installed copy ran `about:platform` outside the build directory with a separate profile. It presented a frame and displayed the platform counter at 3 in the title after a synthetic click. This page combines Promise and JSON, template cloning, DOM mutations, and Grid. Alpha.4 added `about:forms` for editing, selecting options, resetting values, and testing local form submission.

Alpha.4 expanded SVG and MathML foreign content parsing in HTML documents and fragments. It handles element and attribute namespaces, case adjustments for SVG names, CDATA, integration points, and switching back to HTML parsing rules. Cloning and import preserve namespace metadata and `template.content`. The pinned html5lib suite now runs every previously disabled SVG and MathML case. The test data README records totals, known failures, and unsupported cases. At this stage, the support covered parsing and DOM integration. [HTML parser test data](../crates/engine/data/html5lib/README.md)

Remaining foundation work includes broader HTML and CSS coverage, typography, text selection, incremental updates, content-process isolation and IPC, and a GPU rendering path. Grid still needs subgrid and more sizing rules. Flexbox, positioned layout, web fonts, and CSS animations also need broader coverage.

## 2. JavaScript and interactive pages

The scope includes the language, its standard library, and the platform for interactive sites. It covers DOM object identity and liveness, scheduling, microtasks and events, Promise, modules, forms, Fetch, cookies, and same-document history. Language conformance is measured against a pinned Test262 revision with the applicable variants and harness. Reports retain unsupported cases and failures.

Alpha.2 introduced the Rust lexer, parser, bytecode compiler, and VM with UTF-16 values, closures, an object model, and tracing garbage collection. The host integrated a DOM subset, clicks, timers, and microtasks. Classic scripts ran after parsing. A separate test process and Test262 runner reported language coverage and gaps. [JavaScript](JAVASCRIPT.md)

Alpha.3 added the following:

- Native Promise records with thenable resolution, `then`/`catch`/`finally` chaining, and a shared FIFO queue for GC-traced reactions and microtasks. `JSON.parse` and `JSON.stringify` use a dedicated grammar and UTF-16 serialization, support revivers and replacers, and enforce work and allocation limits.
- Buffered Fetch, Headers, Request, and Response with `text()`, `json()`, cloning, and `bodyUsed` checks. HTTP and HTTPS work ran in bounded thread pools outside the window loop and navigation. Fetch handled CORS preflight, header validation, redirects, and opaque `no-cors` responses.
- `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `DocumentFragment`, `template.content`, `cloneNode`, `replaceChildren`, and insertion and removal. Fragments preserve context, the script flag, and the document mode. The update checks hierarchy, cycles, depth, node budget, and arena storage before committing. Scripts inserted through these methods stay inert.
- The `about:platform` demo and integration tests that combine Fetch responses, Promise and JSON, templates, and DOM mutations. [JavaScript](JAVASCRIPT.md) · [CSS layout](CSS_LAYOUT.md) · [HTML parser test data](../crates/engine/data/html5lib/README.md)

Alpha.4 added the following:

- A separate Symbol type, symbol keys, the `Symbol.for`/`Symbol.keyFor` registry, synchronous iterators, Array and String iterators, `Array.from`, `for...of` with a declared identifier, and spread in arrays, calls, and `new` expressions. Shared iterator handling covers closure and work limits. Some built-ins that consume well-known symbols remain pending.
- `Promise.all`, `race`, `allSettled`, and `any` with the synchronous iterator protocol, result ordering, thenables, and `AggregateError`. The engine implements Promise, JSON, the object model, and garbage collection in Rust. [JavaScript](JAVASCRIPT.md)
- Form state for dirty and default values, checkboxes and radio groups, select and option elements, ownership outside a form's subtree, disabled controls, reset, cancelable events, and a validation subset. The native window supports text and password editing, keyboard selection and clipboard, Tab and Shift+Tab, dropdowns and listboxes, and submitter activation. GET and URL-encoded POST navigation apply redirect-specific rules for the request method and body. [Forms](FORMS.md)
- A CookieStore shared by navigation, Fetch credentials, and `document.cookie`. It supports HttpOnly, Secure, SameSite, domain and path rules, prefixes, ICANN and private rules from a pinned Public Suffix List, and basic top-level partitioning. On Windows, persistent cookies are protected with DPAPI and written atomically under the profile lock. Session cookies expire when the process exits. [Cookies](COOKIES.md)

Seventeen form semantics tests and eight renderer tests cover cancellation, reentrancy, ownership, radio and select behavior, UTF-16 editing, current value snapshots, geometry, password masking, including in fallback layout, and paint clipping. `tools/verify-forms.mjs` exercises app input handlers against a local HTTP server. It checks POST/303, Origin and Cookie headers, Promise and JSON with Fetch, HttpOnly visibility, and reopening the profile. The optional reference run uses a separate browser process. [Forms](FORMS.md) · [Reference observations](RESEARCH.md)

Remaining language work includes Intl, Proxy, SharedArrayBuffer, import attributes, RegExp Unicode sets, and more of the standard library. Fetch needs additional request options. Ports need cross-context transfer. Forms need broader validation, date and color pickers, accessibility, and drag selection. Navigation history needs BFCache, full state serialization, and POST resubmission. Cookies need all browsing contexts and privacy policies. IndexedDB, credential storage, CSP, and local network access restrictions remain pending.

Destructive `document.write` and `document.open`, resource updates after DOM mutations, specialized events, Shadow DOM retargeting, and collections with named properties also remain pending. Compatibility verification will expand to more real applications.

## 3. Web platform

Remaining work includes IndexedDB, workers and service workers, offline applications, WebAssembly, broader SVG and Canvas 2D coverage, MathML layout, OffscreenCanvas, WebGL, WebGPU, audio and video, WebRTC, permissions, and native integrations. The feature matrix uses web standards and comparisons with a pinned reference browser. Protected media and services require separate DRM and CDM integration and verification.

## 4. Performance and a practical release

Planned work includes profiling and allocation controls, incremental layout and painting, tiling and GPU composition, cache budgets, and a JIT compiler. Verification uses WPT, Test262, real-site workflows, security tests, and long sessions. Measurements cover the entire process set, memory, GPU and CPU use, startup, and interaction latency.

## Release criteria

Each completed stage produces a tested installation under the same desktop shortcut, with the profile and the previous build available. Release notes record supported features and known gaps. Full project completion requires implementation and verification across the roadmap.
