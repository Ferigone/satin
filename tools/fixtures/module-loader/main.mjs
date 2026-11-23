import { scoped, importSharedA, probeBlocked, probeInvalidAddress } from "./modules/deep/a.mjs";
import { unscoped, importSharedB } from "./outside/b.mjs";

const receipt = document.getElementById("module-loader-receipt");
const observed = { scoped, unscoped };

function publish() {
  receipt.textContent = JSON.stringify(observed);
}

async function checkCache() {
  // Both cold requests are issued in the same job, before either can settle.
  const fromA = importSharedA();
  const fromB = importSharedB();
  const [a, b] = await Promise.all([fromA, fromB]);
  const warm = await importSharedB();

  const initial = b.count;
  a.increment();
  observed.cache = {
    distinctPromises: fromA !== fromB,
    sameNamespace: a === b && b === warm,
    sameExport: a.token === b.token && b.token === warm.token,
    evaluations: window.sharedModuleEvaluations,
    liveBinding: [initial, b.count, warm.count],
  };
  publish();
}

async function checkBlocking() {
  observed.blocked = await probeBlocked();
  publish();
  observed.invalidAddress = await probeInvalidAddress();
  publish();
}

async function run() {
  try {
    await checkCache();
  } catch (error) {
    observed.cacheError = error.name;
    publish();
  }
  await checkBlocking();
}

function finish() {
  publish();
  receipt.setAttribute("data-complete", "yes");
}

publish();
run().then(finish, error => {
  observed.error = error.name;
  finish();
});
