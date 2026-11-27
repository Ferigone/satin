(function () {
    const p = Object.create({ a: 1, b: 2, c: 3, d: 4 });
    let n = 0;
    for (let i = 0; i < 26000; ++i) {
        n += p.a + p.b + p.c + p.d;
    }
    return n === 260000;
})()
