// Local adapter for the public Testharness APIs used by the pinned specimens.
(function () {
  var cases = [];
  var pending = 0;
  var loaded = false;
  var chain = Promise.resolve();

  function publish() {
    if (!loaded || pending !== 0 || cases.length === 0) return;
    var receipt = document.getElementById('wpt-receipt');
    if (!receipt) {
      receipt = document.createElement('pre');
      receipt.id = 'wpt-receipt';
      document.body.appendChild(receipt);
    }
    receipt.textContent = JSON.stringify(cases);
  }

  function makeCase(name) {
    var record = { name: name || 'Unnamed test', status: 'pending' };
    var cleanups = [];
    cases.push(record);
    pending++;
    function finish(status, detail) {
      if (record.status !== 'pending') return;
      for (var i = 0; i < cleanups.length; i++) {
        try { cleanups[i](); } catch (error) {
          status = 'fail';
          detail = String(error);
        }
      }
      record.status = status;
      if (detail !== undefined) record.detail = detail;
      pending--;
      publish();
    }
    function step(callback, receiver, args) {
      if (record.status !== 'pending') return;
      try {
        return callback.apply(receiver, args);
      } catch (error) {
        finish('fail', String(error));
      }
    }
    return {
      step: function (callback) { return step(callback, undefined, []); },
      step_func: function (callback) {
        return function () { return step(callback, this, arguments); };
      },
      step_func_done: function (callback) {
        return function () {
          step(callback, this, arguments);
          finish('pass');
        };
      },
      unreached_func: function (detail) {
        return function () { finish('fail', detail); };
      },
      add_cleanup: function (callback) { cleanups.push(callback); },
      done: function () { finish('pass'); },
      fail: function (error) { finish('fail', String(error)); },
    };
  }

  window.test = function (body, name) {
    var t = makeCase(name);
    t.step(body);
    t.done();
  };
  window.async_test = function (body, name) {
    var t = makeCase(name);
    t.step(function () { body(t); });
    return t;
  };
  window.promise_test = function (body, name) {
    var t = makeCase(name);
    chain = chain.then(function () {
      return Promise.resolve().then(function () { return body(t); }).then(t.done, t.fail);
    });
  };
  window.assert_equals = function (actual, expected, detail) {
    if (!Object.is(actual, expected)) {
      throw new Error((detail || 'assert_equals') + ': expected ' + expected + ', got ' + actual);
    }
  };
  window.assert_true = function (actual, detail) { assert_equals(actual, true, detail || 'assert_true'); };
  window.assert_false = function (actual, detail) { assert_equals(actual, false, detail || 'assert_false'); };
  window.addEventListener('load', function () {
    loaded = true;
    publish();
  });
})();
