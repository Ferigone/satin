// The generated fixtures have synchronous test bodies scheduled for load.
// Record just those bodies to keep the testharness call stack out of each test.
const canvasLoadChecks = [];
function _addTest(run, options) {
    canvasLoadChecks.push({ run: run, options: options, name: t.name });
}

function async_test(name) {
    return { name: name };
}

function assert_approx_equals(actual, expected, tolerance, description) {
    if (typeof actual !== "number" || !(Math.abs(actual - expected) <= tolerance)) {
        wptFail("assert_approx_equals", description, "expected " + expected + " ± " + tolerance
            + ", got " + String(actual));
    }
}

function assert_throws_dom(expected, operation, description) {
    try {
        operation();
    } catch (error) {
        assert_true(error instanceof DOMException, description);
        const legacy = DOMException[expected];
        if (typeof legacy === "number") {
            assert_equals(error.code, legacy, description);
        } else {
            assert_equals(error.name, expected, description);
        }
        return;
    }
    wptFail("assert_throws_dom", description, "callback returned normally");
}
