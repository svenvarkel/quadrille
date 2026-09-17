"""Exercise the release binary through a real PTY (macOS/Linux, Python stdlib)."""
import csv
import fcntl
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time

temporary = tempfile.TemporaryDirectory(prefix='quadrille-tui-')
root = Path(temporary.name)
source = root / 'source.csv'
original = b'id,name,note\r\n00123,Tallinn,"line 1\nline 2"\r\n00456,Tartu,last\r\n'
source.write_bytes(original)
out = root / 'source.edited.csv'
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 110, 0, 0))
env = dict(os.environ, TERM='xterm-256color')
process = subprocess.Popen(['./target/release/qd', str(source)], stdin=slave, stdout=slave, stderr=slave, env=env)
os.close(slave)
output = bytearray()

def collect(seconds=0.25):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if select.select([master], [], [], min(0.05, max(0, end-time.monotonic())))[0]:
            try:
                chunk = os.read(master, 65536)
                if not chunk: break
                output.extend(chunk)
            except OSError:
                break

def send(data):
    os.write(master, data)
    collect()

try:
    deadline = time.monotonic() + 5
    while (b'Quadrille' not in output or b'00123' not in output) and time.monotonic() < deadline and process.poll() is None:
        collect(0.1)
    assert b'Quadrille' in output and b'00123' in output, ('Initial grid missing', process.poll(), bytes(output[-2000:]))
    mark = len(output)
    send(b'?')
    assert b'NAVIGATION' in output[mark:]
    mark = len(output)
    send(b'\x1b[C')
    assert b'EDIT AND SAVE' in output[mark:]
    mark = len(output)
    send(b'\x1b[C')
    assert b'Sorting' in output[mark:], output[mark:]
    send(b'\x1b[Z')  # Shift+Tab returns to Editing.
    send(b'\x1b')
    send(b'h')
    send(b'\x1b')
    # SGR mouse reports: double-click B2 to open the editor.
    click_b2 = b'\x1b[<0;38;5M\x1b[<0;38;5m'
    send(click_b2)
    send(click_b2)
    send('New, "quoted" 🦀'.encode())
    send(b'\r')
    send(b'\x1a')  # Undo
    send(b'\r')
    send(b'\x1b[200~New, "quoted"\ncity\x1b[201~')
    send(b'\r')
    # Click F6 in the footer and sort descending by A, keeping the header.
    send(b'\x1b[<0;40;28M\x1b[<0;40;28m')
    send(b'-A\r')
    collect(0.5)
    send(b'\x13')  # Save As
    send(b'\r')
    collect(0.6)
    assert out.exists(), output[-2000:]
    with out.open(newline='') as f:
        rows = list(csv.reader(f))
    assert rows[2][1] == 'New, "quoted"\ncity', rows
    assert rows[1][0] == '00456'
    assert rows[2][0] == '00123'
    assert rows[2][2] == 'line 1\nline 2'
    assert source.read_bytes() == original
    before = out.stat().st_mtime_ns
    send(b'\x13\r')  # Existing output must not be replaced
    assert out.stat().st_mtime_ns == before, 'Existing destination was replaced'
    send(b'q')
    assert process.wait(timeout=3) == 0
    assert b'\x1b[?1006l' in output, 'Mouse capture was not disabled'
    assert b'\x1b[?1049l' in output, 'Terminal alternate screen was not restored'
    print('PTY smoke PASS: tabbed help, mouse double-click, edit, undo, paste, footer sort, Save As, no-clobber, clean exit')
finally:
    if process.poll() is None:
        process.kill(); process.wait()
    os.close(master)
    temporary.cleanup()
