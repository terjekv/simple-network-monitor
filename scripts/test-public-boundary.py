#!/usr/bin/env python3
"""Check publication ignore rules without creating operational files."""
from pathlib import Path
import re
import subprocess
import sys

root = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent
samples = ['.env', '.env.production', 'nested/.env', 'network-monitor.db-wal', 'nested/state.db', 'fake.key', 'fake.pem', 'cert.crt', 'cert.p12']
for sample in samples:
    result = subprocess.run(['git', 'check-ignore', '--no-index', '-q', sample], cwd=root)
    assert result.returncode == 0, f'Operational file is not ignored: {sample}'
tracked = subprocess.check_output(['git', 'ls-files', '-z'], cwd=root).decode().split('\0')
for name in filter(None, tracked):
    assert not re.search(r'(^|/)(\.env($|\.)|monitor(?:\..+)?\.toml$)|\.(?:db(?:-.*)?|sqlite3?(?:-.*)?|key|pem|crt|cer|p12|pfx)$', name) or name in {'monitor.example.toml', '.env.example'}, f'Operational file tracked: {name}'
print('Public repository boundary checks passed')
