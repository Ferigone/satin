(function () {
    const b = { compiledComponentConfigurationNameUsedDuringInitialization: 1 };
    let n = 0;
    for (let i = 0; i < 18000; ++i) {
        n += b.compiledComponentConfigurationNameUsedDuringInitialization;
    }
    return n === 18000;
})()
