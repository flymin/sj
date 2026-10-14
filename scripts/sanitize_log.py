"""Hide local account and filesystem identifiers before build logs are saved."""

import os
import re
import sys


def redact(text, environment):
    keys = (
        "SJ_PROJECT", "SJ_VENV", "SJ_TOOLCHAIN", "SJ_PREFIX", "CARGO_HOME",
        "CARGO_TARGET_DIR", "SJ_INSTALL_DIR", "SJ_DOWNLOAD", "SJ_UV", "HOME",
    )
    paths = sorted(
        ((environment[key], "${" + key + "}") for key in keys if environment.get(key)),
        key=lambda item: len(item[0]),
        reverse=True,
    )
    # One pass keeps short overlapping paths from changing generated placeholders.
    replacements = dict(reversed(paths))
    if replacements:
        pattern = "|".join(re.escape(path) for path, _ in paths)
        text = re.sub(pattern, lambda match: replacements[match[0]], text)
    for key in ("USER", "LOGNAME"):
        user = environment.get(key)
        if user:
            text = re.sub(
                r"(?<![\w.-])" + re.escape(user) + r"(?![\w.-])",
                lambda _: "${USER}",
                text,
            )
    return text


if __name__ == "__main__":
    for line in sys.stdin:
        sys.stdout.write(redact(line, os.environ))
        sys.stdout.flush()
