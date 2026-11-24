// The VM supplies private intrinsic operations. Guest callbacks never receive
// this table or the collection record. Await uses the realm's promise jobs.
(async function (operations, constructor, items, mapper, receiver) {
    "use strict";
    const state = operations.prepare(constructor, items, mapper);
    let index = 0;
    if (state.iterator === null) {
        while (index < state.length) {
            let value = await state.source[index];
            if (mapper !== void 0) {
                value = await operations.call(mapper, receiver, value, index);
            }
            operations.define(state.target, index, value);
            index++;
        }
    } else {
        while (true) {
            const item = await operations.call(state.next, state.iterator);
            operations.requireObject(item);
            if (item.done) break;
            let value = item.value;
            try {
                if (mapper !== void 0) {
                    value = await operations.call(mapper, receiver, value, index);
                }
                operations.define(state.target, index, value);
            } catch (error) {
                try {
                    const close = state.iterator.return;
                    if (close !== null && close !== void 0) {
                        await operations.call(close, state.iterator);
                    }
                } catch (ignored) {}
                throw error;
            }
            index++;
        }
    }
    state.target.length = index;
    return state.target;
})
