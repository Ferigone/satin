// The selected canvas tests call _addTest once and make synchronous assertions.
function async_test(name) {
    return { name: name, done: function() {} };
}

let canvasCase;

function _addTest(run) {
    canvasCase = run;
}

function runCanvasWpt() {
    if (!canvasCase) return;
    try {
        const canvas = document.getElementById("c");
        const context = canvas.getContext("2d");
        canvasCase(canvas, context);
        wptResults.push({ name: t.name, status: "PASS" });
    } catch (error) {
        wptResults.push({ name: t.name, status: "FAIL", message: String(error) });
    }
}

function _assertSame(actual, expected, actualExpression, expectedExpression) {
    assert_equals(actual, expected, actualExpression + " === " + expectedExpression);
}

function _assertPixel(canvas, x, y, r, g, b, a) {
    const context = canvas.getContext("2d");
    const pixel = context.getImageData(x, y, 1, 1).data;
    if (pixel[0] !== r || pixel[1] !== g || pixel[2] !== b || pixel[3] !== a) {
        throw new Error("pixel at " + x + "," + y + " expected "
            + [r, g, b, a].join(",") + " got " + pixel.join(","));
    }
}

globalThis.canvasWptResults = wptResults;
