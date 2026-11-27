(function () {
    const map = new Map();
    const keys = new Set();
    const prefix = "compiled-component-property-shared-prefix-";
    for (let i = 0; i < 850; ++i) {
        const key = prefix + i;
        map.set(key, i);
        keys.add(key);
    }
    for (let i = 0; i < 850; i += 11) {
        if (map.get(prefix + i) !== i || !keys.has(prefix + i)) return false;
    }
    const iterator = map.keys();
    map.delete(prefix + 1);
    map.set(prefix + 1, -1);
    return map.size === 850 && keys.size === 850 &&
        iterator.next().value === prefix + 0 &&
        iterator.next().value === prefix + 2 &&
        map.get(prefix + 1) === -1 && !keys.has(prefix + 850);
})()
