# Windows 11, WSL2 and Linux

| Environment | Status |
|---|---|
| Linux (native) | Supported; developed and tested on Ubuntu 24.04. |
| **WSL2 Ubuntu on Windows 11** | **Primary tested environment** (all tests in this repository were run here). |
| Native Windows (`maw.exe`) | **Not validated.** The code avoids Unix-only APIs where practical, but it has not been built or run natively (no MSVC toolchain was available during development), and provider process control (signals, job objects) is untested. Use WSL2. |

## Recommended setup (Windows 11)

```powershell
wsl --install -d Ubuntu          # once; reboot if asked
```

Inside Ubuntu:

```bash
sudo apt update && sudo apt install -y git build-essential
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh     # Rust (to build MAW)
npm i -g @openai/codex                                              # or your preferred install method
# install Claude Code per https://code.claude.com/docs/en/setup
cd ~ && git clone <your MAW checkout> maw && cd maw && cargo install --path crates/maw-cli
maw doctor && maw auth codex && maw auth claude
```

Keep project repositories **inside the WSL filesystem** (`~/code/...`). Under `/mnt/c/...` git and builds are much slower and file-watching semantics differ; `maw doctor` warns when the working directory is a Windows mount.

## Paths

Pass Windows paths if you like: under WSL, `maw --project 'C:\Users\me\repo'` is translated to `/mnt/c/Users/me/repo` (`platform::windows_to_wsl_path`, unit-tested). Terminal: any UTF-8 terminal (Windows Terminal recommended); set `NO_COLOR=1` or `maw config set ui.monochrome=true` for the monochrome theme.

## Developing MAW itself from Windows

Edit files on Windows, build in WSL with the helper that keeps `target/` on the Linux filesystem:

```powershell
wsl -d Ubuntu-24.04 -e bash -lc 'cd /mnt/c/path/to/MAW && ./scripts/wsl-cargo.sh test --workspace'
```

## Authentication

Windows and WSL logins are separate; log in from the environment MAW runs in (see AUTHENTICATION.md).
