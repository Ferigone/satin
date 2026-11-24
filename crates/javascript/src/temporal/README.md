# Temporal standard-library implementation

Satin runs the calendar and duration algorithms from Adam Shaw’s
[`temporal-polyfill` 1.0.8](https://github.com/fullcalendar/temporal-polyfill)
in its JavaScript interpreter. The source distribution is MIT-licensed.
[License](LICENSE) · [Pinned npm archive](https://registry.npmjs.org/temporal-polyfill/-/temporal-polyfill-1.0.8.tgz)

The upstream input is `package/full/global.js`, SHA-256
`95a7fde114214858e9fc6379f3c2f8d9d36c635d23040e2a222aa4fc0401bb28`.
The local wrapper accepts intrinsic constructors and returns the Temporal
namespace and Date conversion method. It omits the upstream global and Intl
overrides and the optional debug string. ISO durations format their exact
integer components directly. ICU4X supplies Chinese, Dangi and Umm al-Qura
calendar fields and civil calendar arithmetic. Outside ICU’s civil range, the
Chinese and Korean calendars extend its mean solar-term and synodic-month
model. Property bag conversion, fixed-offset arithmetic, duration validation
and exact rational totals run in native code against the same VM budget.
Jiff reads time zone offsets and transitions from TZDB.

Each public realm receives its own constructors and prototypes. First use
initializes the algorithms in a private realm on the same VM and heap.
Exported functions and errors use the public realm’s prototypes. Each Temporal
instance carries a native brand and garbage-collected internal slots. Methods
and Intl.DateTimeFormat recognize those slots across realms. The internal
calendar algorithms use pristine intrinsics even if page scripts replace
public methods. Instructions, allocation and call depth count against the
calling task’s usual budget.
