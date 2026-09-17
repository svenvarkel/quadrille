"""Workbook sheet selection/edit/undo/native save through a real PTY."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time

from workbook_smoke import xlsx, ods

binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/qd').resolve())
with tempfile.TemporaryDirectory(prefix='quadrille-workbook-tui-') as tmp:
    for ext, make in [('xlsx', xlsx), ('ods', ods)]:
        source = Path(tmp) / f'source.{ext}'
        make(source)
        original = source.read_bytes()
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 110, 0, 0))
        process = subprocess.Popen([binary, str(source)], stdin=slave, stdout=slave, stderr=slave,
                                   env=dict(os.environ, TERM='xterm-256color'))
        os.close(slave)
        output = bytearray()

        def collect(seconds=0.15):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if select.select([master], [], [], min(0.05, max(0, until - time.monotonic())))[0]:
                    try:
                        data = os.read(master, 65536)
                        if not data:
                            break
                        output.extend(data)
                    except OSError:
                        break

        def send(data):
            os.write(master, data)
            collect()

        def read(path, sheet, cell):
            result = subprocess.run([binary, str(path), '--sheet', sheet, '--read', cell], capture_output=True, check=True)
            return json.loads(result.stdout)['rows'][0][0]

        def wait_read(path, sheet, cell, expected):
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                collect()
                try:
                    if read(path, sheet, cell) == expected:
                        return
                except subprocess.CalledProcessError:
                    pass
            assert read(path, sheet, cell) == expected

        try:
            deadline = time.monotonic() + 5
            while b'00123' not in output and time.monotonic() < deadline and process.poll() is None:
                collect()
            assert b'00123' in output, output[-2000:]
            send(b'w')
            send(b'\x1b[B\r')  # Choose Notes using Down + Enter.
            send(b'\x073\r')  # Ctrl+G row 3, then B3.
            send(b'\x1b[C\r')
            send(b'temporary\r')
            send(b'\x1a')  # Undo.
            send(b'\x13\r')
            saved = source.with_name(f'source.edited.{ext}')
            deadline = time.monotonic() + 5
            while not saved.exists() and time.monotonic() < deadline:
                collect()
            assert saved.exists(), output[-2000:]
            assert saved.read_bytes() == original, 'Undo did not restore the exact workbook'
            send(b'\r')
            send(b'Changed note\r')
            send(b'w')  # Pending edits must block sheet switching.
            assert b'edits to a native workbook' in output, output[-2000:]
            send(b'\x13')
            send(b'\x01')
            saved2 = source.with_name(f'notes-edited.{ext}')
            send(str(saved2).encode() + b'\r')
            deadline = time.monotonic() + 5
            while not saved2.exists() and time.monotonic() < deadline:
                collect()
            assert read(saved2, 'Notes õ', 'B3') == 'Changed note'
            send(b'w')
            send(b'1\r')  # Switch into Data from the newly saved workbook.
            send(b'\x1b[B\x1b[C\x1b[C\r')  # C2
            send(b'Changed data\r')
            if ext == 'xlsx':
                send(b'\x1b[C\x1b[C\x1b[C\x1b[C\r')  # Skip empty columns to G2.
                send(b'=SUM(B2:B3)\r')
            send(b'\x13\r')
            saved3 = saved2.with_name(f'notes-edited.edited.{ext}')
            deadline = time.monotonic() + 5
            while not saved3.exists() and time.monotonic() < deadline:
                collect()
            assert read(saved3, 'Data', 'C2') == 'Changed data'
            assert read(saved3, 'Notes õ', 'B3') == 'Changed note'
            if ext == 'xlsx':
                formula = subprocess.run(
                    [binary, str(saved3), '--sheet', 'Data', '--read', 'G2'],
                    capture_output=True, check=True)
                assert json.loads(formula.stdout)['formulas']['G2'] == 'SUM(B2:B3)'
            send(b'\x13')
            send(b'\x01' + str(saved2).encode() + b'\r')  # Replace the open source.
            wait_read(saved2, 'Data', 'C2', 'Changed data')
            if ext == 'xlsx':
                send(b'\x1b[D\x1b[D\x1b[D\x1b[D')  # G2 back to C2 after reload.
            send(b'\rAfter reload\r')
            send(b'\x13')
            send(b'\x01' + str(saved2).encode() + b'\r')
            wait_read(saved2, 'Data', 'C2', 'After reload')
            assert source.read_bytes() == original
            send(b'q')
            assert process.wait(timeout=3) == 0
            assert b'\x1b[?1006l' in output and b'\x1b[?1049l' in output
            print(ext, 'PTY PASS: sheets, edit, undo, save, source replacement/reload, switch with retained edits, clean exit')
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
