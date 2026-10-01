#!/usr/bin/env python3
"""Exercise immutable publication using a local fake gh; never contacts GitHub."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().with_name('publish-tag.sh')

class ImmutablePublication(unittest.TestCase):
    def test_existing_assets_are_never_mutated(self):
        for identical in (True, False):
            with self.subTest(identical=identical), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                (root / 'artifacts').mkdir()
                (root / 'existing').mkdir()
                (root / 'artifacts' / 'artifact.tar.gz').write_bytes(b'example-payload')
                (root / 'existing' / 'artifact.tar.gz').write_bytes(b'example-payload' if identical else b'different')
                gh = root / 'gh'
                gh.write_text('''#!/bin/sh
case "$2" in
view) exit 0 ;;
download) cp "$FAKE_RELEASE"/* "$5/" ;;
*) echo 'Unexpected mutation' >&2; exit 99 ;;
esac
''')
                gh.chmod(0o755)
                result = subprocess.run(['bash', str(SCRIPT), 'v0.0.2', str(root / 'artifacts')], env={**os.environ, 'PATH': f'{root}:{os.environ["PATH"]}', 'FAKE_RELEASE': str(root / 'existing')}, capture_output=True)
                self.assertEqual(result.returncode == 0, identical, result.stderr)

if __name__ == '__main__':
    unittest.main()
