# Sommelier-rs: crosvm's Compainion Wayland Proxy

This project is a rust rewrite of the [Sommelier](https://chromium.googlesource.com/chromiumos/platform2/+/main/vm_tools/sommelier/) Wayland proxy. Its goal is to allow unmodified GUI applications running inside a virtual machine to display windows seamlessly onto the Host machine's desktop, complete with native window management and clipboard sharing. Supporting X is an explicit no-goal for this project.

When referring to this project, please use "sommelier-rs" to avoid confusion with the original sommelier.

## Quick Start

**This is the `virtwl` branch, it only works when running on a [virtio_wl](https://chromium.googlesource.com/chromiumos/third_party/kernel/+/refs/heads/chromeos-5.4/drivers/virtio/virtio_wl.c) enabled guest kernel.** virtio_wl is not part of mainline Linux kernel, and thus not supported on most distribution kernels. Examples of distribution kernels that support virtio_wl includes ChromiumOS's guest kernel such as the kernel running in Crostini / Baguette.

### Prerequisites

- Rust toolchain
- A Wayland compositor running on the host
- Linux dependencies

### Build and Run (on a Debian-compatible distro)

0. Verify virtio_wl support

   ```bash
   ls -l /dev | grep wl
   ```

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
   target/release/sommelier --virtio-wl /dev/wl0 wayland-0
   ```

*(Note: Depending on your environment, you may need to stop existing wayland compositors such as sommelier's wayland instances with `systemctl --user stop sommelier@0 sommelier@1`).*

## Developer Documentation

Refer to the `main` branch for developer documentation. This `virtwl` branch's main addition are located in `sommelier/src/virtwl.rs` and `sommelier/src/virtwl_channel.rs`.

## Other Notes

This is not an officially supported Google product. This project is not
eligible for the [Google Open Source Software Vulnerability Rewards
Program](https://bughunters.google.com/open-source-security).

This is also not a ChromiumOS component.
