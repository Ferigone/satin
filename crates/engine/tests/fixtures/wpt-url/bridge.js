// Synchronous assertions used by the URL fixtures. This bridge installs no browser APIs.
const wptResults = [];

function test(run, name) {
    try {
        run();
        wptResults.push({ name: name, status: "PASS" });
    } catch (error) {
        const message = error !== null && typeof error === "object"
            ? error.name + ": " + error.message
            : String(error);
        wptResults.push({ name: name, status: "FAIL", message: message });
    }
}

function wptFail(assertion, description, message) {
    throw new Error(assertion + (description === undefined ? "" : " (" + description + ")") + ": " + message);
}

function assert_equals(actual, expected, description) {
    if (!Object.is(actual, expected)) {
        wptFail("assert_equals", description, "expected " + String(expected) + " (" + typeof expected
            + "), got " + String(actual) + " (" + typeof actual + ")");
    }
}

function assert_not_equals(actual, expected, description) {
    if (Object.is(actual, expected)) {
        wptFail("assert_not_equals", description, "values were equal");
    }
}

function assert_true(actual, description) {
    if (actual !== true) wptFail("assert_true", description, "got " + String(actual));
}

function assert_false(actual, description) {
    if (actual !== false) wptFail("assert_false", description, "got " + String(actual));
}

function assert_array_equals(actual, expected, description) {
    if (typeof actual !== "object" || actual === null || !("length" in actual)) {
        wptFail("assert_array_equals", description, "missing array-like value");
    }
    if (actual.length !== expected.length) {
        wptFail("assert_array_equals", description, "expected length " + expected.length + ", got " + actual.length);
    }
    for (let index = 0; index < actual.length; index++) {
        if (actual.hasOwnProperty(index) !== expected.hasOwnProperty(index)
            || !Object.is(actual[index], expected[index])) {
            wptFail("assert_array_equals", description, "index " + index
                + " expected " + String(expected[index]) + ", got " + String(actual[index]));
        }
    }
}

function assert_throws_js(constructor, run, description) {
    try {
        run();
    } catch (error) {
        let base = constructor;
        while (base && base !== Error) base = Object.getPrototypeOf(base);
        if (typeof constructor !== "function" || base !== Error) {
            wptFail("assert_throws_js", description, "invalid expected Error constructor");
        }
        if (error === null || typeof error !== "object"
            || error.constructor !== constructor || error.name !== constructor.name) {
            wptFail("assert_throws_js", description, "expected " + constructor.name + ", got " + String(error));
        }
        return;
    }
    wptFail("assert_throws_js", description, "callback returned normally");
}

function assert_unreached(description) {
    wptFail("assert_unreached", description, "unreachable code was executed");
}
