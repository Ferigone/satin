# Message channels

`MessageChannel` creates two entangled `MessagePort` objects in the document.
Each port is an EventTarget. A port starts receiving tasks after `start()` or
its first `onmessage` assignment. Listeners added with `addEventListener`
need an enabled queue. Each message dispatch has its own microtask checkpoint.
The port task source preserves the enqueue order of eligible messages.
[HTML channel messaging](https://html.spec.whatwg.org/multipage/web-messaging.html#channel-messaging)

`postMessage` snapshots its data through the same graph walk as
`structuredClone`. Both the iterable transfer argument and the
`{transfer: iterable}` form use Web IDL sequence conversion. Delivery creates
a trusted `MessageEvent`. Its frozen `ports` array contains the transferred
ports in list order. References to one of those ports inside `event.data`
share the destination wrapper. [MessageEvent](https://html.spec.whatwg.org/multipage/comms.html#the-messageevent-interface) ·
[Web IDL overload resolution](https://webidl.spec.whatwg.org/#es-overloads)

Transferring a port moves its endpoint and incoming packets, detaches the old
wrapper and gives the destination a disabled queue. The endpoint relationship
survives moving either port or both. Transfer preparation checks object brands
and duplicates before visiting graph getters. Every source is revalidated
after traversal before any backing store or endpoint moves. The sender
rechecks its connection before queueing a packet, including when a getter
transfers or closes a port. Listing the sender raises `DataCloneError`.
Listing the receiver transfers it and discards the packet.
[Structured serialization with transfer](https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer) ·
[MessagePort posting steps](https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps)

`close()` detaches a port and disconnects the pair. Previously queued tasks
remain available to their receiver. Traced endpoint and packet slots preserve
reachable queues and allow unreachable disabled cycles to be reclaimed. The
task source roots runnable queues. The queue limits are 256 packets per
endpoint and 2,048 packets per document, alongside the VM's work and allocation
limits. [MessagePort lifecycle and collection](https://html.spec.whatwg.org/multipage/web-messaging.html#ports-and-garbage-collection)

This implementation runs in one document realm. Cross-document routing,
worker messaging and receive-time deserialization across realms remain open.

The verifier serves each original Window WPT specimen in its own page with
the required support script and checks the test and support bytes against
pinned Git blob hashes. The local harness implements the assertions used by
the selection. Authored cases exercise transfer identity, microtask ordering,
getter reentry and receiver close. Each browser's report retains its observed
statuses and close behavior.

```powershell
node tools/verify-message-ports.mjs target/release/rust-browser.exe target/message-ports-verification both RELEASE_SHA256
```

Set `REA_CLI` to the installed REA CLI script to run owned headless Chrome.
[Pinned WPT](https://github.com/web-platform-tests/wpt/tree/fa649586f35119af9032312ef21e300e41d1c3b6/webmessaging)

The alpha.13 release and Chrome 157 each passed 37 of 38 selected WPT cases
and all three authored cases. The Rust test failure reaches the missing
`FileReader` API after receiving a Blob. Chrome's failure is the closed-port
transfer assertion. In the queued-close observation, Rust delivered both
packets and Chrome delivered the first.
