(function () {
    // Semantic model only: the lookahead/backreference commits to the
    // maximal deterministic run. Its synthetic capture is projected out.
    function atomic(before, repeat, after) {
        return before + "(?=(?<__scan>" + repeat + "))\\k<__scan>" + after;
    }
    function named(obj) {
        if (obj === undefined) return [];
        return Object.keys(obj).filter(function (k) { return k !== "__scan"; })
            .map(function (k) { return [k, obj[k]]; });
    }
    function snapshot(result, regexp, drop) {
        return JSON.stringify([Object.is(regexp.lastIndex, -0) ? "-0" : String(regexp.lastIndex),
            result === null ? null : [
                result.index,
                result.input,
                Array.from(result).filter(function (_, i) { return i !== drop; }),
                result.indices.filter(function (_, i) { return i !== drop; }),
                named(result.groups), named(result.indices.groups)
            ]]);
    }
    var families = [
        // Same quoted-token loop shape as the hot validation pass.
        ['"', '[^"\\\\\\n\\r]*', '"', 1, ["", '""', '"A😀\\q"abc"ok"',
            'z"a\nd" and "good"', '"\ud800\udc00\ud800"', '"unterminated']],
        ["a", "[^!]{2,4}", "!z", 1, ["ab!z", "abc!z", "ax😀!z", "abcde!z", "abc!y abc!z"]],
        ["(b|bb)", "[^!]+", "!(c?)", 2, ["bbxc", "bb!!c", "zbb😀!c", "bbx!cc", "bx!"]],
        ["(?<tag>x)", "[0-9]*", ":$", 2, ["x999:", "x:", "ax42:\nx1:", "x42:x", "x😀:"]],
        ["^", ".*", "\\nend", 1, ["a\nend", "a\nwrong\nend", "😀\nend", "\r\nend", "x\u2028\nend"]],
        ["(?<=a)x", "[^>]{1,3}", ">(?<tail>z?)", 1, ["ax>", "ax\u2028>z", "ax😀>z", "bxA>z", "ax1234>"]],
    ];
    var count = 0;
    families = families.slice(disjointAuditSlice * 2, disjointAuditSlice * 2 + 2);
    for (var family of families) {
        for (var flags of ["g", "y", "gy", "ug", "uy", "mg", "my", "umy"]) {
            var before = family[0], repeat = family[1], after = family[2], capture = family[3];
            var plain = new RegExp(before + repeat + after, flags + "d");
            var committed = new RegExp(atomic(before, repeat, after), flags + "d");
            for (var text of family[4]) {
                for (var start of [-0, 1, 2, 4, 20]) {
                    plain.lastIndex = committed.lastIndex = start;
                    var expected = snapshot(plain.exec(text), plain, -1);
                    var actual = snapshot(committed.exec(text), committed, capture);
                    if (actual !== expected) {
                        throw new Error(JSON.stringify([before, repeat, after, flags, text, start, expected, actual]));
                    }
                    count++;
                }
                if (text.replace(plain, "<$&>") !== text.replace(committed, "<$&>")) {
                    throw new Error("disjoint replacement mismatch: " + before + repeat + after);
                }
                count++;
            }
        }
    }
    // Dropping backtracking on these shapes would change a real match.
    var unsafe = [
        ["a.*a", atomic("a", ".*", "a"), "", "aba"],
        ["a[ab]{1,3}a", atomic("a", "[ab]{1,3}", "a"), "", "aba"],
        ["(?i:x(?-i:[^a])*a)", "(?i:" + atomic("x", "(?-i:[^a])*", "a") + ")", "", "xA"],
        ["^.*\\nend", atomic("^", ".*", "\\nend"), "s", "a\nend"],
        ["a[^!]*!?b", atomic("a", "[^!]*", "!?b"), "", "ab"],
    ];
    for (var control of unsafe) {
        if (!new RegExp(control[0], control[2]).test(control[3]) ||
            new RegExp(control[1], control[2]).test(control[3])) {
            throw new Error("unsafe commitment control did not diverge: " + control[0]);
        }
        count++;
    }
    return count;
})();
