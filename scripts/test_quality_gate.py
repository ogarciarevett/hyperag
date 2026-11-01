"""Exercise the contributor command with real pinned tools and disposable sources."""

    def test_wrong_python_metadata_pin_stops_before_tools(self):
        for package, pin in (("black", "26.10.1"), ("ruff", "0.14.3")):
            with self.subTest(package):
                result = self.check_python("answer = 42\n", pins={package: "0.0.0"})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"{package} must be {pin}", result.stderr)
                self.assertNotIn("+", result.stdout)
                self.assertNotIn("CONTINUED", result.stdout)
