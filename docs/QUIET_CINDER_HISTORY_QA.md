# Quiet Cinder: local browsing history

`Ctrl+H`, the overflow menu's **Historia** row, and `about:history` open the
same browser-owned page. Back and Forward still use each tab's independent
navigation stack.

The current history lane retains up to 200 distinct, recent, committed
top-level HTTP(S) GET addresses. It does not import restored sessions, record
failed navigations, replay POST, or retain credentials, query strings, or
fragments. Addresses with those fields are skipped rather than truncated.
The bounded `Visits.v1.json` lives under the owning profile's lock. An unreadable
file is preserved. Only the static native-owned history document may dispatch
individual removal and confirmed clear. The page currently shows UTC time and
has no text filter.

After building this exact checkout, run the coordinator's
`run-seeded-native.mjs <staged-exe> <commit> <scratch-evidence> history`.
It uses loopback pages and fresh profiles at each width. It checks a real
visit list, canceling clear, removing one entry, and explicitly clearing it
after opening the same profile again. Do not infer a passing native test
from the standalone store/document tests.
