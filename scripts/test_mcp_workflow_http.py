#!/usr/bin/env python3
"""Run real HTTP contracts with an isolated simulated SDK and offline restart."""

import json
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import tempfile
import time
import urllib.request
import zlib

repo = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("MCP_BINARY", repo / "target/debug/android-computer-use-mcp")).resolve()
assert binary.is_file(), "Build the provider first with cargo build --locked"


def run_phase(root, phase):
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    base = {key: os.environ[key] for key in ["PATH", "HOME"] if key in os.environ}
    env = {
        **base,
        "ANDROID_COMPUTER_USE_MCP_SDK_ROOT": str(root / "sdk"),
        "ANDROID_COMPUTER_USE_MCP_BIND_ADDR": f"127.0.0.1:{port}",
        "ANDROID_COMPUTER_USE_MCP_ENVIRONMENT_ID": "http-contract",
        "ANDROID_COMPUTER_USE_MCP_PROVIDER_INSTANCE_ID": "simulated-provider",
        "ANDROID_COMPUTER_USE_MCP_SESSION_ID": "isolated-test",
    }
    with (root / f"provider-{phase}.log").open("wb") as log:
        server = subprocess.Popen(
            [str(binary), "--artifact-dir", str(root / "artifacts")],
            env=env, stdout=log, stderr=subprocess.STDOUT,
        )
        try:
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError("Provider exited before HTTP readiness")
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=1) as response:
                        assert response.status == 200
                    break
                except OSError:
                    time.sleep(0.1)
            else:
                raise RuntimeError("Provider HTTP readiness timed out")
            subprocess.run(
                ["node", str(repo / "scripts/mcp_workflow_http_contracts.mjs")],
                env={
                    **base, "MCP_ENDPOINT": f"http://127.0.0.1:{port}/mcp",
                    "MCP_HTTP_CONTRACT_PHASE": phase,
                    "DIRECT_HTTP_EVIDENCE": str(root / f"evidence-{phase}.json"),
                },
                check=True, timeout=180,
            )
        except Exception:
            log.flush()
            print((root / f"provider-{phase}.log").read_text()[-5000:])
            raise
        finally:
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


def png_chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


with tempfile.TemporaryDirectory(prefix="android-mcp-http-contract-") as temporary:
    root = Path(temporary)
    sdk = root / "sdk"
    for relative in ["platform-tools", "emulator", "cmdline-tools/latest/bin"]:
        (sdk / relative).mkdir(parents=True)
    adb = sdk / "platform-tools/adb"
    shutil.copyfile(repo / "tests/fixtures/fake_adb.py", adb)
    adb.chmod(0o755)
    for relative in ["emulator/emulator", "cmdline-tools/latest/bin/avdmanager"]:
        executable = sdk / relative
        executable.write_text("#!/bin/sh\nexit 1\n")
        executable.chmod(0o755)
    (sdk / "screen.png").write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
        + png_chunk(b"IDAT", zlib.compress(b"\x00\xff\xff\xff"))
        + png_chunk(b"IEND", b"")
    )
    (sdk / "ui.xml").write_text(
        '<hierarchy rotation="0"><node index="0" text="Home" resource-id="fixture:id/home" '
        'class="android.widget.TextView" package="fixture" clickable="false" enabled="true" '
        'bounds="[0,0][1,1]" /></hierarchy>'
    )
    run_phase(root, "execute")
    expected = [
        "shell input keyevent KEYCODE_HOME",
        "shell input keyevent KEYCODE_BACK",
        "shell input keyevent KEYCODE_HOME",
    ]
    effects = [json.loads(line) for line in (sdk / "effects.jsonl").read_text().splitlines()]
    assert [event["command"] for event in effects] == expected, effects
    (sdk / "offline").touch()
    run_phase(root, "recover")
    assert [json.loads(line) for line in (sdk / "effects.jsonl").read_text().splitlines()] == effects
    print("HTTP contracts passed: exactly three simulated mutations; restart recovery performed no device action.")
