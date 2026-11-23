import flavor from "flavor";

export const unscoped = flavor;

export function importSharedB() {
  return import("../modules/shared.mjs");
}
