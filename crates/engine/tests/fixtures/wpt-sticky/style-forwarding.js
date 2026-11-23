// WPT's pinned setupStickyTest assigns Element.style as a string.
// Forward those writes to the engine's native inline-style declaration.
const stylePrototype = Object.getPrototypeOf(document.body);
const styleProperty = Object.getOwnPropertyDescriptor(stylePrototype, "style");

if (styleProperty.set === undefined) {
    Object.defineProperty(stylePrototype, "style", {
        get: styleProperty.get,
        set(value) { this.style.cssText = value; },
        enumerable: styleProperty.enumerable,
        configurable: styleProperty.configurable,
    });
}
