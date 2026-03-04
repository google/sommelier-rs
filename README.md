# Sommelier-rs: Virtio-GPU Cross-Domain Wayland Proxy

This project is a Rust rewrite of the Sommelier Wayland proxy. Its goal is to allow unmodified GUI applications running inside a Guest Virtual Machine (VM) to render windows seamlessly onto the Host machine's desktop, complete with native window management, clipboard sharing, and hardware-accelerated buffer transport.

## Quick Start

### Prerequisites

- Rust toolchain (cargo, rustc)
- A Wayland compositor running on the host
- Linux dependencies for Wayland/GBM/DRM development

### Build and Run

1. Navigate to the project root:
   ```bash
   # cd sommelier-rs-stagging
   ```

2. Build the workspace:
   ```bash
   cargo build
   ```

3. Run the proxy:
   ```bash
   cargo run --bin sommelier
   ```

*(Note: Depending on your environment, you may need to configure specific environment variables such as `WAYLAND_DISPLAY` or setup virtio-gpu paths).*

## Architecture

This proxy acts as a complex "Man-in-the-Middle" between the Guest applications and the Host Compositor (e.g., Weston, Mutter, KWin). It handles the complexities of bridging the VM boundary, including:

*   **Modes of Operation:** The proxy supports a **Placeholder Proxy (Local Mode)** for rapid development and debugging where no VM boundary is crossed (proxying clients to a host compositor on the same OS). It also supports a **Cross-Domain Proxy (VM Mode)**, the primary operational mode, where it utilizes `virtio-gpu` to tunnel commands and memory across a true VM boundary. *(Note: If you are using this over a `virtio-wayland` virtual device, e.g., when running in a guest on ChromeOS, please refer to the `virtwl` branch).*
*   **State Management (Shadow Table):** Multiplexing multiple Wayland clients over a single host connection by mapping client-allocated object IDs to host-allocated IDs.
*   **"Split-Brain" Memory Bridging:** Bridging the file descriptor gap between Guest and Host. The proxy handles `wl_shm` by directly allocating cross-domain hardware buffers (dma-bufs) via `virtio-gpu`. These dma-bufs are passed to the host compositor as standard `wl_shm` pools (since they are mappable FDs). When the guest commits a frame, the proxy performs a CPU copy from the guest's POSIX SHM into the mapped dma-buf, handling stride and alignment differences automatically. The host compositor receives standard `wl_shm` calls for guest `wl_shm` surfaces.
*   **Protocol Codegen:** Utilizing code generation from Wayland XML definitions to automatically handle the vast majority of Wayland protocol dispatch and ID mapping, reducing boilerplate and errors.

## Project Structure

*   `sommelier/`: The main proxy executable and core logic.
    *   `src/`: Source code for the proxy, including connections, state management, and protocol handlers.
*   `wayland_codegen/`: A build-time crate that generates Rust code from Wayland XML protocol definitions.
*   `protocols/`: XML files defining the Wayland core protocol and extensions used by the project.
*   `docs/`: Architecture documentation.

## Further Reading

For a detailed dive into the architecture and the challenges this project solves, see `docs/architectures.md`.


