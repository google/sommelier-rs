# Sommelier-rs: Virtio-GPU Cross-Domain Wayland Proxy

This project is a rust rewrite of the [Sommelier](https://chromium.googlesource.com/chromiumos/platform2/+/main/vm_tools/sommelier/) Wayland proxy. Its goal is to allow unmodified GUI applications running inside a virtual machine to display windows seamlessly onto the Host machine's desktop, complete with native window management and clipboard sharing. Supporting X is an explicit no-goal for this project.

This project is designed to be used as a companion to [crosvm](https://github.com/google/crosvm). It should also work with other virtual machine monitors (VMMs) that support virtio-gpu cross domain.

## Quick Start

**To run it on ChromeOS, use `virtwl` branch. Main branch will not work on ChromeOS.**

### Prerequisites

- Rust toolchain
- A Wayland compositor running on the host
- A VMM with virtio-gpu cross domain support enabled
- Linux pacakge dependencies installed in guest

### Build and Run (on a Debian-compatible distro)

1. Install dependencies

   ```bash
   sudo apt-get install build-essential pkg-config libgbm-dev libdrm-dev
   ```

2. Navigate to the project root:

   ```bash
   cd sommelier-rs
   ```

3. Build the workspace:

   ```bash
   cargo build --release
   ```

4. Run the proxy:

   ```bash
   target/release/sommelier wayland-0
   ```

*(Note: Depending on your environment, you may need to stop existing wayland compositors before running the proxy as `wayland-0`. Alternatively you can specify `WAYLAND_DISPLAY=wayland-proxy-0` for the application you wish to run through the proxy).*

## Design

This proxy acts as a state-tracking proxy between the Guest applications and the Host Compositor (e.g., Weston, Mutter, KWin). It handles the bridging over VM boundary together with the VMM. It runs in the guest, serving as the guest's wayland compositor facing guest wayland clients.

- **Modes of Operation:** The proxy supports a **Placeholder Proxy (Local Mode)** for debugging and testing where no VM boundary is crossed (proxying clients to a host compositor on the same OS). It also supports a **Cross-Domain Proxy (VM Mode)**, the primary operational mode, where it utilizes virtio-gpu cross domain to tunnel commands and memory across VM boundary.
- **State Management (Shadow Table):** Managing Wayland protocol state and mapping wayland object IDs, as it is required for the proxy to understand and track wayland protocol state to function. The proxy uses a one-to-one mapping between host connections and client connections to prevent a faulty client from triggering the host compositor tearing down connections for other clients.
- **Protocol Codegen:** Utilizing code generation from Wayland XML definitions to automatically handle the vast majority of Wayland protocol dispatch and ID mapping, reducing boilerplate and errors.

## Project Structure

- `sommelier/`: The main proxy executable and core logic.
  - `src/handler`: Non-default set of wayland protocol handler implementations, used when codegen is insufficient.
- `wayland_codegen/`: A build-time crate that generates Rust code from Wayland XML protocol definitions. This is not an equivalent to `wayland-scanner`.
- `third_party/protocols/`: XML files defining the Wayland core protocol and extensions used by the project.
- `docs/`: Documentations.

## Further Reading

For a detailed dive into the architecture, see `docs/ARCHITECTURE.md`.

## Other Notes

This is not an officially supported Google product. This project is not
eligible for the [Google Open Source Software Vulnerability Rewards
Program](https://bughunters.google.com/open-source-security).
