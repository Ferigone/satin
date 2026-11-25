"""Run each script from the pinned 2016 website in a fresh Satin VM.

Usage: python3 legacy-vm.py --website /path/to/website --worker /path/to/rb-js --output outcomes.jsonl
Git checkout: https://github.com/v8/test262 at e1166500a0d26c8b90e704a31c6646992573f632.
"""
import argparse
import base64
import concurrent.futures
from contextlib import suppress
import hashlib
import json
from pathlib import Path
from queue import Empty, Queue
import shutil
import subprocess
import threading

REVISION = 'e1166500a0d26c8b90e704a31c6646992573f632'
BLOBS = {
    "harness/Date_constants.js": "b9e072f533d66ec731801ca40725e6a3bd137b6e",
    "harness/Date_library.js": "a971ebb2eef2e1914aa249cd4116d96097124f89",
    "harness/PromiseHelper.js": "59ba0c74c9d6d34e707fb2b79f4e67bbd48762e1",
    "harness/arrayContains.js": "b295de5f3fbaa2642ea953570a4e79a24adf9a8d",
    "harness/assert.js": "97d585fa9254c4f0db3fb48207c40857011e1eab",
    "harness/compareArray.js": "6eb69e6eba012ddc1a1d25f81cbeb4d1b2398111",
    "harness/fnGlobalObject.js": "0a68c2da738418bbd469016ab130193dd25febb4",
    "harness/math_isequal.js": "a68434e075d645cc6ebc83de2efebfc9028ebfe6",
    "harness/math_precision.js": "fd6ed1d0a326b0de93d509546cb87f5ecbef9e88",
    "harness/numeric_conversion.js": "427eeaa21e8b54084efdec4644bbe6175c4945b6",
    "harness/propertyHelper.js": "e2627877e4c3bdbc854a1509740c5255b03b4926",
    "harness/proxyTrapsHelper.js": "04a504b85ef60f81b259d9e3915a0b225bcf5e7b",
    "harness/sta.js": "5a95afb1841e54ab0a099030a8194a39a0396ab0",
    "harness/tco-helper.js": "5e9bee2257713180fc18d827c4f4d14e9d50e800",
    "harness/testBuiltInObject.js": "a9c498c136655cdf6b8491c88272ac56a63dbe3c",
    "harness/testTypedArray.js": "1d783c58b5a2573c40a8f160792f728c658db172",
    "json/annexB.json": "311493be2b21d85657d0311b0708dfef1921712b",
    "json/built-ins.json": "e482c293486e09d5ca82410b4d4c4301e1bf05e0",
    "json/default.json": "9861bfd6bced2019bfa7fe2d10373f6dd5045505",
    "json/harness.json": "2351ecac3fb317e593619ecc0b6276f7172bd5fe",
    "json/language.json": "e102878d7e06d81d456ea4c01005bf9ad1ea2599",
    "scripts/ed.js": "1cc6c9603003f3001acf9c51e2d49207f98fd77d",
    "scripts/gs.js": "e0768f99a6e17f562769fa85deb678a6f7a3508e"
}
local = threading.local()


def pinned(root, name):
    data = (root / name).read_bytes()
    sha = hashlib.sha1(f'blob {len(data)}\0'.encode() + data).hexdigest()
    if sha != BLOBS[name]:
        raise ValueError(f'{name} differs from pinned upstream {REVISION}')
    return data.decode('utf-8-sig')


def requests(root):
    manifest = json.loads(pinned(root, 'json/default.json'))
    shared = {}
    records = []
    for path in manifest['testSuite']:
        data = json.loads(pinned(root, path))['testsCollection']
        if len(data['tests']) != int(data['numTests']):
            raise ValueError(f'chapter count mismatch: {path}')
        for t in data['tests']:
            source = base64.b64decode(t['code'], validate=True).decode('utf-8')
            description = dict(t)
            description['id'] = Path(t['path']).stem
            description['code'] = source
            setup = [{'name': 'test metadata', 'source': 'var testDescrip = ' +
                      json.dumps(description, ensure_ascii=True) + ';'}]
            for name in ([f'harness/{name}' for name in t.get('includes', [])] +
                         ['harness/sta.js', 'scripts/ed.js', 'scripts/gs.js', 'harness/assert.js']):
                if name not in shared:
                    shared[name] = {'name': name, 'source': pinned(root, name)}
                setup.append(shared[name])
            flags = t.get('flags', [])
            records.append(dict(id=t['path'], legacy_website=True, source=source, setup=setup,
                                strict='onlyStrict' in flags and 'raw' not in flags,
                                async_test='$DONE' in source))
    if len(records) != manifest['numTests'] or len(set(r['id'] for r in records)) != len(records):
        raise ValueError('catalog count or unique identities do not match')
    return records


def exchange(proc, outgoing, incoming):
    try:
        while (payload := outgoing.get()) is not None:
            proc.stdin.write(payload)
            proc.stdin.flush()
            line = proc.stdout.readline()
            if not line:
                raise EOFError("worker exited before replying")
            incoming.put(line)
    except (OSError, EOFError) as err:
        incoming.put(err)
    finally:
        with suppress(OSError):
            proc.stdin.close()
        proc.stdout.close()


def run_case(command, timeout, request, workers):
    proc = getattr(local, 'worker', None)
    if proc is None or proc.poll() is not None:
        proc = subprocess.Popen([command, '--worker'], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        local.worker = proc
        local.outgoing, local.incoming = Queue(), Queue()
        thread = threading.Thread(target=exchange, args=(proc, local.outgoing, local.incoming), daemon=True)
        thread.start()
        workers.put((proc, local.outgoing, thread))
    try:
        # Start the deadline before writing. A stopped worker can fill a pipe
        # before even a single large case has been delivered.
        local.outgoing.put(json.dumps(request, separators=(',', ':')).encode() + b'\n')
        line = local.incoming.get(timeout=timeout)
        if isinstance(line, Exception):
            raise line
        reply = json.loads(line)
        if not isinstance(reply, dict) or reply.get('id') != request['id'] or reply.get('status') not in ('pass', 'fail', 'harness_error'):
            raise ValueError('invalid worker reply')
        if 'diagnostic_profile' in request:
            settings = reply.get('diagnostic_profile')
            if (not isinstance(settings, dict) or settings.get('name') != request['diagnostic_profile']
                    or settings.get('max_instructions') != request['max_instructions']):
                raise ValueError('worker did not confirm the requested diagnostic settings')
        return reply
    except (OSError, EOFError, Empty, ValueError) as err:
        reason = 'worker deadline exceeded' if isinstance(err, Empty) else str(err)
        if proc.poll() is None:
            proc.kill()
        proc.wait()
        return {'id': request['id'], 'status': 'harness_error', 'message': reason}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--website', type=Path, required=True)
    parser.add_argument('--worker', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--jobs', type=int, default=4)
    parser.add_argument('--timeout', type=int, default=10)
    parser.add_argument('--filter', help='only run paths containing this substring')
    parser.add_argument('--require-all-pass', action='store_true',
                        help='exit 1 if any selected case fails')
    parser.add_argument('--diagnostic-profile', choices=['web-vm-alpha37'],
                        help='record isolated VM diagnostics using versioned document VM limits')
    parser.add_argument('--max-instructions', type=int,
                        help='per-script fuel for --diagnostic-profile (1 to 100000000, default 1000000)')
    args = parser.parse_args()
    if args.max_instructions is not None:
        if not args.diagnostic_profile or not 1 <= args.max_instructions <= 100_000_000:
            parser.error('--max-instructions requires --diagnostic-profile and a value from 1 to 100000000')
    if args.jobs <= 0 or args.timeout <= 0:
        parser.error('--jobs and --timeout must be positive')
    executable = shutil.which(args.worker)
    if not executable:
        parser.error(f'worker executable was not found: {args.worker}')
    try:
        catalog = requests(args.website)
    except (OSError, ValueError) as err:
        parser.error(str(err))
    selected = [(i, case) for i, case in enumerate(catalog, 1)
                if not args.filter or args.filter in case['id']]
    if not selected:
        parser.error('empty selection')
    if args.diagnostic_profile:
        fuel = args.max_instructions or 1_000_000
        for _, case in selected:
            case.update(diagnostic_profile=args.diagnostic_profile, max_instructions=fuel)
        digest = hashlib.sha256()
        with Path(executable).open('rb') as worker:
            for chunk in iter(lambda: worker.read(1024 * 1024), b''):
                digest.update(chunk)
        provenance = {'requested_profile': args.diagnostic_profile, 'max_instructions': fuel,
                      'corpus_revision': REVISION, 'worker_sha256': digest.hexdigest(),
                      'process_timeout_seconds': args.timeout}
        print('Isolated VM diagnostic', json.dumps(provenance, sort_keys=True), flush=True)
    totals = {}
    workers = Queue()
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool, args.output.open('w') as dest:
            results = pool.map(lambda item: run_case(args.worker, args.timeout, item[1], workers), selected)
            for completed, ((index, _), result) in enumerate(zip(selected, results), 1):
                result['index'] = index
                if args.diagnostic_profile:
                    result['diagnostic_run'] = provenance
                dest.write(json.dumps(result, ensure_ascii=True, separators=(',', ':')) + '\n')
                totals[result['status']] = totals.get(result['status'], 0) + 1
                if completed % 500 == 0:
                    dest.flush()
                    print(completed, totals, flush=True)
    except OSError as err:
        parser.exit(2, f'{parser.prog}: {err}\n')
    finally:
        while not workers.empty():
            proc, outgoing, thread = workers.get_nowait()
            outgoing.put(None)
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
            thread.join(timeout=2)
    print('Final', totals)
    if totals.get('harness_error'):
        return 2
    return int(args.require_all_pass and totals.get('pass', 0) != len(selected))


if __name__ == '__main__':
    raise SystemExit(main())
