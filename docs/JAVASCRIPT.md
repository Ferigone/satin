# JavaScript language foundation in alpha.9

`crates/javascript` is an independent, dependency-free Rust implementation. It does not embed V8, Chromium, WebView, Boa, QuickJS, or another JavaScript runtime. The lexer, parser, compiler, typed stack bytecode, interpreter, object model, lexical environments, native bindings and tracing collector are project code. The implemented language surface and the remaining gaps are described below.

## Execution architecture

The lexer produces located tokens. Recursive-descent/Pratt parsing produces an AST; unsupported grammar productions are rejected rather than rewritten into source-specific behavior. The compiler performs declaration planning and uses an explicit traversal stack to emit expression bytecode, property references, calls, assignments and conditional jumps. Flat expression trees do not consume recursive compiler frames. Structured bytecode instructions contain compiled chunks for blocks, branches, loops, switch and exception handling. Source text is not inspected to synthesize execution results.

Functions retain bytecode and a traced lexical-environment handle. Environments distinguish lexical declarations, function variables and global object properties. Member references preserve their receiver for method calls. Assignment references are evaluated once, including compound assignments and updates. Completion records distinguish normal evaluation, return, break, continue and thrown errors; `finally` can replace an earlier abrupt completion.

Synchronous bytecode uses bounded Rust calls for nested chunks. A body that can suspend uses owned frames and resumes through Promise jobs. Parser, native execution, and suspended frame limits keep execution bounded on a Windows thread with a 1 MiB stack. The VM has no JIT or proper tail calls.

## Implemented language subset

- `undefined`, `null`, booleans, IEEE-754 numbers, UTF-16 strings, unique Symbol primitives and object identities.
- Decimal, hexadecimal, binary and octal-prefixed number literals; quoted strings and Unicode/code-point escapes; template literals and tagged templates; line/block comments and basic semicolon insertion.
- Ordinary objects with string/Symbol keys, computed literal keys/methods, sparse arrays, string indexing by UTF-16 code unit, property reads/writes/deletion, prototypes, extensibility and data/accessor property descriptors. Array/argument spread consumes the synchronous iterator protocol; object spread copies enumerable own string/Symbol properties. Concise methods are callable but are not constructors.
- `var`, `let`, `const`, lexical blocks, temporal dead zones, function variable hoisting, closures, named function expressions, ordinary/arrow functions with default/rest/destructuring parameters, explicit receivers, `arguments` as an array-like object, calls and ordinary construction. Array/object patterns work in declarations, assignments, catch bindings and declared loop heads.
- Arithmetic, number bitwise operations, strict/abstract equality, relational comparisons, `typeof`, `void`, `delete`, updates, compound assignments, short-circuit `&&`/`||`/`??`, logical assignments, optional member/call chains, conditional and comma expressions.
- `if`, `while`, `do/while`, classic `for`, declared-binding `for/in` and `for/of`, per-iteration loop lexical environments, `switch` with fall-through, return/break/continue, throw/try/catch/finally.
- Basic strict-mode behavior, direct/indirect `eval` scope separation, and the `Function` constructor using this engine's own parser. Function parameter and body grammar goals are parsed separately.
- Ordinary classes with inheritance, public fields, static initialization blocks, and prototype/static methods and accessors.
- Async function declarations, expressions, arrows, methods, and the AsyncFunction constructor. Await resumes through the native Promise job queue.
- Source-text modules with static imports and exports, live bindings, cyclic graph linking and evaluation, namespaces, import.meta, top-level await, and an embedding hook for dynamic import.
- Native Promise records, resolving functions, chaining and a traced FIFO job queue. The engine parses and serializes JSON directly.

Available standard-library subsets include Object descriptor/prototype/key utilities; Function call/apply/bind; common Array construction, mutation, search, iteration and reduction methods; common String code-unit, search, slicing, splitting, literal replacement, case and trimming methods; Number/Boolean conversion; parseInt/parseFloat; Math; and core Error constructors. This list describes implemented paths, not complete coverage of each constructor or method.

Objects enumerate numeric index keys before other string keys, then Symbol keys in insertion order. Object.keys/values/entries/getOwnPropertyNames and for-in omit Symbols; Object.getOwnPropertySymbols and Reflect.ownKeys expose the appropriate key kinds. Object.assign, defineProperties and freeze/seal include Symbol descriptors. Deleting and reinserting a key moves it to the end of its key category. Array length changes enforce writable length and non-configurable element restrictions. Native-generated TypeError/ReferenceError/etc. inherit their intrinsic subtype prototype; caught errors therefore retain constructor identity independently of a user-controlled `name` property. Throwing an arbitrary object or Symbol does not coerce it.

## Templates, patterns and short-circuit references

Template lexing tracks substitution braces and nested templates as grammar tokens. Raw and cooked segments retain UTF-16, normalize source CR/CRLF line terminators, and preserve lone surrogates. Invalid escape sequences are early errors in untagged templates; tagged templates receive undefined cooked entries and the original raw escape text. Untagged substitutions perform ToString with the string hint before evaluating the next substitution. Tagged calls preserve their receiver, pass substitutions without coercion, and use frozen cooked/raw arrays. String.raw uses the actual raw property and indexed values.

Each compiled tagged-template site owns a distinct identity. Repeated evaluation of that site within one VM returns the same template object, including after collection; a separate site or eval compilation obtains a different object. The VM cache holds weak code-site identities and traces cached arrays while their code is alive. Cache bookkeeping and arrays consume the aggregate allowance. An unreachable code site can retain its cache entry until the next explicit collection after its code has been swept. Realms and cross-realm template caches remain unimplemented.

Binding and assignment patterns are compiled into structured pattern records with expression chunks for defaults, computed keys and assignment references. Array patterns use GetIterator/IteratorStepValue/IteratorClose, including elisions that do not read value, normal early completion that closes an unfinished iterator, and the precedence of an original throwing completion. Rest arrays consume the remaining iterator under work, length and allocation guards. Object patterns reject null/undefined before evaluating keys, retain excluded PropertyKeys, and create rest data properties using the same CopyDataProperties mechanism as object spread. That mechanism observes own-key ordering, enumerability, getters and Symbol identity; it does not invoke a target setter. Assignment patterns return their original right-hand value and evaluate each member target once in the specified order before getting its source value.

Parameters are declared before initialization, so later parameters retain their temporal dead zone. Defaults run only for undefined, receive inferred anonymous function names, and cannot see body var/function declarations. Parameter expressions use a separate environment for body variables; rest/destructuring lists without expressions preserve shared parameter/var bindings. Defaults and computed parameter keys cannot use sloppy direct eval to redeclare a parameter. Function length stops at the first default or rest parameter, and non-simple lists reject duplicate names and a use-strict directive. The arguments object remains an unmapped array-like approximation.

Optional chains retain public and private references across contiguous member and call links. Nullish short-circuiting skips computed keys and arguments. Grouping ends that chain, method calls retain their receiver, and optional eval calls are indirect. Logical assignments get their reference once, read once, and evaluate the right side and write only when required. A skipped assignment avoids writes to const bindings and calls to setters.

## Symbols and synchronous iterators

Symbol() creates an opaque identity with an optional UTF-16 description. Symbol.for/keyFor use a registry owned by this VM; multiple realms/agents and a shared cross-realm registry are not implemented. Symbols are primitive values, distinct from boxed Symbol objects. Implicit string/numeric conversion throws; the String function explicitly produces a descriptive string for primitive Symbols. ToPropertyKey preserves Symbol identity, including a Symbol returned by an object's Symbol.toPrimitive method. Coercion passes the string/number/default hint, and instanceof consults Symbol.hasInstance.

Well-known Symbol identities include iterator, asyncIterator, species, toPrimitive, toStringTag, hasInstance, isConcatSpreadable, unscopables, match/matchAll/replace/search/split and dispose/asyncDispose. An exposed identity does not imply its consuming language/API feature is implemented. Current consumers include synchronous iteration, Promise species, Array map/filter/slice/concat species, concat spreadability, coercion, instanceof and object tags. Reflect provides ownKeys, getOwnPropertyDescriptor, get, set, has, deleteProperty, defineProperty, preventExtensions, apply, and construct. Proxy remains absent.

Array values/keys/entries use branded traced iterator records with a live target and increasing index. String iteration yields Unicode code points while preserving isolated surrogate code units. Native iterators inherit a shared self-iterable prototype and retain their targets until exhaustion. Array.from supports iterable and array-like inputs, mapping, constructor calls and iterator closing on mapping/property-definition failures.

GetIterator obtains the actual Symbol.iterator method and caches next. IteratorStepValue checks result objects and reads done/value in order. IteratorClose gets and invokes return, with the original throwing completion taking precedence over ordinary close errors. A `for...of` closes its iterator on break, return, body exceptions and a continue to an outer loop. A continue to the same loop preserves its iterator. Resource controls abort without invoking untrusted cleanup. Loop heads use a declared binding, including array and object patterns with rest.

Labels resolve within the current function or script. `break` exits the named statement, and `continue` resumes the named iteration. A jump crossing iterators closes them in order. Pending exits preserve statement completion values and run intervening `finally` blocks, which can replace the exit. [ECMAScript labelled evaluation](https://tc39.es/ecma262/multipage/ecmascript-language-statements-and-declarations.html#sec-runtime-semantics-labelledevaluation)

## Classes and construction

Class declarations use lexical bindings with temporal dead zones. Class expressions have their own name environment. Instance and static methods have home objects, and super property references use the current home-object prototype and the actual receiver. Derived constructors initialize their receiver through super calls. Before initialization, reading this produces a ReferenceError. Super calls forward the original new.target. A derived constructor can return an object explicitly. Public fields initialize in declaration order on the correct receiver and use own data properties, and static field initializers and blocks run as part of class definition. Class calls without new throw, and concise methods cannot be used as constructors.

Instance and static private fields, methods and accessors use opaque names allocated on each class evaluation. The class body, closures and direct eval resolve those names lexically, including forward declarations. Heritage expressions use the enclosing private scope. The parser rejects undeclared names, disallowed duplicates, `#constructor`, `super.#name` and deletion of private references. Public and private identifier names share Unicode identifier tables and decode identifier escapes. The parser distinguishes an escaped name from a grammatical keyword. [Private names in class bodies](https://tc39.es/ecma262/multipage/ecmascript-language-functions-and-classes.html#sec-static-semantics-allprivateidentifiersvalid)

Private storage belongs to the actual receiver object. Private methods and accessors install before fields or static initializers run. Field initialization adds each slot after its initializer succeeds. `#name in object` checks for that slot, and private reads and writes validate the receiver before getting a value or invoking an accessor. Reflection enumerates ordinary properties, while ordinary freeze and seal operations leave private slots writable. A derived constructor can stamp private slots onto an object returned by its superclass. Repeating the same stamp raises a TypeError. [Class definition evaluation](https://tc39.es/ecma262/multipage/ecmascript-language-functions-and-classes.html#sec-runtime-semantics-classdefinitionevaluation) · [PrivateGet and PrivateSet](https://tc39.es/ecma262/multipage/abstract-operations.html#sec-privateget) · [Private brand checks](https://tc39.es/ecma262/multipage/ecmascript-language-expressions.html#sec-relational-operators)

The browser configures its window global and Location receivers to reject private elements through the HTML host hook. Ordinary receivers accept private elements even after they become nonextensible. [HTML private-element host hook](https://html.spec.whatwg.org/multipage/webappapis.html#hostensurecanaddprivateelement)

Native constructors including Array, Function, Object, Number, Boolean, String, Error, AggregateError, and Promise receive the actual newTarget and select the instance prototype before allocation. Plain calls and construction have separate entry points. Strict or unmapped arguments objects expose a throwing callee accessor. Full mapped arguments remain unsupported. [Super expressions](https://tc39.es/ecma262/multipage/ecmascript-language-expressions.html#sec-super-keyword)

## Async functions and await

Statements before the first await run during the call. An async call returns an intrinsic Promise and rejects it on parameter binding or body errors. Await uses intrinsic PromiseResolve and queues resumption through the same FIFO as Promise reactions. The VM retains the operands, references, lexical environments, and pending control flow in traced frames until the awaited result arrives. Branches, loops, synchronous iterator bodies, switch, catch, and finally preserve their state across suspension. A return from finally can override an earlier pending return or throw.

Async methods retain their receiver and home object. Async functions have %AsyncFunction.prototype%, expose the AsyncFunction constructor through that prototype, and reject construction of an async function instance. The constructor compiles its parameter list and body through the VM's own parser. Await inside destructuring subexpressions, class heritage, and computed class element names remains unsupported. [Async function evaluation and await](https://tc39.es/ecma262/multipage/control-abstraction-objects.html#sec-async-functions-abstract-operations)

## Generators and async iteration

Generator declarations, expressions, methods, and the `GeneratorFunction` constructor create resumable functions. Parameter initialization runs at the call. The first `next()` starts the body and ignores the sent argument. A suspended generator retains traced bytecode frames, environments, operands, active iterators, and pending completions. Later `next(value)`, `return(value)`, and `throw(reason)` resume at the suspended expression. Catch, finally, and iterator closing follow the resulting completion, including a finally block that yields before completing. Each generator and generator function has its intrinsic prototype chain and brand checks. [Generator definitions](https://tc39.es/ecma262/multipage/ecmascript-language-functions-and-classes.html#sec-generator-function-definitions)

`yield*` caches the delegated iterator's `next` method and resolves `return` and `throw` at resumption. Synchronous delegation can return the exact delegated iterator result. Asynchronous generators return promises and queue requests in FIFO order. They await yielded and returned values and asynchronously advance delegated iterators. Pending reactions and requests are traced through the same job queue used for async functions. [Generator abstract operations](https://tc39.es/ecma262/multipage/control-abstraction-objects.html#sec-generator-abstract-operations)

`for await...of` works in async function bodies, async generators, and source-text modules. It prefers `Symbol.asyncIterator` and adapts a synchronous iterator when needed. The adapter awaits sync iterator values. An abrupt loop exit awaits iterator closing and preserves an original thrown reason if closing also fails. Loop heads support a `var`, `let`, or `const` binding and destructuring. Assignment heads and `Symbol.asyncDispose` remain unsupported. The shared task budgets apply to resumed code, awaited reactions, and buffered async generator requests. [Async-from-Sync Iterator Objects](https://tc39.es/ecma262/multipage/control-abstraction-objects.html#sec-async-from-sync-iterator-objects)

## Source-text modules

Modules are parsed as their own strict grammar goal. Static import requests retain their UTF-16 specifiers in first-occurrence order. Imports support side effects, named/default bindings, and namespace bindings. Exports support local declarations, default expressions/functions/classes, named reexports, and star and namespace reexports. A linked module uses its own environment and imports resolve to live aliases into other module environments. Hoisted functions are available within cycles. Reads of uninitialized exported lexical bindings raise ReferenceError. Linking resolves missing and ambiguous imports before executing module bodies.

Module namespaces are stable null-prototype objects with sorted UTF-16 export names and Symbol.toStringTag. Export descriptors reflect the current binding value and report enumerable, writable, and nonconfigurable. Namespace assignment and deletion of exported names are rejected. import.meta is a stable null-prototype object initialized with the host-supplied identifier as its url property.

An embedding first calls `parse_module(source, identifier)`, then reads `module_requests` and resolves/fetches the graph. `ModuleId` is an opaque handle belonging to that VM. Use `root_module` and `unroot` while a host retains handles across collections. `link_module` takes a Rust resolver that maps a referrer ModuleId and a borrowed JsString specifier to the target ModuleId. `module_identifier`, `module_status`, `module_namespace`, and `module_import_meta` expose the state and accessible objects. `check_module_syntax` compiles the Module goal without executing it.

`evaluate_module` executes a linked graph in dependency order, tracks strongly connected components, and returns a cached intrinsic graph Promise. Top-level await suspends the active module, and modules wait for their asynchronous dependencies before running. Independent branches can still start. Ready importers resume in module evaluation order. Errors propagate to the affected importers.

While a graph is suspended, `module_status` reports Evaluating and `module_evaluation_result` returns None. After the graph finishes, `module_evaluation_result` yields the settled result. The API handles the graph rejection when it returns an error. An embedding that reports module failures itself can call `observe_module_evaluation` after starting evaluation to mark the graph Promise handled before it settles. `module_evaluation_error` reads the result without handling the rejection. Task-wide instruction and heap budgets apply, with a graph limit of 1,024 records. [Source Text Module Records](https://tc39.es/ecma262/multipage/ecmascript-language-scripts-and-modules.html#sec-source-text-module-records)

Dynamic import evaluates and converts its specifier then queues work through `Vm::set_dynamic_import_hook`. The request provides the exact UTF-16 `specifier`, a defining script or module `referrer`, and an optional `referrer_module` handle. `eval_with_identifier` records classic script identity for later calls. The host roots the request's Promise capability while it loads and links the graph. `settle_import_after_evaluation` starts or reuses evaluation and transfers settlement to a traced reaction on the graph Promise. URL resolution, Fetch, credentials, and MIME validation live in the browser. Import options and attributes, synthetic and JSON modules, and source-phase or deferred imports remain unsupported. [Dynamic import evaluation](https://tc39.es/ecma262/multipage/ecmascript-language-expressions.html#sec-evaluate-import-call)

## Regular expressions

RegExp literals and the RegExp constructor compile directly to the VM's regular-expression instructions. The parser selects the lexical goal at the slash, including inside template substitutions and arrow parameters. Each evaluation of a literal creates its own RegExp object and `lastIndex`.

Patterns support alternation, greedy and lazy quantifiers, character classes and ranges, shorthand escapes, named and numbered captures and references, lookahead and lookbehind, and scoped `i`, `m`, and `s` modifiers. The flags `d`, `g`, `i`, `m`, `s`, `u`, and `y` support capture indices, repeated and sticky matching, and observable `lastIndex` coercion and updates. Matching preserves lone surrogates and reports offsets in UTF-16 code units. The `u` flag matches supplementary code points. Case equivalence and capture identifiers use pinned Unicode 17.0.0 data under `crates/javascript/src/regexp/data`. [ECMAScript RegExp objects](https://tc39.es/ecma262/multipage/text-processing.html#sec-regexp-regular-expression-objects) [Unicode Character Database](https://www.unicode.org/ucd/)

The VM honors overridden `exec`, `Symbol.match`, `Symbol.matchAll`, `Symbol.search`, `Symbol.replace`, `Symbol.split`, and species constructors. String methods include matching, lazy `matchAll`, search, split with captures, and string or callback replacements. RegExp string iterators retain a traced matcher and the input until they finish. `RegExp.escape`, source formatting, and the browser compatibility method `compile` are available.

The matcher uses a bounded explicit backtracking stack. Compilation accepts at most 16,384 pattern code units, 256 captures, and 32 levels of nested groups. Matching, capture copying, and substitution use the shared VM work and heap budgets. Backtracking scratch space is capped at 8 MiB. Unicode property escapes and the `v` flag currently raise `UnsupportedFeature`. The Test262 scope and individual status counts are reported separately from the existing language and API scopes.

## Keyed collections

Map and Set use insertion-ordered storage with SameValueZero keys and live iteration. Map includes `groupBy`, `getOrInsert`, and `getOrInsertComputed`. Set supports union, intersection, difference, symmetric difference, and disjointness, subset, and superset checks using the set-like protocol. WeakMap and WeakSet accept objects and nonregistered symbols as weak keys. Ephemeron tracing retains a WeakMap value when its key is independently reachable and removes dead entries during collection. WeakMap also includes `getOrInsert` and `getOrInsertComputed`.

## BigInt

BigInt literals accept decimal, binary, octal, and hexadecimal digits and numeric separators between digits. The VM implements signed arithmetic, exponentiation, division truncated toward zero, remainder, two's-complement bitwise operations, signed shifts, and increment and decrement. Mixed Number/BigInt arithmetic and unsigned right shift raise TypeError. Relational operators and loose equality compare a BigInt and a Number at the Number's exact represented value. Strict equality and Map and Set keys distinguish the types. [ECMAScript BigInt operations](https://tc39.es/ecma262/multipage/ecmascript-data-types-and-values.html#sec-ecmascript-language-types-bigint-type)

`BigInt()` accepts integral Numbers as well as the strings, booleans, and BigInts accepted by ToBigInt. `Number()` explicitly converts BigInt to binary64. BigInt supports boxing, formatting in bases 2–36, and modular conversion through `asIntN` and `asUintN`. Locale formatting uses the ECMAScript decimal fallback. The VM preserves BigInt in structured clones and session-history primitives. Standalone worker results report it as a decimal string with a `bigint` type tag. [BigInt objects](https://tc39.es/ecma262/multipage/numbers-and-dates.html#sec-bigint-objects)

`JsBigInt` stores the signed and unsigned 64-bit ranges inline and shares immutable limbs for larger magnitudes. `num-bigint` supplies integer math. The VM applies ECMAScript coercion, error, and evaluation rules. Values are capped at 65,536 magnitude bits. Native arithmetic, conversions, and comparisons charge the shared work budget, and retained limbs count toward the VM's heap estimate. The lexer separately budgets BigInt literal conversion and decimal property-key formatting per source. Excessive work can fail before the maximum bit width is reached.

## Strings and object lifetime

`JsString` owns reference-counted `[u16]` storage. `JsString::from_utf16` and `as_utf16` preserve isolated high and low surrogates. Lexing, concatenation, indexing, comparisons, substring operations and property keys preserve those code units. Conversion to Rust Unicode text is explicit: `to_string_lossy` is intended for host display. String casing uses Rust's Unicode mappings and Unicode context properties for lowercase final sigma. It preserves unpaired surrogates. `toLocaleLowerCase` and `toLocaleUpperCase` use the same locale-neutral mappings; Intl locale selection and language-specific casing are not implemented. `eval`/Function source containing isolated surrogate code units is explicitly unsupported rather than silently decoded lossily.

The arena contains ordinary/native/interpreted/bound-function objects, iterators, Promise aggregation records, source-text modules, and lexical environments. Handles contain a slot index and generation. Access through a reclaimed handle returns an error. Collection traces global state, intrinsic prototypes, explicit host roots, prototypes, string/Symbol properties, accessor functions, captured environments, bound arguments, boxed values, class home objects and field code, private slots and methods, module graph edges, live aliases, namespaces, and evaluation Promises. It then sweeps unreachable cells, including cycles. Private-name environments and slots consume the aggregate allocation budget. Sweeping an unreachable native function drops its callback and captured Rust data. SymbolId uses an opaque Rc identity holding description and registry text. Symbol storage and retained descriptions are accounted and reclaimed when their final owner disappears.

Suspended frames are traced by pending Promise reactions and queued jobs, including operands and the values retained by iterator and finally state. Garbage collection is currently requested explicitly between browser tasks. It is deferred during active execution, native callbacks or a `with_task` scope, so local interpreter operands cannot be collected. This is a stop-the-world mark/sweep foundation, not an incremental, generational or concurrent collector.

## Embedding contract

```rust
use rust_browser_javascript::{Value, Vm};

let mut vm = Vm::new();
let node_prototype = vm.new_object();
let node = vm.new_object_with_prototype(node_prototype)?;
vm.set_global("node", node.clone())?;

let callback = vm.eval("(function () { this.count = (this.count || 0) + 1; })")?;
let callback_root = vm.root(callback);

vm.with_task(|vm| {
    vm.call(vm.root_value(callback_root).unwrap(), node, &[])?;
    Ok(())
})?;
vm.collect_garbage();
vm.unroot(callback_root);
# Ok::<(), rust_browser_javascript::JsError>(())
```

Important public operations:

- `Vm::new`, `with_limits`, `limits`, `check_syntax`, `eval`, `call`, and `with_task`.
- `global`, `get_global`, `set_global`, `new_object`, `new_object_with_prototype`, `new_array` and `new_native_function`.
- `get_property`, `set_property`, `define_data_property`, `define_accessor`, `is_callable`, `to_string` and `to_number`.
- `new_symbol(Option<JsString>) -> Result<Value, JsError>`, `well_known_symbol(&str) -> Option<Value>`, `to_property_key(&Value) -> Result<PropertyKey, JsError>`, and `own_property_keys(Value) -> Result<Vec<PropertyKey>, JsError>`.
- `get_property_key(Value, PropertyKey)`, `set_property_key(Value, PropertyKey, Value)`, `define_data_property_key` and `define_accessor_key`. These accept the exported `PropertyKey::String(JsString)` or `PropertyKey::Symbol(SymbolId)`. Existing string-only embedding operations keep their signatures. `Value::Symbol` is a distinct enum variant; host matches must handle it explicitly.
- `root`, `root_value`, `unroot`, `collect_garbage`, `heap_cell_count` and `estimated_heap_bytes`.
- `iterator_from_method`, `iterator_step` and `iterator_close` implement synchronous Web IDL sequence conversion. An opaque `IteratorRecord` is valid within an active VM task.

Native callbacks have the signature `Fn(&mut Vm, Value, &[Value]) -> Result<Value, JsError> + 'static`. The second argument is the explicit receiver. The VM is owned by one thread and callbacks need not be Send/Sync. A callback may capture DOM IDs and shared Rust state. A JavaScript value retained by host code **must** have an explicit root; capturing an unrooted `Value` in an opaque Rust closure does not make it traceable. Store a RootId and release it when the listener/wrapper is removed. Avoid ownership cycles between the VM, host state and native callbacks. Release RefCell/lock guards before calling back into JavaScript.

`Value` is Clone, with From conversions for Rust strings, `JsString`, booleans and common numeric types. Errors expose kind, message, optional location and optional thrown value. `ErrorKind::UnsupportedFeature` is distinguishable from `SyntaxError`; hosts and conformance tools must preserve this distinction. A user object whose `name` says TypeError is still an arbitrary thrown value.

`new_native_constructor` retains its boolean construction flag. `new_native_constructor_with_new_target` supplies `None` for calls and `Some(new_target)` for construction. Both constructor helpers pass an undefined receiver to construction and require an object result. Rust hosts can construct through `construct` and `construct_with_new_target`. `set_host_data`/`host_data` attach opaque typed Rc Rust metadata which is invisible to JavaScript and dropped with its object. JavaScript wrapper caches belong in `set_host_slot`/`host_slot`: up to 32 invisible values per object, traced and accounted by the VM. `is_array`, `enumerable_own_keys`, public `ensure_string_allocation` and `charge_work` let hosts use intrinsics and resource guards independently of mutable globals.

`set_host_indexed_properties(wrapper, Rc<dyn HostIndexedProperties>)` attaches a read-only live indexed collection to an extensible ordinary object. Canonical indices below the provider's current length participate in reads, presence tests, descriptors, own-key enumeration, and property copying. Presence and key-only queries avoid wrapping values. Supported indices report enumerable, configurable, and nonwritable descriptors. Define operations on canonical indices, deletion of supported indices, and preventExtensions/seal/freeze are rejected. The heap releases the provider with the wrapper. Providers own their metadata budgets and can charge work when materializing a value. [Web IDL legacy platform objects](https://webidl.spec.whatwg.org/#legacy-platform-object)

## Promise jobs and JSON

Promise construction executes its executor synchronously. Resolve/reject functions share a one-shot flag, perform synchronous then-property lookup, defer callable thenable assimilation to a job, adopt other promises, and reject self-resolution/chaining cycles. `then`, `catch` and `finally` propagate values/errors and finally waits for a returned promise. Promise.resolve/reject/withResolvers and Promise.try support ordinary custom capability constructors; try immediately calls its callback with undefined receiver/forwarded arguments and settles through its capability. then/finally observe constructor and Symbol.species; the intrinsic species getter returns its receiver. Promise subclasses receive the original newTarget, and chains observe their species constructor. Unhandled-rejection DOM events and proposed keyed combinators remain absent.

Promise.all/race/allSettled/any obtain the constructor's resolve method once and consume the actual synchronous iterator protocol. They support custom capability constructors and thenables, preserve input ordering, close unfinished iterators when constructor resolve/then processing throws, and use shared one-shot flags for each element. Aggregate state, capabilities, handlers and values are explicit traced heap fields; callbacks do not capture hidden JavaScript references. Empty all/allSettled fulfill with a fresh array, empty race remains pending and empty any rejects with an authentic AggregateError. Any's errors preserve input order. AggregateError itself consumes an iterable, observes message/cause before iteration, inherits Error and defines a non-enumerable errors array. There are no array-only combinator shortcuts.

`new_promise_capability` returns `PromiseCapability { promise, resolve, reject }`. A host retains explicit roots until completion and settles through `Vm::call`. `enqueue_microtask`, `enqueue_call` and `enqueue_json_parse` share one FIFO with promise reactions and thenable jobs. The latter queues intrinsic JSON parsing and settles/rejects through traced resolving functions. `drain_jobs(max_jobs)` returns the number consumed; an ordinary throwing microtask is consumed and returns an error, leaving subsequent jobs queued. A promise handler throw instead rejects its chained promise. `with_task` shares one work allowance across a checkpoint; `clear_jobs` releases queued jobs after fatal limits. Eval/call do not secretly run an event loop.

`take_unhandled_rejections` reports still-unhandled reasons in rejection order after a host checkpoint. Records are bounded by max_pending_jobs and traced until taken; attaching then before the checkpoint removes the original promise's record. No user code runs while reporting. `has_prototype` compares actual prototype identities without getters or Symbol hooks, allowing the conformance host to distinguish a captured Test262Error prototype from a forged error name. Unhandled reasons are host diagnostics rather than automatically thrown synchronous errors: intentional rejected promises in language tests are not blanket failures.

JSON.parse uses an independent UTF-16 scanner and strict JSON grammar. It supports duplicate-key replacement, ordinary `__proto__` data properties, postorder revivers and primitive reviver source contexts. JSON.stringify supports toJSON, callable/array replacers, indentation, boxed primitives, own-key order, undefined/function/Symbol omission, array null substitution, cycle detection and well-formed quoting of unpaired surrogates. Symbol keys are omitted, while a boxed Symbol is an ordinary object for serialization. Native `parse_json(&JsString)` and `stringify_json(Value)` bypass mutable global JSON; the latter returns None for an undefined serialization result. Raw JSON wrapper APIs remain absent; number formatting inherits the VM's documented gaps. Parsing, reviver traversal and serialization share a 32-level nesting/reentrancy limit, with character-work and allocation guards before growth.

`JSON.stringify` checks `toJSON` even on BigInt primitives, then runs the replacer and unboxes wrapper objects. A BigInt that remains at that point raises TypeError. [JSON serialization](https://tc39.es/ecma262/multipage/structured-data.html#sec-serializejsonproperty)

## Binary data and Fetch

ArrayBuffer owns native byte storage. TypedArray and DataView instances trace
the backing buffer and expose checked views. This includes `BigInt64Array`,
`BigUint64Array`, and the signed and unsigned BigInt64 DataView methods. BigInt
writes use ToBigInt and reduce to the low 64 bits. The VM accounts for the backing
once, including when multiple views share it. Web IDL consumers use the visible
byte range for snapshots, and TextEncoder can fill a newly allocated Uint8Array
directly.

Headers, Request, Response and readable streams store their JavaScript
references in traced host slots. Fetch retains capability roots until response
headers arrive and the stream keeps in-flight read promises alive until they
settle. The 16 MiB Fetch budget counts shared body backing once. Streams manage
their separate queue and collection budgets. [Fetch host](../crates/engine/src/script/fetch.rs) ·
[Readable streams](../crates/engine/src/script/streams.rs) · [Transport](NAVIGATION.md)

This host is a buffered subset: string bodies, HTTP ByteString headers, request mode/credentials/redirect options, header guards, clone/bodyUsed and queued UTF-8 text/JSON reads. Streams, binary bodies, Blob, FormData, AbortSignal, cache/referrer/integrity/keepalive controls, header iterators and static Response.redirect/json are explicitly unsupported. Request-to-request body transfer is consumed synchronously by the buffered model; stream lock/disturbance timing and backpressure require future stream support. Cookie/authentication and CORS/redirect policy belong to the browser transport modules rather than bypassing origin checks in native callbacks.

## Resource limits

Default `VmLimits`:

| Field | Default |
| --- | ---: |
| max_instructions | 1,000,000 per outer task/eval/call |
| max_call_depth | Up to 64 on Windows with available native stack, up to 24 on other systems |
| max_heap_objects | 100,000 objects/environments |
| max_heap_bytes | 64 MiB conservative aggregate allowance |
| max_source_bytes | 2 MiB, further constrained by the aggregate parser allowance |
| max_string_units | 8 Mi UTF-16 code units per string |
| max_array_length | 1,000,000 |
| max_pending_jobs | 4,096 jobs, reactions per pending promise, or unhandled rejection records |

Synchronous bytecode execution permits 16 native depth units by default.
On Windows the VM allows up to 64 when the current thread has at least
256 KiB of headroom at a deeper entry. The same check applies above
24 nested calls. The browser and conformance worker reserve a 4 MiB virtual
stack in their Windows MSVC executables. Additional pages commit as depth
grows. A regression test exercises deeper execution on a 4 MiB thread and
failure recovery on both 1 MiB and 4 MiB threads.
[Win32 stack limits](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getcurrentthreadstacklimits) ·
[MSVC stack reservation](https://learn.microsoft.com/en-us/cpp/build/reference/stack-stack-allocations?view=msvc-170)

Suspended execution can retain 64 owned frames. Destructuring initialization
is capped at 16 pattern levels, AST depth at 64 and recursive grammar parsing
at 24. Function environment setup and structured control dispatch use
separate native frames to bound their temporary storage. Explicit 1 MiB tests
also cover tagged recursion, parameter defaults, nested patterns, recursive
class construction, async recursion and asynchronous module chains.

The aggregate byte limit is an **allocation estimate**, not a measurement of process RSS or a general allocator quota. It includes distinct UTF-16 storage with pointer bookkeeping, object/environment cells, property/binding/container estimates, native temporary reservations and retained bytecode. Source parsing/compilation is conservatively precharged at 512 bytes per UTF-8 source byte plus overhead; a default 64 MiB VM therefore accepts substantially less than the standalone 2 MiB source ceiling. This avoids allocating an unbounded AST before finding the final bytecode size. Collection recomputes estimates from traced live containers, strings, roots and shared bytecode. Allocation-since-collection accounting can reject a task even when many intermediate strings have already become unreachable; collection between tasks restores capacity.

Large native array/key/apply/Function builders check aggregate allowances before their large allocations. Replacement expansion checks each append. String search comparisons and large string/key comparisons consume work budget. Resource-limit errors are uncatchable host controls. The browser must stop the event/microtask task on a fatal limit and collect after leaving its execution scope. `with_task` lets event listeners, timers and microtask checkpoints share one instruction allowance instead of resetting it per callback.

Allocating string operations also charge proportional copy work independently of the allocation tracker: borrowed-slice copies, output-buffer writes and the final Vec-to-Rc copy all consume fuel. This covers slice/substring/substr, repeat/concat, trim/case/padding/replacement, joins, binary concatenation, JSON buffers/source contexts and dynamic-source/formatting conversions. Reusing an allocator address cannot make repeated temporary copies free. Cloning an existing Rc string without copying its code units keeps its constant-cost path; memory preflight checks alone do not repeatedly charge an already-built prefix.

The host owns trusted Rust allocations and the sizes of native captures, DOM storage and input/output buffers. The infallible host allocation helpers do not constitute a general-purpose Rust allocator sandbox. They are intended for bounded embedding data. Process isolation, an allocator-backed exact heap quota and interruptible native operations remain further work.

## WebAssembly

The VM exposes `WebAssembly.Module`, `Instance`, `validate()`, `compile()`, and both `instantiate()` overloads for a bounded integer subset. It copies the visible bytes of a BufferSource, validates the binary sections and operand stack, and links callable JavaScript imports. Exported functions accept i32 arguments and return at most one i32 value. The core executes straight-line locals, constants, add, subtract, multiply, drop, and calls to imported functions.

`compile()` and `instantiate()` return promises with branded compilation and link errors. Byte-source instantiation links through the VM job queue. A compiled module can be reused across instances in the same VM. Exported functions keep their module and imports alive through garbage collection. The core caps module input at 2 MiB and charges decoding and execution to the VM's work and memory budgets. [WebAssembly JavaScript Interface](https://www.w3.org/TR/wasm-js-api-2/) · [WebAssembly Core Specification](https://www.w3.org/TR/wasm-core-2/)

Linear memory, tables, control flow, calls to defined functions, other numeric types, streaming instantiation, and cross-realm module transfer remain open.

## Explicit gaps and validation

Import attributes and assignment heads in for-in, for-of, and for-await-of remain unsupported. Await and yield within destructuring subexpressions or class computed keys and heritage expressions remain unsupported. Proxy, iterator helpers, Intl, WeakRef/finalization, and SharedArrayBuffer/Atomics remain absent. Date calendar arithmetic and local timezone integration are covered separately. [Date](DATE.md)

Additional compatibility gaps include complete Annex B and lexical grammar, mapped non-strict arguments, complete early-error and directive handling, exact source preservation for Function.prototype.toString, Unicode identifiers and case handling, numeric formatting and rounding, cross-realm behavior, consumers of exposed well-known Symbols, and standard-library methods. Unsupported productions surface as UnsupportedFeature, so entering unsupported syntax cannot satisfy a negative test expecting SyntaxError.

The crate has semantic, embedding, garbage-collection, small-budget, and explicit 1 MiB stack regression tests. The separate [Test262 runner](../tools/test262/README.md) uses the Rust VM to execute pinned upstream test and harness blobs. It checks module parsing, linking, and execution as separate phases and uses canonical relative fixtures for static and dynamic imports. Reports retain unsupported features, test failures, resource limits, unfinished asynchronous tests, crashes, and timeouts.

Primary references include the [ECMAScript language specification](https://tc39.es/ecma262/), its [abstract operations](https://tc39.es/ecma262/multipage/abstract-operations.html), [expressions](https://tc39.es/ecma262/multipage/ecmascript-language-expressions.html), [iterator closing](https://tc39.es/ecma262/multipage/abstract-operations.html#sec-iteratorclose), [Promise aggregation](https://tc39.es/ecma262/multipage/control-abstraction-objects.html#sec-performpromiseall), and [function declaration instantiation](https://tc39.es/ecma262/multipage/ordinary-and-exotic-objects-behaviours.html#sec-functiondeclarationinstantiation). Chrome supplies separate behavioral comparisons.
