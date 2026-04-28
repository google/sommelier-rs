# Sommelier-rs: crosvm's Compainion Wayland Proxy

This project is a rust rewrite of the [Sommelier](https://chromium.googlesource.com/chromiumos/platform2/+/main/vm_tools/sommelier/) Wayland proxy. Its goal is to allow unmodified GUI applications running inside a virtual machine to display windows seamlessly onto the Host machine's desktop, complete with native window management and clipboard sharing. Supporting X is an explicit no-goal for this project.

When referring to this project, please use "sommelier-rs" to avoid confusion with the original sommelier.

## Quick Start

**This is the `virtwl` branch, it only works when running on a [virtio_wl](https://chromium.googlesource.com/chromiumos/third_party/kernel/+/refs/heads/chromeos-5.4/drivers/virtio/virtio_wl.c) enabled guest kernel.** virtio_wl is not part of mainline Linux kernel, and thus not supported on most distribution kernels. Examples of distribution kernels that support virtio_wl includes ChromiumOS's guest kernel such as the kernel running in Crostini / Baguette.

1. Download the newest version with "virtwl" in its name from [GitHub Releases](https://github.com/google/sommelier-rs/releases) according to your CPU architecture.

   For x86_64

   ```bash
   wget -O sommelier-rs-v0.1.1 https://github.com/google/sommelier-rs/releases/download/virtwl-v0.1.1/sommelier_rs_virtwl-v0.1.1-x86_64
   ```

   For arm64 / aarch64

   ```bash
   wget wget -O sommelier-rs-v0.1.1 https://github.com/google/sommelier-rs/releases/download/virtwl-v0.1.1/sommelier_rs_virtwl-v0.1.1-aarch64
   ```

2. (If you are running migrating from sommelier, e.g. in ChromeOS guests)

   Stop sommerlier's Wayland compositor guest interface (X interface will still be running).

   ```bash
   systemctl --user stop sommelier@0 sommelier@1
   ```

3. Give sommelier-rs permission to run

   ```bash
   chmod +x sommelier-rs-v0.1.1
   ```

4. Run sommelier-rs

   ```bash
   ./sommelier-rs-v0.1.1 --virtio-wl /dev/wl0 wayland-0
   ```

5. Run your favourite Wayland app in a separate terminal, it should automatically find and use sommelier-rs to display its windows

### Build and Run (on a Debian-compatible distro)

To build, run and develop yourself, follow these steps:

#### Prerequisites

- Rust toolchain
- A Wayland compositor running on the host passed to guest via virtwl
- Linux dependencies

#### Instructions

0. Verify virtio_wl support

   ```bash
   ls -l /dev | grep wl
   ```

1. Install dependencies

   ```bash
   sudo apt-get install build-essential pkg-config libgbm-dev libdrm-dev libxkbcommon-dev libexpat1-dev
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

*(Note: Depending on your environment, you may need to stop existing Wayland compositors such as sommelier's Wayland instances with `systemctl --user stop sommelier@0 sommelier@1`).*

## Developer Documentation

Refer to the `main` branch for developer documentation. This `virtwl` branch's main addition are located in `sommelier/src/virtwl.rs` and `sommelier/src/virtwl_channel.rs`.

## Other Notes

This is not an officially supported Google product. This project is not
eligible for the [Google Open Source Software Vulnerability Rewards
Program](https://bughunters.google.com/open-source-security).

This is also not a ChromiumOS component.
