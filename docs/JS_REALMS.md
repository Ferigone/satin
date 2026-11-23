# Shared VM realms for blank iframes and Test262

**Realm creation is not exposed.** The heap identity guard and the private
global environment split do not create realms or expose `contentWindow`.
The [browser probe](../tools/fixtures/iframe-script-realms.md) checks the
intended behavior without changing the public ES5.1 suite.

## VM seam

- Keep one heap, budget, symbol registry, and job queue per JS agent. Move
  globals, global environments, intrinsics, and realm-local caches out of
  agent state into GC-traced realm records. The global object and global
  `this` must be distinct slots so a browser realm can use a `WindowProxy` for
  global `this`. A new `Vm` is still an independent heap; its object handles
  are not transferable.
- The first stage traces the binding object and global `this` independently
  from the existing global environment. `Vm::global()` returns the binding
  object; the built-in `globalThis` property and a sloppy function's nullish
  receiver use the environment's `this`. This does not yet activate another
  global environment or associate functions with their defining realms.
- Provide host-only `create_realm`, `realm_global`, and scoped `with_realm`
  operations. Realm handles must be validated against the owning heap and
  remain live only while traced or explicitly retained. Create realms under
  the existing memory budgets. Restore the active realm after nested calls
  and errors; entering a realm must not reset the surrounding task budget.
- Entry into a function uses its owning realm (including built-ins); jobs
  and suspended execution preserve the realm they need when resumed.
  Resolved global identifier references now capture the binding object of
  their own environment for read, write and delete. Suspended operands trace
  that object. Unresolvable sloppy writes still use the active global, per
  `PutValue`; this requires correct owner-realm entry.
  `GetPrototypeFromConstructor` must use the realm of `newTarget` for
  intrinsic fallback, not the realm of the active constructor. Verify this
  with pinned `built-ins/Promise/proto-from-ctor-realm.js` after wiring
  `$262.createRealm()`.
- Trace globals, retained functions, running/suspended contexts, queued jobs,
  and host roots. A live callable may retain a removed frame's realm; once
  no such refs remain, the realm and its intrinsics must be collectible.
  Avoid permanent roots for each frame's intrinsics and DOM wrappers.

## Host boundaries

Lane 50 can wire `$262.createRealm()` and its returned `$262.evalScript` to
the realm seam without any browser dependencies. Current-realm
`$262.evalScript` can land independently. Until real creation works, report
the host gap; do not implement it with separate VMs or copied values.

The browser owns the active document, sandbox flags, inherited origin and
policy, and `WindowProxy` checks. Share the VM agent for same-origin initial
blank frames, not the parent's JS global or DOM. Inherit the creator's
origin unless sandbox flags require an opaque origin. Gate script execution
separately from document access. `contentWindow` and the indexed `frames`
properties must use the same WindowProxy as each other. Recheck effective
access on the active document so a proxy retained across navigation cannot
expose the old same-origin global as if it were the new one. Unsupported
navigations must not fall back to an accessible blank document.

Lane 04 owns the document parser and `open/write/writeln/close`. Frame
documents will use that parser in the target realm, with the caller's HTML
entry settings preserved for the `document.open()` origin check. Those
settings are not the same thing as the active JS realm. Lane 06 owns Window
error events. Before exposing frame APIs, verify reentrant parent callbacks,
fresh globals and intrinsics, removal/collection, sandboxed script denial,
and cross-origin denial even through retained proxies.
