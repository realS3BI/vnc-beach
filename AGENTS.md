## Project Instructions

- Goal: a VNC server and viewer that work on macOS and Windows and stay free. Interoperability with standard VNC clients/servers (RFB 3.8, RFC 6143) is a hard requirement; don't add proprietary protocol changes without a negotiated (pseudo-)encoding or security type.
- README and user-facing UI text are German with real umlauts (`Zurück`, not `Zurueck`). Code, comments, log messages and CLI help are English.
- Protocol logic belongs in `crates/rfb` with unit tests; platform code (capture, input, clipboard) stays in `beach-server`; UI in `beach-viewer`.
- Run `cargo test --workspace` and `cargo build --workspace` before finishing. Windows code can't be compiled on the Mac (Homebrew Rust, no Windows target). Sebastian's PC (SSH host alias `sebastian-pc` from `~/.ssh/config`; address and key are not in this repo) has a GNU toolchain: sync the source to `%LOCALAPPDATA%\vnc-beach\src` (tar over SSH with `COPYFILE_DISABLE=1`) and build with `%LOCALAPPDATA%\vnc-beach\cargo.cmd build --release --workspace`. Note that `xcap::Monitor` is not `Send` on Windows.
- The installed viewer on the PC is `%LOCALAPPDATA%\Programs\vnc-beach\vnc-beach.exe` (Start menu: `vnc-beach`, `vnc-beach - Mac`). Copy a new build there after changes.
- Testing the server on this Mac: use `cargo run --release -p rfb --example snapshot -- HOST:PORT PASSWORD out.ppm` instead of opening the viewer on the same machine; viewer and server fight over the mouse pointer.
- macOS needs Screen Recording and Accessibility permissions for whatever process launches the server. Without Screen Recording, captures silently contain only wallpaper and menu bar.
