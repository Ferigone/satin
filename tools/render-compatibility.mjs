import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { gunzipSync } from 'node:zlib';

const root = new URL('../', import.meta.url);
const load = async path => JSON.parse(await readFile(new URL(path, root), 'utf8'));
const snapshot = await load('docs/compatibility/snapshot.json');
const js = await load('docs/compatibility/test262.json');
if (!js.fullSuite) throw new Error('The README requires a full Test262 run (--path .).');
const outcomeFile = await readFile(new URL('docs/compatibility/test262-outcomes.tsv.gz', root));
if (createHash('sha256').update(gunzipSync(outcomeFile)).digest('hex') !== js.outcomesSha256) {
  throw new Error('The Test262 outcomes archive and summary refer to different results.');
}

const number = n => n.toLocaleString('en-US');
const ratio = (pass, total) => (100 * pass / total).toFixed(2) + '%';
const escape = value => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
const scopes = [
  ['language', 'Core language'], ['built-ins', 'Built-in objects'], ['intl402', 'Intl (ECMA-402)'],
  ['annexB', 'Annex B'], ['harness', 'Test harness'], ['staging', 'Staging']
];
const jsCells = scopes.map(([folder, label]) => {
  const { statuses, totalVariants } = js.byFolder[folder];
  return { label, pass: statuses.pass, total: totalVariants, scope: `Test262 · \`${folder}\`` };
});
if (Object.keys(js.byFolder).length !== jsCells.length) throw new Error('Review new Test262 folders before rendering.');

// An exact pass ratio is printed in every measured cell. Colour indicates the band.
const bands = [
  { min: 0,   name: '0–<25%',   fill: '#502c2c', ink: '#ffad9e' },
  { min: .25, name: '25–<50%',  fill: '#503723', ink: '#ffc491' },
  { min: .5,  name: '50–<75%',  fill: '#4d4224', ink: '#f6d887' },
  { min: .75, name: '75–<100%', fill: '#263e37', ink: '#a1dac5' },
  { min: 1,   name: '100%',     fill: '#154943', ink: '#94efdc' }
];
const unknown = { name: 'No full run', fill: '#2a2a2d', ink: '#c5c3c8' };
const font = 'system-ui,-apple-system,BlinkMacSystemFont,Segoe UI,sans-serif';

function label(x, y, value, size, colour, weight = 400, align = 'start') {
  return `<text x="${x}" y="${y}" font-size="${size}" fill="${colour}" font-weight="${weight}" text-anchor="${align}">${escape(value)}</text>`;
}

function tile(cell, x, y) {
  const measured = cell.pass !== null;
  const band = measured ? bands.findLast(b => cell.pass / cell.total >= b.min) : unknown;
  const value = measured ? ratio(cell.pass, cell.total) : 'Unmeasured';
  const units = measured ? `${number(cell.pass)} / ${number(cell.total)}` : 'Full CSS WPT suite';
  return `<g transform="translate(${x} ${y})">
    <title>${escape(cell.label + ' · ' + value + ' · ' + units)}</title>
    <rect width="244" height="84" rx="7" fill="${band.fill}"/>
    ${label(14, 24, cell.label, 16, '#f5f2ec', 600)}
    ${label(13, 57, value, measured ? 30 : 25, band.ink, 650)}
    ${label(15, 75, units, 13, '#e0dad1')}
  </g>`;
}

function section(y, title, caption, cells) {
  return [label(20, y, title, 20, '#f6f3ed', 650),
    label(768, y, caption, 13, '#b7b5b1', 450, 'end'),
    ...cells.map((cell, i) => tile(cell, 20 + (i % 3) * 252, y + 12 + Math.floor(i / 3) * 92))
  ].join('\n');
}

function renderSvg() {
  const legend = [...bands, unknown].map((b, i) => {
    const x = [20, 120, 239, 358, 487, 574][i];
    return `<rect x="${x}" y="503" width="13" height="13" rx="3" fill="${b.ink}"/>\n` +
      label(x + 19, 514, b.name, 13, '#d4d0cb', 500);
  }).join('\n');
  return `<svg xmlns="http://www.w3.org/2000/svg" width="788" height="532" viewBox="0 0 788 532" role="img" aria-labelledby="chart-title chart-desc">
  <title id="chart-title">Satin compatibility heatmap</title>
  <desc id="chart-desc">Pass rates by Test262 directory, selected CSS Web Platform Tests, HTML parser corpus and Canvas pixel tests. The README includes a text table with every cell.</desc>
  <rect x=".5" y=".5" width="787" height="531" rx="13" fill="#19191b" stroke="#424044"/>
  <g font-family="${font}" style="font-variant-numeric:tabular-nums">
  ${section(33, 'JavaScript', 'Test262  /  all six directories', jsCells)}
  ${section(254, 'CSS', 'Web Platform Tests', snapshot.css)}
  ${section(389, 'HTML & graphics', 'Pinned test cases', snapshot.web)}
  ${legend}
  </g>
</svg>\n`;
}

const overall = js.overall;
const testRate = ratio(overall.statuses.pass, overall.totalVariants);
const allCells = [...jsCells, ...snapshot.css, ...snapshot.web];
const table = allCells.map(cell => {
  const source = cell.source ? `[${cell.scope}](${cell.source})` : cell.scope;
  const values = cell.pass === null ? ['—', 'Unmeasured'] :
    [`${number(cell.pass)} / ${number(cell.total)}`, ratio(cell.pass, cell.total)];
  return `| ${cell.label} | ${values.join(' | ')} | ${source} |`;
}).join('\n');
const revision = snapshot.satinRevision;
const baseLink = `https://github.com/Ferigone/satin/commit/${revision}`;
const suiteLink = `https://github.com/tc39/test262/tree/${js.test262Revision}`;
const statuses = Object.entries(overall.statuses)
  .filter(([name, n]) => name !== 'pass' && n > 0)
  .map(([name, n]) => `${number(n)} \`${name}\``).join(', ');

const block = `<!-- compatibility:begin -->
Satin’s JavaScript engine passed **${number(overall.statuses.pass)} of ${number(overall.totalVariants)} Test262 checks (${testRate})** on ${snapshot.platform}. **${number(overall.passedFiles)} of ${number(overall.testedFiles)} files** passed in every required mode. Measured ${snapshot.date} on [Satin \`${revision.slice(0, 7)}\`](${baseLink}) against [Test262 \`${js.test262Revision.slice(0, 7)}\`](${suiteLink}).

![Compatibility heatmap showing JavaScript by Test262 directory, selected CSS tests, HTML parsing and Canvas pixel tests. Exact counts and sources follow.](assets/compatibility-heatmap.svg)

Colours show the pass rate within each named suite. The CSS results cover sticky positioning and logical properties with variables. A full CSS Web Platform Tests (WPT) run is still open.

<details>
<summary>Exact counts, test scope and reproduction</summary>

| Suite | Passed / total | Pass rate | Scope and evidence |
| --- | ---: | ---: | --- |
${table}

The standalone JavaScript worker ran every eligible test file in the pinned checkout. A file can contribute both a strict and a non-strict result. All outcomes count toward the denominator. The remaining results were ${statuses}. A file whose metadata the runner cannot read contributes one result. The separate legacy website probe passed its 55-case harness chapter. [Legacy browser probe](tools/test262/website.md)

The Test262 run used ${snapshot.build}. Each variant had a ${js.limits.timeoutMs / 1000}-second timeout and a ${number(js.limits.maxInstructions)}-instruction budget. [Per-file and per-variant outcomes (TSV.GZ)](docs/compatibility/test262-outcomes.tsv.gz) · [Machine-readable totals](docs/compatibility/test262.json) · [Reproduce the measurement](tools/test262/README.md#reports-and-limits)

</details>
<!-- compatibility:end -->`;

const readmeUrl = new URL('README.md', root);
const readme = await readFile(readmeUrl, 'utf8');
const region = /<!-- compatibility:begin -->[\s\S]*?<!-- compatibility:end -->/g;
if ([...readme.matchAll(region)].length !== 1) throw new Error('Expected one compatibility region in README.md.');
await writeFile(new URL('assets/compatibility-heatmap.svg', root), renderSvg());
await writeFile(readmeUrl, readme.replace(region, () => block));
