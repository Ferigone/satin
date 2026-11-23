(function (shard) {
    function digit(u) { return u >= 48 && u <= 57; }
    function upper(u) { return u >= 65 && u <= 90; }
    function word(u) { return digit(u) || upper(u) || (u >= 97 && u <= 122) || u === 95; }
    function space(u) { return (u >= 9 && u <= 13) || u === 32; }
    var cases = [
        [String.raw`[^"\\\n\r]`, function (u) { return u !== 34 && u !== 92 && u !== 10 && u !== 13; }],
        [String.raw`[\s0-9_]`, function (u) { return space(u) || digit(u) || u === 95; }],
        [String.raw`[^\d\s]`, function (u) { return !digit(u) && !space(u); }],
        [String.raw`[^\W_ ]`, function (u) { return word(u) && u !== 95; }],
        [String.raw`[A-Z_$]`, function (u) { return upper(u) || u === 95 || u === 36; }],
        [String.raw`[\x00-\x20\x7f]`, function (u) { return u <= 32 || u === 127; }],
        [String.raw`[\u0100-\u0200\u03a9]`, function () { return false; }],
        [String.raw`[^\x60-\u0200\u0300]`, function (u) { return u < 96; }],
        [String.raw`[\d]`, digit],
        ["[]", function () { return false; }],
    ];
    var count = 0;
    for (var i = shard; i < cases.length; i += 2) {
        var row = cases[i];
        for (var flags of ["dy", "duy"]) {
            var re = new RegExp(row[0], flags);
            for (var unit = 0; unit < 128; unit++) {
                // Sticky mode bypasses the unrelated prefix seek.
                var allowed = row[1](unit);
                re.lastIndex = 0;
                var match = re.exec(String.fromCharCode(unit));
                if ((match !== null) !== allowed || re.lastIndex !== (allowed ? 1 : 0) ||
                    (allowed && (match[0].charCodeAt(0) !== unit ||
                        match.indices[0][0] !== 0 || match.indices[0][1] !== 1))) {
                    throw new Error(JSON.stringify([row[0], flags, unit, allowed]));
                }
                count++;
            }
        }
    }
    if (count !== 1280) throw new Error("missing ASCII controls");

    function snapshot(re, input, index) {
        re.lastIndex = index;
        var match = re.exec(input);
        return JSON.stringify([re.lastIndex, match === null ? null : [
            match.index, Array.from(match), match.indices, match.groups, match.indices.groups,
        ]]);
    }
    // Reference patterns use single-member atoms or a lookaround plus dot,
    // so these comparisons do not rely on multi-member class membership.
    var contexts = [
        [String.raw`"(?<body>[^"\\\n\r]*)"`, String.raw`"(?<body>(?:(?!(?:"|\\|\n|\r))(?s:.))*)"`,
            ['""', '"😀x\ud800"', 'z"a\nb" "ok"', '"a\\b" "x"', '"unclosed']],
        [String.raw`(?<head>[ab]+)b$`, String.raw`(?<head>(?:a|b)+)b$`,
            ["abb", "aab", "b", "xyzabb", "a😀b"]],
        [String.raw`(?<=([ab]{2}))x`, String.raw`(?<=((?:a|b){2}))x`,
            ["abx", "bax", "axb", "😀abx", "b😀x"]],
        [String.raw`x(?i:[a-z_])(?-i:[A-Z_])`, String.raw`x(?i:(?:[a-z]|_))(?-i:(?:[A-Z]|_))`,
            ["xKA", "xſZ", "xAB", "Xa_", "za", "xKa"]],
    ];
    for (var i = shard; i < contexts.length; i += 2) {
        var row = contexts[i];
        for (var flags of ["dg", "dy", "dgu", "duy", "dgiu"]) {
            var actual = new RegExp(row[0], flags);
            var control = new RegExp(row[1], flags);
            for (var input of row[2]) {
                for (var index of [0, 1, 2, 10]) {
                    if (snapshot(actual, input, index) !== snapshot(control, input, index)) {
                        throw new Error(JSON.stringify([row[0], flags, input, index]));
                    }
                }
                if (input.replace(actual, "<$&>") !== input.replace(control, "<$&>")) {
                    throw new Error("replacement mismatch: " + row[0]);
                }
            }
        }
    }
    return true;
})(classAuditSlice);
