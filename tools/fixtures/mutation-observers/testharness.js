// Local adapter for the Testharness APIs used by these unchanged specimens.
(function () {
  var cases = [];
  var pending = 0;
  var loaded = false;
  var sequence = Promise.resolve();

  function publish() {
    if (!loaded || pending || !cases.length) return;
    var receipt = document.getElementById('wpt-receipt');
    if (!receipt) {
      receipt = document.createElement('pre');
      receipt.id = 'wpt-receipt';
      document.body.appendChild(receipt);
    }
    receipt.textContent = JSON.stringify(cases);
  }

  function begin(name) {
    var record = { name: name || 'Unnamed test', status: 'pending' };
    cases.push(record);
    pending++;
    function finish(status, reason) {
      if (record.status !== 'pending') return;
      record.status = status;
      if (reason !== undefined) record.detail = String(reason);
      pending--;
      if (loaded && !pending) publish();
    }
    var test = {
      done: function () { finish('pass'); },
      fail: function (reason) { finish('fail', reason); },
      step: function (body) {
        if (record.status !== 'pending') return;
        try { return body(); } catch (error) { finish('fail', error); }
      },
      step_timeout: function (body, delay) {
        return setTimeout(function () { test.step(body); }, delay);
      },
      unreached_func: function (reason) {
        return function () { finish('fail', reason || 'Unexpected invocation'); };
      },
    };
    return test;
  }

  window.setup = function (body) { body(); };
  window.test = function (body, name) {
    var test = begin(name);
    try { body(); } catch (error) { test.fail(error); return; }
    test.done();
  };
  window.async_test = function (body, name) {
    var hasBody = typeof body === 'function';
    var test = begin(hasBody ? name : body);
    if (hasBody) {
      try { body(test); } catch (error) { test.fail(error); }
    }
    return test;
  };
  window.promise_test = function (body, name) {
    var test = begin(name);
    sequence = sequence.then(function () {
      try { return Promise.resolve(body(test)).then(test.done, test.fail); }
      catch (error) { test.fail(error); }
    });
  };
  window.assert_equals = function (actual, expected, description) {
    if (!Object.is(actual, expected)) {
      throw new Error((description || 'assert_equals') + ': expected ' + expected + ', got ' + actual);
    }
  };
  window.assert_true = function (actual, description) {
    if (actual !== true) throw new Error(description || 'assert_true');
  };
  window.assert_array_equals = function (actual, expected, description) {
    if (actual.length !== expected.length) {
      throw new Error((description || 'assert_array_equals') + ': expected length ' + expected.length + ', got ' + actual.length);
    }
    var mismatch = -1;
    for (var i = 0; i < expected.length && mismatch < 0; i++)
      mismatch = Object.is(actual[i], expected[i]) ? -1 : i;
    if (mismatch >= 0)
      throw new Error((description || 'assert_array_equals') + ': mismatch at index ' + mismatch);
  };
  window.addEventListener('load', function () {
    loaded = true;
    publish();
  });
})();
