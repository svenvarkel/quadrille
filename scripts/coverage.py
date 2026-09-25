"""Line-coverage gate, using only Python's stdlib.

Usage: python3 scripts/coverage.py [report.json]
Without an argument it runs `cargo llvm-cov --json --summary-only` itself.
Fails if a1.rs, ops.rs or cli.rs miss more lines than allow-listed below, or if any
other source file falls below its baseline (develop at e761b9f, see
docs/2026-09-25-ops-plan.md). A file missing from the report fails too.
"""
import json
from pathlib import Path
import subprocess
import sys

# Files that must be fully covered.
COMPLETE = ('a1.rs', 'ops.rs', 'cli.rs')
# Uncovered lines tolerated in COMPLETE files, only for paths that need an I/O fault:
# {'cli.rs': [('emit: stdout closed', 1)]}. Currently none are needed.
ALLOWED = {}
# Minimum line coverage, in percent, of every other file.
BASELINE = {
    'find.rs': 100.00,
    'lib.rs': 91.50,
    'main.rs': 50.70,
    'sort.rs': 97.57,
    'workbook.rs': 37.17,
}


def report(argv):
    if len(argv) > 1:
        return json.loads(Path(argv[1]).read_text())
    run = subprocess.run(['cargo', 'llvm-cov', '--json', '--summary-only'],
                         capture_output=True, text=True, check=True)
    return json.loads(run.stdout)


def main(argv):
    files = {Path(f['filename']).name: f['summary']['lines'] for f in report(argv)['data'][0]['files']}
    failures = []
    for name in (*COMPLETE, *BASELINE):
        lines = files.pop(name, None)
        if lines is None:
            failures.append(f'{name}: not in the report')
            continue
        missed = lines['count'] - lines['covered']
        if name in COMPLETE:
            allowed = sum(count for _, count in ALLOWED.get(name, []))
            ok = missed <= allowed
            detail = f'{missed} uncovered, {allowed} allowed'
        else:
            ok = lines['percent'] + 1e-9 >= BASELINE[name]
            detail = f'baseline {BASELINE[name]:.2f}%'
        print(f"{'ok  ' if ok else 'FAIL'} {name:12} {lines['percent']:6.2f}% ({detail})")
        if not ok:
            failures.append(name)
    for name in files:
        failures.append(f'{name}: no coverage rule; add it to COMPLETE or BASELINE')
    if failures:
        sys.exit('Coverage gate failed: ' + ', '.join(failures))
    print('Coverage gate passed')


if __name__ == '__main__':
    main(sys.argv)
