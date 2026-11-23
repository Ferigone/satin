// Provide the testharness operations used by these fixtures. Their scripts
// call the engine's real timers, animation frames, and IntersectionObserver.
const wptIntersectionTests = [];

function wptStep(item, callback, receiver, args) {
    if (item.status !== 'RUNNING') return;
    try {
        return callback.apply(receiver, args);
    } catch (error) {
        item.message = String(error);
        item.status = 'FAIL';
    }
}

function wptTest(name) {
    const item = {
        name: name || document.title || 'Untitled',
        status: 'RUNNING',
        message: null,
    };
    wptIntersectionTests.push(item);
    const t = {
        done: function() {
            if (item.status === 'RUNNING') item.status = 'PASS';
        },
        step: function(callback) {
            return wptStep(item, callback, this, Array.prototype.slice.call(arguments, 1));
        },
        step_func: function(callback) {
            return function() { return wptStep(item, callback, this, arguments); };
        },
        step_func_done: function(callback) {
            return function() {
                wptStep(item, callback, this, arguments);
                t.done();
            };
        },
        step_timeout: function(callback, delay) {
            return setTimeout(t.step_func(callback), delay);
        },
    };
    return t;
}

function test(callback, name) {
    const t = wptTest(name);
    t.step_func_done(callback)(t);
}

function async_test(callback, name) {
    if (typeof callback === 'string') return wptTest(callback);
    const t = wptTest(name);
    t.step_func(callback)(t);
    return t;
}

function promise_test(callback, name) {
    const t = wptTest(name);
    Promise.resolve().then(function() {
        return callback(t);
    }).then(t.done, t.step_func(function(error) {
        throw error;
    }));
}

function step_timeout(callback, delay) {
    return setTimeout(callback, delay);
}

function wptFail(expected, actual, description) {
    const context = description === undefined ? '' : description + ' ';
    throw new Error(context + 'expected ' + String(expected) + ', got ' + String(actual));
}

function assert_equals(actual, expected, description) {
    if (!Object.is(actual, expected)) wptFail(expected, actual, description);
}

function assert_true(actual, description) {
    assert_equals(actual, true, description);
}

function assert_false(actual, description) {
    assert_equals(actual, false, description);
}

function assert_array_equals(actual, expected, description) {
    assert_equals(actual.length, expected.length, description);
    for (let i = 0; i < expected.length; i++) {
        assert_equals(actual[i], expected[i], description);
    }
}

function assert_approx_equals(actual, expected, epsilon, description) {
    if (typeof actual !== 'number' || !(Math.abs(actual - expected) <= epsilon)) {
        wptFail(expected + ' +/- ' + epsilon, actual, description);
    }
}

function assert_throws_js(kind, callback) {
    try {
        callback();
    } catch (error) {
        if (error instanceof kind) return;
        wptFail(kind.name, error);
    }
    wptFail(kind.name, 'no exception');
}

function assert_throws_dom(code, callback) {
    try {
        callback();
    } catch (error) {
        assert_true(error instanceof DOMException);
        assert_equals(error.code, DOMException[code]);
        return;
    }
    wptFail(code, 'no exception');
}
