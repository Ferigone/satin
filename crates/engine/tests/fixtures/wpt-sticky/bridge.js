// Register the synchronous upstream test bodies and run each in its own VM task.
const stickyWptTests = [];

function test(run, name) {
    stickyWptTests.push({ run, name });
}

function assert_equals(actual, expected, description) {
    if (!Object.is(actual, expected)) {
        const context = description === undefined ? "" : description + " ";
        throw new Error(context + "expected " + String(expected) + ", got " + String(actual));
    }
}
