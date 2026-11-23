window.sharedModuleEvaluations = (window.sharedModuleEvaluations || 0) + 1;

export const token = {};
export let count = 0;

export function increment() {
  count++;
}
