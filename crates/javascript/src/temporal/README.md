# Temporal standard-library implementation

Satin runs the calendar and duration algorithms from Adam Shaw’s
[`temporal-polyfill` 1.0.8](https://github.com/fullcalendar/temporal-polyfill)
in its JavaScript interpreter. The source distribution is MIT-licensed.
[License](LICENSE) · [Pinned npm archive](https://registry.npmjs.org/temporal-polyfill/-/temporal-polyfill-1.0.8.tgz)

The upstream input is `package/full/global.js`, SHA-256
`95a7fde114214858e9fc6379f3c2f8d9d36c635d23040e2a222aa4fc0401bb28`.
The local wrapper takes its intrinsic constructors as arguments and returns
the Temporal namespace and Date conversion method. It omits the global and
Intl overrides and the optional development-only instance string.

Each public realm receives independent classes, prototypes and internal slot
maps. First use initializes the implementation in a private realm on the same
VM and heap. Exported constructors, prototypes, methods and errors belong to
the public realm; the algorithm’s private collections and intrinsic methods
remain independent of changes to page globals. Each calling task retains
its ordinary instruction, allocation and execution-depth limits.
