"""Regression checks for portable defaults and anonymous build transcripts."""

import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


PROJECT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "sanitize_log", PROJECT / "scripts/sanitize_log.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class BuildPrivacyTests(unittest.TestCase):
    def test_paths_and_accounts_are_replaced_without_changing_placeholders(self):
        environment = {
            "HOME": "/example/home/example-user",
            "USER": "example-user",
            "LOGNAME": "login-user",
            "SJ_PROJECT": "/example/home/example-user/local/src/sj",
            "SJ_VENV": "/example/home/example-user/.venvs/dev",
            "SJ_INSTALL_DIR": "/example/install",
        }
        transcript = (
            "Building /example/home/example-user/local/src/sj\n"
            "/example/home/example-user/.venvs/dev/bin/rustc\n"
            "Installed /example/install/sj for example-user (login-user)\n"
            "/example/home/example-user/other example-user-extra\n"
        )
        self.assertEqual(
            MODULE.redact(transcript, environment),
            "Building ${SJ_PROJECT}\n${SJ_VENV}/bin/rustc\n"
            "Installed ${SJ_INSTALL_DIR}/sj for ${USER} (${USER})\n"
            "${HOME}/other example-user-extra\n",
        )
        self.assertEqual(MODULE.redact("${USER} startup", {"USER": "u"}), "${USER} startup")

    def test_stream_filter_uses_the_environment(self):
        with tempfile.TemporaryFile(mode="w+") as stream:
            stream.write("/example/home/example-user as example-user\n")
            stream.seek(0)
            completed = subprocess.run(
                [os.sys.executable, str(PROJECT / "scripts/sanitize_log.py")],
                env={"HOME": "/example/home", "USER": "example-user"},
                stdin=stream, text=True, capture_output=True, check=True,
            )
        self.assertEqual(completed.stdout, "${HOME}/${USER} as ${USER}\n")

    def test_script_defaults_follow_the_selected_environment(self):
        for script in ("build.sh", "setup-toolchain.sh"):
            # Read only the variable declarations; never install a toolchain here.
            prefix = (PROJECT / script).read_text().split('mkdir -p "$SJ_PROJECT/.logs"')[0]
            declarations = "\n".join(
                line for line in prefix.splitlines()
                if line.startswith(("SJ_VENV=", "SJ_INSTALL_DIR=", "SJ_UV="))
            )
            with tempfile.TemporaryDirectory(prefix="sj-defaults-") as temporary:
                base = {"HOME": temporary, "PATH": "/usr/bin:/bin"}
                cases = (
                    ({}, f"{temporary}/.venvs/dev"),
                    ({"VIRTUAL_ENV": "/example/active-venv"}, "/example/active-venv"),
                    ({"SJ_VENV": "/example/selected-venv", "VIRTUAL_ENV": "/example/active-venv"}, "/example/selected-venv"),
                )
                for overrides, expected in cases:
                    with self.subTest(script=script, overrides=overrides):
                        command = declarations + '\nprintf "%s\\n" "$SJ_VENV" "${SJ_INSTALL_DIR-}" "${SJ_UV-}"'
                        completed = subprocess.run(
                            ["bash", "-c", command], env=base | overrides,
                            text=True, capture_output=True, check=True,
                        )
                        values = completed.stdout.splitlines()
                        self.assertEqual(values[0], expected)
                        if script == "build.sh":
                            self.assertEqual(values[1], f"{temporary}/local/bin")
                        else:
                            self.assertEqual(values[2], f"{temporary}/.local/bin/uv")


if __name__ == "__main__":
    unittest.main()
