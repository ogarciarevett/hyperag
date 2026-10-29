#!/usr/bin/env python3
"""Test the separately maintained coordinator helper against synthetic responses.

Run with the path to hyperag-manual.py as the only argument.
"""
import contextlib
import importlib.util
import io
import json
import pathlib
import sys
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('manual', sys.argv.pop(1))
manual = importlib.util.module_from_spec(spec)
spec.loader.exec_module(manual)


class Response:
    def __init__(self, value):
        self.value = value

    def __enter__(self):
        return io.BytesIO(json.dumps(self.value).encode())

    def __exit__(self, *args):
        pass


class ScopeFilterTest(unittest.TestCase):
    def test_flags_forwarded_and_old_server_fails_closed(self):
        for response, expected in [({'retrieval': {}}, 1), ({'retrieval': {'scopeFilterApplied': True}}, 0)]:
            requests = []
            def urlopen(request, timeout):
                requests.append(json.loads(request.data))
                return Response(response)
            with patch.object(sys, 'argv', ['manual', 'search', 'fixture', '--scope', 'a', '--exclude-scope', 'b']), \
                 patch.object(pathlib.Path, 'read_text', return_value='{"HYPERAG_COORDINATOR_TOKEN":"fixture"}'), \
                 patch.object(manual.urllib.request, 'urlopen', side_effect=urlopen), \
                 contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(manual.main(), expected)
            self.assertEqual(requests[0]['scopes'], ['a'])
            self.assertEqual(requests[0]['excludeScopes'], ['b'])


if __name__ == '__main__':
    unittest.main()
