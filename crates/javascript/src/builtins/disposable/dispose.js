// The intrinsic operations and the linked resource records are private to the
// engine. All guest calls use the receiver and argument list saved at adoption.
({
    sync(ops, stack) {
        let next = ops.begin(stack, false);
        let error, failed = false;
        while (next !== null) {
            const node = next;
            next = node.previous;
            try { ops.invoke(node); }
            catch (e) {
                error = failed ? ops.suppress(e, error) : e;
                failed = true;
            }
        }
        if (failed) throw error;
    },
    async async(ops, stack) {
        let next = ops.begin(stack, true);
        let error, failed = false, needsAwait = false, hasAwaited = false;
        async function disposeSync(node) { ops.invoke(node); }
        while (next !== null) {
            const node = next;
            next = node.previous;
            if (node.kind === 4) {
                needsAwait = true;
                continue;
            }
            try {
                const result = node.kind === 3 ? disposeSync(node) : ops.invoke(node);
                hasAwaited = true;
                await result;
            } catch (e) {
                error = failed ? ops.suppress(e, error) : e;
                failed = true;
            }
        }
        if (needsAwait && !hasAwaited) await undefined;
        if (failed) throw error;
    },
    async iterator(ops, it) {
        const finish = it.return;
        if (finish !== undefined && finish !== null) await ops.finish(finish, it);
    }
})
