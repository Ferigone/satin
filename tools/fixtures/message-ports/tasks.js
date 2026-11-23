'use strict';

async_test(t => {
  const carrier = new MessageChannel();
  const channel = new MessageChannel();
  const bytes = new Uint8Array([19, 23]);
  const order = [];
  const data = { bytes, port: channel.port2, integer: 9007199254740993n };
  data.self = data;
  channel.port1.postMessage('before transfer');
  carrier.port2.onmessage = t.step_func(event => {
    assert_true(event.isTrusted);
    assert_equals(event.target, carrier.port2);
    assert_equals(event.data.self, event.data);
    assert_equals(event.data.port, event.ports[0]);
    assert_equals(event.data.integer, 9007199254740993n);
    assert_true(Object.isFrozen(event.ports));
    assert_array_equals(Array.from(event.data.bytes), [19, 23]);
    order.push('receive');
    queueMicrotask(() => order.push('receive microtask'));
    event.ports[0].onmessage = t.step_func(message => {
      order.push(message.data);
      if (message.data === 'after transfer') {
        assert_array_equals(order, ['send microtask', 'receive', 'receive microtask', 'before transfer', 'after transfer']);
        t.done();
      }
    });
  });
  carrier.port1.postMessage(data, { transfer: [bytes.buffer, channel.port2] });
  assert_equals(bytes.byteLength, 0);
  assert_equals(order.length, 0);
  queueMicrotask(() => order.push('send microtask'));
  channel.port1.postMessage('after transfer');
}, 'Transferred queues and microtask checkpoints');

async_test(t => {
  const channel = new MessageChannel();
  const seen = [];
  channel.port2.onmessage = t.step_func(event => {
    seen.push(event.data);
    if (event.data === 1) channel.port2.close();
  });
  channel.port1.postMessage(1);
  channel.port1.postMessage(2);
  t.step_timeout(() => {
    recordObservation('messagesAlreadyQueuedOnClose', seen);
    assert_equals(seen[0], 1);
    t.done();
  }, 150);
}, 'Record delivery of a packet already queued at receiver close');

async_test(t => {
  const transferred = new MessageChannel();
  const closed = new MessageChannel();
  const seen = [];
  transferred.port2.onmessage = t.step_func(() => seen.push('original'));
  transferred.port1.postMessage({
    get field() {
      const port = structuredClone(transferred.port2, { transfer: [transferred.port2] });
      port.onmessage = t.step_func(() => seen.push('transferred'));
      return 1;
    }
  });
  closed.port2.onmessage = t.step_func(() => seen.push('closed'));
  closed.port1.postMessage({ get field() { closed.port2.close(); return 2; } });
  t.step_timeout(() => {
    recordObservation('receiverChangedBySerializationGetter', seen);
    assert_array_equals(seen, ['transferred']);
    t.done();
  }, 150);
}, 'Receiver transfer and close from a serialization getter');
