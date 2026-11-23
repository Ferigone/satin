// The subset of testharness.js used by the pinned Streams tests in this verifier.
(function () {
  const cases = [];
  const tests = [];
  class OptionalFeatureUnsupportedError extends Error {}
  self.OptionalFeatureUnsupportedError = OptionalFeatureUnsupportedError;

  // The iterator specimens sort dense arrays of property names in assertions.
  if (typeof Array.prototype.sort !== 'function') {
    Object.defineProperty(Array.prototype, 'sort', {
      configurable: true,
      writable: true,
      value: function sort() {
        for (let index = 1; index < this.length; index++) {
          const next = this[index];
          let before = index;
          while (before > 0 && String(next) < String(this[before - 1])) {
            this[before] = this[before - 1];
            before--;
          }
          this[before] = next;
        }
        return this;
      }
    });
  }

  function expect(condition, description) {
    if (!condition) throw new Error(description || 'Assertion failed');
  }

  function register(kind, body, name) {
    const record = { name: name || '(unnamed)', status: 'pending' };
    cases.push(record);
    tests.push({ kind, body, record });
  }

  function schedule(index, receipt) {
    setTimeout(() => {
      if (index > 0) receipt.textContent = JSON.stringify(cases);
      if (index >= tests.length) {
        receipt.setAttribute('data-complete', 'yes');
        return;
      }
      const { kind, body, record } = tests[index];
      const cleanups = [];
      function finish(status, error) {
        if (record.status !== 'pending') return;
        record.status = status;
        if (error !== undefined) record.detail = String(error);
        clearTimeout(timer);
        while (cleanups.length) {
          try { cleanups.pop()(); } catch (problem) {
            record.status = 'fail';
            record.detail = 'Cleanup: ' + String(problem);
          }
        }
        schedule(index + 1, receipt);
      }
      function finishError(error) {
        finish(error instanceof OptionalFeatureUnsupportedError ? 'precondition' : 'fail', error);
      }
      const context = {
        done() { finish('pass'); },
        add_cleanup(cleanup) { cleanups.push(cleanup); },
        step(callback, ...args) {
          if (record.status !== 'pending') return;
          try { return callback(...args); } catch (error) { finishError(error); }
        },
        step_func(callback) {
          return function (...args) {
            if (record.status !== 'pending') return;
            try { return callback.apply(this, args); } catch (error) { finishError(error); }
          };
        },
        step_func_done(callback) {
          return context.step_func(function (...args) {
            if (callback) callback.apply(this, args);
            finish('pass');
          });
        },
        step_timeout(callback, delay, ...args) {
          return setTimeout(context.step_func(callback), delay, ...args);
        },
        unreached_func(reason) {
          return function () { finish('fail', reason || 'Unexpected invocation'); };
        }
      };
      const timer = setTimeout(() => finish('timeout', 'Test exceeded one second'), 1000);
      // Invoke the specimen directly from a Promise job. This leaves the full
      // JavaScript call budget available to the specimen.
      Promise.resolve(context).then(body).then(() => {
        if (kind !== 'async') finish('pass');
      }, finishError);
    }, 0);
  }

  self.test = (body, name) => register('sync', body, name);
  self.promise_test = (body, name) => register('promise', body, name);
  self.async_test = (body, name) => register('async', body, name);
  self.step_timeout = (callback, delay, ...args) => setTimeout(callback, delay, ...args);
  self.assert_equals = (actual, expected, description) => {
    if (!Object.is(actual, expected)) {
      throw new Error((description || 'assert_equals') + ' (expected ' + String(expected) +
        ', got ' + String(actual) + ')');
    }
  };
  self.assert_not_equals = (actual, expected, description) => expect(!Object.is(actual, expected),
    description || 'assert_not_equals');
  self.assert_true = (value, description) => assert_equals(value, true, description || 'assert_true');
  self.assert_false = (value, description) => assert_equals(value, false, description || 'assert_false');
  self.assert_array_equals = (actual, expected, description) => {
    const message = description || 'assert_array_equals';
    assert_equals(actual.length, expected.length, message + ' (length)');
    for (let index = 0; index < expected.length; index++) {
      assert_equals(index in actual, index in expected, message + ' (presence at ' + index + ')');
      assert_equals(actual[index], expected[index], message + ' (index ' + index + ')');
    }
  };
  self.assert_object_equals = (actual, expected, description) => {
    const message = description || 'assert_object_equals';
    if (actual === null || typeof actual !== 'object') throw new Error(message + ' (expected an object)');
    const keys = Object.keys(actual);
    const expectedKeys = Object.keys(expected);
    assert_equals(keys.length, expectedKeys.length, message + ' (property count)');
    for (const key of expectedKeys) {
      if (!keys.includes(key) || !Object.is(actual[key], expected[key])) {
        throw new Error(message + ' (property ' + key + ')');
      }
    }
  };
  self.assert_unreached = description => { throw new Error(description || 'Unexpected invocation'); };
  self.assert_throws_js = (type, callback, description) => {
    try { callback(); } catch (error) {
      expect(error != null && error.constructor === type, description || 'Unexpected exception type');
      return;
    }
    assert_unreached(description || 'Expected an exception');
  };
  self.assert_throws_exactly = (reason, callback, description) => {
    try { callback(); } catch (error) {
      assert_equals(error, reason, description);
      return;
    }
    assert_unreached(description || 'Expected an exception');
  };
  function rejects(promise, check, description) {
    return promise.then(() => assert_unreached(description || 'Expected a rejection'), check);
  }
  self.promise_rejects_exactly = (context, reason, promise, description) =>
    rejects(promise, error => assert_equals(error, reason, description), description);
  self.promise_rejects_js = (context, type, promise, description) =>
    rejects(promise, error => expect(error != null && error.constructor === type,
      description || 'Unexpected rejection type'), description);
  self.promise_rejects_dom = (context, name, promise, description) =>
    rejects(promise, error => {
      expect(error instanceof DOMException, description || 'Expected a DOMException');
      assert_equals(error.name, name, description || 'Unexpected DOMException name');
    }, description);

  self.runStreamsTests = function () {
    schedule(0, document.getElementById('wpt-receipt'));
  };
})();
