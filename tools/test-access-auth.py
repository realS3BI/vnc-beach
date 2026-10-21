#!/usr/bin/env python3
"""Test password and keyboard-interactive SSH with disposable loopback servers.

Run in a Python environment with Paramiko installed. No system account password,
SSH daemon configuration or desktop session is accessed.
"""

import logging
import argparse
import os
from pathlib import Path
import socket
import subprocess
import threading

import paramiko


class Authentication(paramiko.ServerInterface):
    def __init__(self, mode, transport):
        self.mode = mode
        self.transport = transport

    def get_allowed_auths(self, username):
        return "keyboard-interactive" if self.mode == "interactive" else "password"

    def check_auth_password(self, username, password):
        if username != "fixture" or password != "fixture-password":
            return paramiko.AUTH_FAILED
        if self.mode == "disconnect":
            self.transport.close()
            return paramiko.AUTH_FAILED
        return paramiko.AUTH_SUCCESSFUL

    def check_auth_interactive(self, username, submethods):
        return paramiko.InteractiveQuery(
            "Fixture authentication", "Temporary test credentials",
            ("Password:", False), ("Code:", False),
        )

    def check_auth_interactive_response(self, responses):
        return paramiko.AUTH_SUCCESSFUL if responses == ["fixture-password", "123456"] else paramiko.AUTH_FAILED


def serve(listener, key, mode, stopped):
    while not stopped.is_set():
        try:
            connection, _ = listener.accept()
        except socket.timeout:
            continue
        except OSError:
            return
        with paramiko.Transport(connection) as transport:
            transport.add_server_key(key)
            try:
                transport.start_server(server=Authentication(mode, transport))
                while transport.is_active() and not stopped.wait(0.05):
                    pass
            except (EOFError, paramiko.SSHException):
                pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ios", action="store_true", help="also test the Swift/Rust bridge in the iPad simulator")
    args = parser.parse_args()
    logging.getLogger("paramiko").setLevel(logging.CRITICAL)
    key = paramiko.RSAKey.generate(2048)
    stopped = threading.Event()
    listeners = []
    threads = []
    environment = os.environ.copy()
    for mode in ("password", "interactive", "disconnect"):
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(4)
        listener.settimeout(0.2)
        listeners.append(listener)
        environment[f"VNC_BEACH_TEST_{mode.upper()}_ADDRESS"] = f"127.0.0.1:{listener.getsockname()[1]}"
        thread = threading.Thread(target=serve, args=(listener, key, mode, stopped), daemon=True)
        thread.start()
        threads.append(thread)
    try:
        result = subprocess.run(
            ["cargo", "test", "-p", "beach-access", "password_interactive_and_disconnect_authentication", "--", "--ignored", "--nocapture"],
            cwd=Path(__file__).resolve().parent.parent,
            env=environment,
        )
        if result.returncode == 0 and args.ios:
            for name, value in list(environment.items()):
                if name.startswith("VNC_BEACH_TEST_"):
                    environment["TEST_RUNNER_" + name] = value
            result = subprocess.run(
                ["xcodebuild", "-project", "VNCBeach.xcodeproj", "-scheme", "VNCBeach",
                 "-destination", "platform=iOS Simulator,name=LifeOS Kitchen iPad",
                 "-only-testing:VNCBeachTests/AccessTests", "test"],
                cwd=Path(__file__).resolve().parent.parent / "apps/ios",
                env=environment,
            )
        raise SystemExit(result.returncode)
    finally:
        stopped.set()
        for listener in listeners:
            listener.close()
        for thread in threads:
            thread.join(timeout=2)


if __name__ == "__main__":
    main()
