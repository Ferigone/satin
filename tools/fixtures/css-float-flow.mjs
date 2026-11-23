import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const [browser, executable, expected, gate = "ordinary"] = process.argv.slice(2);
if (!["satin", "chrome"].includes(browser) || !executable || !/^[a-f0-9]{64}$/.test(expected) ||
    !["ordinary", "run-panel"].includes(gate)) {
  throw new Error("usage: node css-float-flow.mjs satin|chrome <exe> <lowercase-sha256> [ordinary|run-panel]");
}
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const exe = resolve(executable);
const sha256 = digest(readFileSync(exe));
if (sha256 !== expected) throw new Error(`unexpected executable SHA-256: ${sha256}`);
let fixture = fileURLToPath(new URL("./css-float-flow.html", import.meta.url));
let bytes, url;
if (gate === "run-panel") {
  fixture = "183f808e3044df6b627296773019dd224ebe8c21:tools/fixtures/test262-run-panel-geometry.html";
  const archived = spawnSync("git", ["show", fixture], {
    cwd: fileURLToPath(new URL("../../", import.meta.url)),
    timeout: 5000,
  });
  if (archived.status !== 0) throw new Error(`cannot read pinned offline reduction: ${archived.stderr}`);
  bytes = archived.stdout;
  url = `data:text/html;base64,${bytes.toString("base64")}`;
} else {
  bytes = readFileSync(fixture);
  url = pathToFileURL(fixture).href;
}
const target = fileURLToPath(new URL("../../target/", import.meta.url));
mkdirSync(target, { recursive: true });
const out = mkdtempSync(join(target, `css-float-${gate}-${browser}-`));
const profile = join(out, "profile");
const dump = join(out, "dom.html");
const args = browser === "satin"
  ? ["--headless", "--profile", profile, "--url", url, "--dump-dom", dump,
     ...(gate === "run-panel" ? ["--width", "984", "--height", "265", "--run-for-ms", "50"] : [])]
  : ["--headless", "--disable-background-networking", "--no-first-run",
     ...(gate === "run-panel" ? ["--disable-gpu", "--hide-scrollbars", "--window-size=1000,360"] : []),
     `--user-data-dir=${profile}`, "--dump-dom", url];
const run = spawnSync(exe, args, { encoding: "utf8", timeout: 15000, windowsHide: true });
const html = browser === "chrome" ? run.stdout : run.status === 0 ? readFileSync(dump, "utf8") : "";
if (html && browser === "chrome") writeFileSync(dump, html);
let result, geometry;
if (gate === "ordinary") {
  result = html?.match(/<output\b[^>]*\bdata-result="([^"]*)"/i)?.[1] ?? "no-result";
} else {
  const json = html?.match(/<pre\b[^>]*\bid="geometry-probe"[^>]*>([^<]+)<\/pre>/i)?.[1];
  geometry = json && JSON.parse(json);
  const equal = (actual, expected) => JSON.stringify(actual) === JSON.stringify(expected);
  const failures = [];
  if (!geometry) failures.push("no-result");
  else {
    const { viewport, initial, loaded, running } = geometry;
    const check = (caseName, actual, expected) => {
      if (!equal(actual, expected)) failures.push(caseName);
    };
    check("viewport", viewport, [984, 265]);
    check("initial-hidden", initial.controls.slice(0, 2), [0, 0]);
    for (const [state, sample, width] of [["loaded", loaded, 160], ["running", running, 50]]) {
      check(`${state}-controls`, sample.controls, [width, 1, "block", true]);
      check(`${state}-placement`, sample.buttonFromBar, [20, 0]);
      check(`${state}-panel-bar`, [sample.panel[0], sample.bar[0]], [800, 462]);
      check(`${state}-buttons`, [sample.run[0], sample.pause[0]],
            state === "loaded" ? [80, 0] : [0, 50]);
    }
  }
  result = failures.length ? failures.join(";") : "pass";
}
const report = {
  fixture, fixtureSha256: digest(bytes), exe, sha256, args, exit: run.status, result, geometry,
  stdout: browser === "satin" ? run.stdout : undefined, stderr: run.stderr,
  error: run.error?.message,
};
writeFileSync(join(out, "report.json"), JSON.stringify(report, null, 2) + "\n");
console.log(JSON.stringify({ result, exit: run.status, report: join(out, "report.json") }));
process.exitCode = run.status === 0 && result === "pass" ? 0 : 1;
