// WPT's pinned setupStickyTest assigns Element.style as a string.
// Forward those writes to the engine's native inline-style declaration.
let stylePrototype = Object.getPrototypeOf(document.body);
let styleProperty;
while (stylePrototype !== null) {
    styleProperty = Object.getOwnPropertyDescriptor(stylePrototype, "style");
    if (styleProperty !== undefined) break;
    stylePrototype = Object.getPrototypeOf(stylePrototype);
}
if (styleProperty === undefined || typeof styleProperty.get !== "function") {
    throw new Error("Expected an inherited inline-style getter");
}

if (styleProperty.set === undefined) {
    Object.defineProperty(stylePrototype, "style", {
        get: styleProperty.get,
        set(value) { this.style.cssText = value; },
        enumerable: styleProperty.enumerable,
        configurable: styleProperty.configurable,
    });
}
