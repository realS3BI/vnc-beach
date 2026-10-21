#!/usr/bin/env python3
"""Exercise SSH identity checking, key authentication and the POSIX WoL relay.

Requires OpenSSH server/client tools. All keys, pins and server configuration
are temporary; the server listens only on loopback and uses no account password.
"""

import getpass
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time


def main():
    sshd = shutil.which("sshd") or "/usr/sbin/sshd"
    if not Path(sshd).is_file():
        raise SystemExit("OpenSSH server is required for this fixture.")
    with tempfile.TemporaryDirectory(prefix="vnc-beach-ssh-") as directory:
        root = Path(directory)
        for name in ("host", "client"):
            subprocess.run(
                ["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)],
                check=True,
            )
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        config = root / "sshd_config"
        config.write_text(
            f"Port {port}\nListenAddress 127.0.0.1\n"
            f'HostKey "{root}/host"\nPidFile "{root}/pid"\n'
            f'AuthorizedKeysFile "{root}/client.pub"\n'
            "StrictModes no\nUsePAM no\nPasswordAuthentication no\n"
            "KbdInteractiveAuthentication no\nPubkeyAuthentication yes\n"
        )
        with (root / "log").open("w+") as log:
            server = subprocess.Popen([sshd, "-D", "-e", "-f", str(config)], stdout=log, stderr=log)
            try:
                for _ in range(50):
                    if server.poll() is not None:
                        log.seek(0)
                        raise SystemExit(log.read())
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                            break
                    except OSError:
                        time.sleep(0.1)
                else:
                    raise SystemExit("The fixture SSH server did not start.")
                environment = os.environ.copy()
                environment.update(
                    VNC_BEACH_TEST_SSH_ADDRESS=f"127.0.0.1:{port}",
                    VNC_BEACH_TEST_SSH_USER=getpass.getuser(),
                    VNC_BEACH_TEST_SSH_KEY=str(root / "client"),
                )
                result = subprocess.run(
                    ["cargo", "test", "-p", "beach-access", "openssh_identity_authentication_and_relay", "--", "--ignored", "--nocapture"],
                    cwd=Path(__file__).resolve().parent.parent,
                    env=environment,
                )
                if result.returncode:
                    log.seek(0)
                    print(log.read())
                    raise SystemExit(result.returncode)
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


if __name__ == "__main__":
    main()
