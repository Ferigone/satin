// Window harness for the selected original WPT specimens. One page per file.
(function () {
  const results = [];
  const observations = {};
  let loaded = false;

  function publish() {
    const receipt = document.getElementById('wpt-receipt');
    receipt.textContent = JSON.stringify({ results, observations });
    if (loaded && results.every(result => result.status !== 'pending')) {
      receipt.setAttribute('data-complete', 'yes');
    }
  }

  function register(name) {
    const record = { name: name || 'unnamed ' + (results.length + 1), status: 'pending' };
    results.push(record);
    function finish(status, error) {
      if (record.status !== 'pending') return;
      record.status = status;
      if (error !== undefined) record.detail = String(error);
      clearTimeout(timeout);
      publish();
    }
    function step(callback) {
      return function (...args) {
        if (record.status !== 'pending') return;
        try { return callback.apply(this, args); }
        catch (error) { finish('fail', error); }
      };
    }
    const timeout = setTimeout(() => finish('timeout', 'Test exceeded 1.5 seconds'), 1500);
    return {
      done() { finish('pass'); },
      step_func: step,
      step_func_done(callback) {
        return step(function (...args) {
          if (callback) callback.apply(this, args);
          finish('pass');
        });
      },
      step_timeout(callback, delay, ...args) { return setTimeout(step(callback), delay, ...args); },
      unreached_func(reason) { return step(() => assert_unreached(reason)); }
    };
  }

  function expect(condition, description) {
    if (!condition) throw new Error(description || 'Assertion failed');
  }

  self.test = (body, name) => {
    const test = register(name);
    test.step_func_done(() => body(test))();
  };
  self.async_test = (body, name) => {
    const test = register(typeof body === 'string' ? body : name);
    if (typeof body === 'function') test.step_func(body)(test);
    return test;
  };
  self.assert_equals = (actual, expected, description) =>
    expect(Object.is(actual, expected), description || 'Expected ' + String(expected) + ', got ' + String(actual));
  self.assert_not_equals = (actual, expected, description) =>
    expect(!Object.is(actual, expected), description || 'Unexpected equality');
  self.assert_true = (value, description) => assert_equals(value, true, description);
  self.assert_false = (value, description) => assert_equals(value, false, description);
  self.assert_array_equals = (actual, expected, description) => {
    assert_equals(actual.length, expected.length, description);
    for (let index = 0; index < expected.length; index++) {
      assert_equals(index in actual, index in expected, description);
      assert_equals(actual[index], expected[index], description);
    }
  };
  self.assert_unreached = description => { throw new Error(description || 'Unexpected callback'); };
  self.assert_throws_js = (constructor, callback, description) => {
    try { callback(); } catch (error) {
      expect(error !== null && error !== undefined && error.constructor === constructor, description);
      return;
    }
    assert_unreached(description || 'Expected a JavaScript exception');
  };
  self.assert_throws_dom = (name, callback, description) => {
    const canonical = name === 'DATA_CLONE_ERR' ? 'DataCloneError' : name;
    try { callback(); } catch (error) {
      expect(error instanceof DOMException, description || 'Expected a DOMException');
      assert_equals(error.name, canonical, description);
      if (canonical === 'DataCloneError') assert_equals(error.code, 25, description);
      return;
    }
    assert_unreached(description || 'Expected a DOMException');
  };
  self.recordObservation = (name, value) => { observations[name] = value; };
  self.finishMessagePortTests = () => { loaded = true; publish(); };
})();
