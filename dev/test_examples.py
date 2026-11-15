
#!/usr/bin/env python3
"""Integration tests for Forge's example projects.

These tests are separate from `cargo test`. They invoke the built Forge
binary and real C/C++ toolchains.

Usage:
    cargo build --release
    python3 dev/test_examples.py

Optional environment variables:
    FORGE_BIN  Path to the Forge executable.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
EXAMPLES = ROOT / "examples"

FORGE = Path(
    os.environ.get(
        "FORGE_BIN",
        str(
            ROOT
            / "target"
            / "release"
            / ("forge.exe" if sys.platform == "win32" else "forge")
        ),
    )
).resolve()

PROJECTS = {
    "c-library": {
        "expected_output": [
            "sqrt(25.0) = 5.0",
            "clamp(42, 0, 10) = 10.0",
        ],
    },
    "cpp-profiles": {
        "expected_output": [
            "Hello, Forge!",
        ],
    },
    "mixed-language": {
        "expected_output": [
            "Message: Hello from Forge",
            "Checksum: 0x",
        ],
    },
}


class IntegrationTestFailure(Exception):
    """Raised when an integration test fails."""


def run(
    command: list[str],
    *,
    cwd: Path,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run a command and capture its output."""
    result = subprocess.run(
        command,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    if check and result.returncode != 0:
        raise IntegrationTestFailure(
            f"Command failed with exit code {result.returncode}:\n"
            f"  {' '.join(command)}\n\n"
            f"stdout:\n{result.stdout}\n"
            f"stderr:\n{result.stderr}"
        )

    return result


def require_tool(name: str) -> None:
    """Ensure a required executable is available."""
    if not shutil.which(name):
        raise IntegrationTestFailure(
            f"Required tool '{name}' was not found on PATH."
        )


def require_project(name: str) -> Path:
    """Return an example directory after validating its existence."""
    project = EXAMPLES / name

    if not project.is_dir():
        raise IntegrationTestFailure(
            f"Example project does not exist: {project}"
        )

    manifest = project / "project.toml"
    if not manifest.is_file():
        raise IntegrationTestFailure(
            f"Missing manifest: {manifest}"
        )

    return project


def copy_project(source: Path, destination: Path) -> Path:
    """Copy an example into an isolated temporary directory."""
    project = destination / source.name
    shutil.copytree(source, project)
    return project


def forge(
    project: Path,
    *arguments: str,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run Forge against a project's manifest."""
    return run(
        [
            str(FORGE),
            *arguments,
            "--manifest",
            str(project / "project.toml"),
        ],
        cwd=project,
        check=check,
    )


def build(
    project: Path,
    *,
    profile: str | None = None,
) -> subprocess.CompletedProcess[str]:
    arguments = ["build", "-v"]

    if profile:
        arguments.extend(["--profile", profile])

    return forge(project, *arguments)


def executable(project: Path, name: str, profile: str) -> Path:
    """Return the expected executable path for the current platform."""
    if sys.platform == "win32":
        name += ".exe"

    return project / "build" / profile / "bin" / name


def run_program(
    project: Path,
    name: str,
    profile: str,
    expected_output: list[str],
) -> str:
    """Run a built example and verify its output."""
    program = executable(project, name, profile)

    if not program.is_file():
        raise IntegrationTestFailure(
            f"Expected executable was not produced: {program}"
        )

    result = run([str(program)], cwd=project)
    output = result.stdout

    for expected in expected_output:
        if expected not in output:
            raise IntegrationTestFailure(
                f"{name}: expected output to contain {expected!r}, "
                f"but got:\n{output}"
            )

    return output


def test_c_library(temp_root: Path) -> None:
    print("[TEST] c-library")

    source = require_project("c-library")
    project = copy_project(source, temp_root)

    build(project)
    run_program(
        project,
        "c-library",
        "debug",
        PROJECTS["c-library"]["expected_output"],
    )

    build(project, profile="release")
    run_program(
        project,
        "c-library",
        "release",
        PROJECTS["c-library"]["expected_output"],
    )

    forge(project, "clean")

    if executable(project, "c-library", "debug").exists():
        raise IntegrationTestFailure(
            "c-library executable still exists after forge clean"
        )

    print("[PASS] c-library")


def test_cpp_profiles(temp_root: Path) -> None:
    print("[TEST] cpp-profiles")

    source = require_project("cpp-profiles")
    project = copy_project(source, temp_root)

    build(project, profile="debug")
    debug_output = run_program(
        project,
        "cpp-profiles",
        "debug",
        PROJECTS["cpp-profiles"]["expected_output"],
    )

    if "Debug configuration" not in debug_output:
        raise IntegrationTestFailure(
            "Debug profile did not produce the expected debug output:\n"
            f"{debug_output}"
        )

    build(project, profile="release")
    release_output = run_program(
        project,
        "cpp-profiles",
        "release",
        PROJECTS["cpp-profiles"]["expected_output"],
    )

    if "Release configuration" not in release_output:
        raise IntegrationTestFailure(
            "Release profile did not produce the expected release output:\n"
            f"{release_output}"
        )

    graph = forge(project, "graph", "--dot")
    if "digraph" not in graph.stdout:
        raise IntegrationTestFailure(
            "forge graph --dot did not produce a DOT graph"
        )

    explanation = forge(project, "explain")
    if not explanation.stdout.strip():
        raise IntegrationTestFailure(
            "forge explain produced no output"
        )

    print("[PASS] cpp-profiles")


def test_mixed_language(temp_root: Path) -> None:
    print("[TEST] mixed-language")

    source = require_project("mixed-language")
    project = copy_project(source, temp_root)

    build(project)
    run_program(
        project,
        "mixed-language",
        "debug",
        PROJECTS["mixed-language"]["expected_output"],
    )

    build(project, profile="release")
    run_program(
        project,
        "mixed-language",
        "release",
        PROJECTS["mixed-language"]["expected_output"],
    )

    print("[PASS] mixed-language")


def test_dependencies(temp_root: Path) -> None:
    """Build and run the example that consumes a local source dependency."""
    print("[TEST] dependencies")

    source = require_project("dependencies")
    dependency_project = temp_root / "c-library"
    if not dependency_project.is_dir():
        copy_project(require_project("c-library"), temp_root)
    project = copy_project(source, temp_root)
    expected = PROJECTS["c-library"]["expected_output"]

    build(project)
    run_program(project, "dependencies", "debug", expected)

    build(project, profile="release")
    run_program(project, "dependencies", "release", expected)

    print("[PASS] dependencies")


def main() -> int:
    if not FORGE.is_file():
        print(
            f"error: Forge binary not found: {FORGE}\n"
            "Build it first with: cargo build --release",
            file=sys.stderr,
        )
        return 2

    if not sys.platform.startswith(("linux", "darwin", "win32")):
        print(
            "error: these integration tests currently target Linux, macOS and Windows",
            file=sys.stderr,
        )
        return 2

    try:
        for name in ("CC", "CXX"):
            compiler = os.environ.get(name)
            if compiler:
                require_tool(compiler)
            else:
                candidates = (
                    ("clang", "gcc")
                    if name == "CC"
                    else ("clang++", "g++")
                )
                if sys.platform == "win32":
                    candidates += ("cl", "clang-cl")
                if not any(shutil.which(candidate) for candidate in candidates):
                    raise IntegrationTestFailure(
                        f"No suitable {name} compiler found. "
                        f"Install one or set {name} explicitly."
                    )

        with tempfile.TemporaryDirectory(prefix="forge-examples-") as temp:
            temp_root = Path(temp)

            test_c_library(temp_root)
            test_cpp_profiles(temp_root)
            test_mixed_language(temp_root)
            test_dependencies(temp_root)

    except (IntegrationTestFailure, OSError) as error:
        print(f"\n[FAIL] {error}", file=sys.stderr)
        return 1

    print("\nAll example integration tests passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
