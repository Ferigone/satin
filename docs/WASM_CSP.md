# WebAssembly compilation policy

Satin checks Content Security Policy (CSP) before creating a WebAssembly module from bytes. Each enforcing policy uses `script-src`, falling back to `default-src` only when `script-src` is absent. The selected directive must contain `'wasm-unsafe-eval'` or `'unsafe-eval'`. A policy with neither directive does not restrict Wasm. All enforcing policies must allow compilation; report-only headers do not restrict it.

The final navigation response supplies the initial policy. A parser-created CSP `<meta>` directly inside the connected HTML head restricts the document as the parser reaches it, including during `document.write()`. Like Chromium, Satin treats comma-separated meta content as separate policies. Removing the element or adding a more permissive policy does not undo an applied restriction. The already-parsed `LoadedPage::from_html` helper starts with the final head policy; only `DocumentSession` executes scripts between parser tokens.

Compilation in a Window uses the policy when bytes reach the VM. A streaming call started before a restricting meta element can still reject if its body arrives after that element. Denial surfaces as `WebAssembly.CompileError` (a throw from the constructor or a rejected promise from an asynchronous API). `WebAssembly.validate()` and instantiating an already compiled module do not acquire new compilation authority.

A classic Blob Worker first needs permission to load via the existing Blob source check. It uses an independent copy of the creator's effective compilation policy from `new Worker(...)`, not from the time the Blob URL was made or the worker thread began. Later meta elements affect the Window and newly constructed workers, not existing workers.

This is not general CSP enforcement. JavaScript source/eval restrictions, dynamically inserted CSP meta elements, and violation events and reports are outside this implementation.

[CSP Level 3: WebAssembly integration](https://www.w3.org/TR/CSP3/#can-compile-wasm-bytes) · [HTML: CSP pragma](https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-content-security-policy)
