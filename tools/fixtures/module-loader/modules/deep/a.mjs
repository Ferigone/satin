import flavor from "flavor";
import outer from "outer-only";
import global from "global/global-fallback.mjs";

export const scoped = [flavor, outer, global];

export function importSharedA() {
  return import("shared-loader");
}

function outcome(promise) {
  return promise.then(module => "fulfilled:" + module.default, error => "rejected:" + error.name);
}

export function probeBlocked() {
  return outcome(import("blocked"));
}

export function probeInvalidAddress() {
  return outcome(import("invalid-address"));
}
