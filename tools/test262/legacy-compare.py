"""Compare full isolated-VM runs of the pinned 16,436-case legacy website."""
import argparse
from collections import Counter
import gzip
import hashlib
import json
from pathlib import Path


def read_report(path):
    with path.open(encoding='utf-8') as source:
        rows = [json.loads(line) for line in source]
    if len(rows) != 16436:
        raise ValueError(f'{path}: expected 16,436 rows, found {len(rows)}')
    seen = set()
    for index, row in enumerate(rows, 1):
        path_id = row['id']
        if (row['index'] != index or not path_id.startswith('TestCases/')
                or path_id in seen or any(ch in path_id for ch in '\t\r\n')):
            raise ValueError(f'{path}: invalid case index or path at row {index}')
        seen.add(path_id)
    return rows


def baseline_result(row):
    if row['status'] == 'pass':
        return 'pass'
    if row['status'] == 'fail':
        return 'nonpass'
    # The first alpha.37 adapter could hit the heap limit again while reporting it.
    if (row['status'] == 'harness_error' and row.get('error', {}).get('kind') == 'HeapLimit'
            and row.get('evaluation_error', {}).get('kind') == 'HeapLimit'):
        return 'nonpass'
    raise ValueError(f"{row['id']}: unexpected baseline outcome {row['status']}")


def candidate_result(row):
    if row.get('verdict_source') == 'website-callback' and row['status'] in ('pass', 'fail'):
        return row['status']
    if (row.get('verdict_source') == 'vm' and row['status'] == 'fail' and row.get('resource_limit')
            and row.get('evaluation_error', {}).get('kind') in ('ExecutionLimit', 'HeapLimit')):
        return 'vm_limit'
    raise ValueError(f"{row['id']}: incomplete or unrecognized candidate outcome")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        baseline = read_report(args.baseline)
        candidate = read_report(args.candidate)
        lines = ['index\tpath\talpha37\talpha38\n']
        transitions = Counter()
        for old, new in zip(baseline, candidate):
            if old['id'] != new['id']:
                raise ValueError(f"case paths differ at index {old['index']}")
            before = baseline_result(old)
            after = candidate_result(new)
            lines.append(f"{old['index']}\t{old['id']}\t{before}\t{after}\n")
            transitions[before, after] += 1
        data = ''.join(lines).encode('utf-8')
        with args.output.open('wb') as dest:
            with gzip.GzipFile(filename='', fileobj=dest, mode='wb', compresslevel=9, mtime=0) as archive:
                archive.write(data)
    except (KeyError, TypeError, AttributeError, ValueError, OSError) as err:
        parser.exit(2, f'{parser.prog}: {err}\n')
    print(json.dumps({'cases': len(baseline), 'gained': transitions['nonpass', 'pass'],
                      'lost': transitions['pass', 'fail'] + transitions['pass', 'vm_limit'],
                      'sha256_tsv': hashlib.sha256(data).hexdigest()}))


if __name__ == '__main__':
    main()
