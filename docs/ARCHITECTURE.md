# Architecture Overview

This document serves as a critical, living template designed to equip agents with a rapid and comprehensive understanding of the codebase's architecture, enabling efficient navigation and effective contribution from day one. Update this document as the codebase evolves.

## 1. Project Structure

This section provides a high-level overview of the project's directory and file structure, categorized by architectural layer or major functional area. It is essential for quickly navigating the codebase, locating relevant files, and understanding the overall organization and separation of concerns.

```
[Project Root]/
├── sommelier/          # Core Wayland proxy executable
│   ├── build.rs        # Build script that invokes wayland_codegen
│   └── src/            # Main source code for the proxy
│       ├── handler/    # Handlers for specific Wayland protocols
│       │   ├── callback.rs    # wl_callback handling
│       │   ├── compositor.rs  # wl_compositor, wl_surface, wl_region
│       │   ├── data_device.rs # Copy/paste and drag-and-drop
│       │   ├── display.rs     # Core display and sync
│       │   ├── linux_dmabuf.rs# zwp_linux_dmabuf_v1 handling
│       │   ├── mod.rs         # Handler module exports
│       │   ├── registry.rs    # Protocol discovery and filtering
│       │   └── shm.rs         # wl_shm translation to cross-domain dma-bufs
│       ├── allocator.rs  # GBM buffer allocator for host zero-copy
│       ├── connection.rs # Async Wayland socket communication
│       ├── main.rs       # Entry point and CLI argument parsing
│       ├── proxy.rs      # Main proxy event loop and message routing (tokio tasks)
│       ├── state.rs      # Global state, context, and ID Shadow Table
│       ├── virtgpu_channel.rs # virtio-gpu communication, actor model, and dma-buf mapping
│       └── wire.rs       # Wayland wire protocol message parsing and building
├── wayland_codegen/    # Build-time crate for generating Rust code from Wayland XML
│   ├── src/
│   │   ├── generator.rs # Core code generation logic
│   │   ├── lib.rs       # Library entry point
│   │   ├── protocol.rs  # Wayland XML parser structures
│   │   └── test_parsing.rs # Unit tests for XML parsing
├── protocols/          # XML files defining the Wayland core and extensions
├── plans/              # Detailed task plans and design notes
├── docs/               # Project documentation
│   └── architectures.md # This document
├── .gitignore          # Specifies untracked files
├── Cargo.toml          # Workspace configuration
└── README.md           # Project overview and quick start guide
```

## 2. High-Level System Diagram

The Sommelier-rs proxy sits between Wayland applications running in an isolated Guest VM and the Wayland compositor running on the Host system. It bridges the gap by translating proxy IDs and managing hardware-backed memory across the VM boundary.

```text
[Guest VM]                                       [Host System]
+-------------------+                            +-------------------------+
| Guest Application |                            | Host Wayland Compositor |
| (Wayland Client)  |                            | (Weston, Mutter, KWin)  |
+---------+---------+                            +------------+------------+
          | (Standard Wayland Socket)                         ^ (Wayland Socket)
          v                                                   |
+------------------------------------------+                  |
|          Sommelier-rs Proxy              |                  |
|                                          |                  |
|  1. Intercepts Guest Wayland Messages    |                  |
|  2. Translates IDs (Shadow Table)        |<-----------------+
|  3. Transmutes Memory (dma-buf backed    |  (Via Crosvm / Rutabaga Tunnel)
|     wl_shm mapping with CPU copy)        |                  ^
|                                          |                  |
+--------------------+---------------------+                  |
                     | (Virtio-GPU ioctls)                    |
                     v                                        v
+------------------------------------------+     +-------------------------+
| Guest Kernel (virtio-gpu driver)         |---->| Host Kernel (DRM/KMS)   |
| (Allocates dma-bufs mapped to Host)      |     | (Hardware dma-bufs)     |
+------------------------------------------+     +-------------------------+
```

### 2.1. Modes of Operation

The Sommelier-rs proxy supports two distinct modes of operation depending on the deployment environment:

1.  **Placeholder Proxy (Local Mode):** In this mode, the proxy sits between a Wayland client and a Wayland compositor running on the *same* OS kernel. No VM boundary is crossed. This mode is primarily used for rapid development, testing, and protocol debugging. It allows developers to verify the core ID translation (Shadow Table) logic and basic protocol handling without the complexity and overhead of a virtual machine or the `virtio-gpu` driver.
2.  **Cross-Domain Proxy (VM Mode):** This is the real operational mode where a VM boundary is being crossed. The proxy runs inside a Guest VM and communicates with a Host Compositor. In this mode, it relies heavily on `virtio-gpu` to allocate cross-domain hardware-backed memory (dma-bufs) and tunnel standard Wayland sockets and File Descriptors (FDs) across the boundary, enabling seamless host integration and rendering. *(Note: If you are operating over a `virtio-wayland` virtual device, e.g., as a guest on ChromeOS, please refer to the `virtwl` branch for the relevant implementation details).*

## 3. Core Components

### 3.1. Sommelier Proxy (`sommelier` crate)

*   **Name:** Sommelier-rs Wayland Proxy
*   **Description:** The core executable running inside the Guest VM. It acts as a Wayland server to guest applications and a Wayland client to the host compositor. It parses the binary Wayland protocol using `wire.rs` and maintains state to prevent ID collisions (Shadow Table in `state.rs`). Crucially, it replaces standard POSIX Shared Memory FDs (`wl_shm` from the guest) with mappable cross-domain hardware buffers (`virtio-gpu` dma-bufs) passed to the host's `wl_shm`.
*   **Technologies:** Rust, `tokio` (async runtime and actors), `nix` (syscalls), `gbm`/`drm` (graphics buffer management via `allocator.rs`).
*   **Deployment:** Runs as a background daemon within Linux Guest VMs (e.g., ChromeOS Crostini, WSLg).

### 3.2. Wayland Code Generator (`wayland_codegen` crate)

*   **Name:** Wayland Protocol Codegen
*   **Description:** A build-time dependency that parses standard Wayland XML protocol definitions and generates the vast majority of the proxy's boilerplate code. It generates massive match statements for dispatching messages, automatically handling the lookup and translation of Wayland Object IDs via the Shadow Table.
*   **Technologies:** Rust, `quick-xml`.
*   **Deployment:** Runs during `cargo build` in the `sommelier` crate's `build.rs`.

## 4. Split-Brain Memory Transmutation

Sommelier-rs operates on a "Split-Brain" architecture to handle standard memory allocation across the VM boundary.

### 4.1. Memory Transmutation (`wl_shm`)

The proxy handles memory type transmutation while retaining the standard `wl_shm` protocol:
1. **Interception:** Guest applications allocate POSIX Shared Memory (memfds) and send them via `wl_shm.create_pool` to the proxy.
2. **Hardware Allocation:** The proxy intercepts this call and allocates a hardware-backed `dma-buf` buffer via `virtio-gpu` (with `MAPPABLE` and `SHAREABLE` flags, or via the `gbm` allocator as a fallback).
3. **Mappable FD Transmission:** Because the `virtio-gpu` dma-buf is mappable, the proxy treats it as a standard SHM pool. It duplicates the `dma-buf` FD and sends it to the Host Compositor using standard `wl_shm.create_pool`. The Host Compositor uses `wl_shm` entirely unaware that it is receiving a hardware-backed `dma-buf`.
4. **Dynamic CPU Copy:** When the guest issues a `wl_surface.commit`, the proxy performs a synchronous, row-by-row CPU memory copy from the guest's POSIX SHM into the mapped `dma-buf` memory region. This correctly accounts for stride or alignment differences (e.g., hardware 64-pixel alignments).

This design ensures the host compositor continues receiving standard `wl_shm` calls rather than complex dmabuf calls for software-rendered applications, minimizing compatibility issues across different compositors.

### 4.2. File Descriptor Bridging (`virtgpu_channel.rs`)

File descriptors (FDs) inherently cannot cross VM namespace boundaries. The Sommelier-rs proxy intercepts standard Wayland messages over Unix Sockets and transports FDs across the boundary via Virtio-GPU commands (`CROSS_DOMAIN_CMD_SEND`, `CROSS_DOMAIN_CMD_RECEIVE`, `CROSS_DOMAIN_CMD_WRITE`). The proxy inspects FDs: if it is a dma-buf, it creates a corresponding `virtio-gpu` blob handle to send; if it is a pipe (e.g., for clipboard copy/paste), it creates a cross-domain pipe representation to tunnel the stream data.

## 5. External Integrations / APIs

*   **Service Name:** Host Wayland Compositor
    *   **Purpose:** The actual display server rendering the desktop on the host machine.
    *   **Integration Method:** Wayland UNIX Domain Socket (proxied across the VM boundary by a VMM like Crosvm/Rutabaga). The physical transmission utilizes `virtgpu_channel.rs` for command wrapping and FD passing.
*   **Service Name:** Virtio-GPU (Guest Kernel Driver)
    *   **Purpose:** Allocates memory that is mapped across the VM boundary to the Host. Also handles Wayland message transport (`CROSS_DOMAIN_CMD_SEND` / `RECEIVE`).
    *   **Integration Method:** DRM (Direct Rendering Manager) ioctls via `/dev/dri/renderD*` device nodes. `virtgpu_channel.rs` wraps this logic heavily, creating a context ring and polling events asynchronously via `tokio` channels and actors.

## 6. Deployment & Infrastructure

*   **Target Environment:** Linux Virtual Machines (Guest OS).
*   **Key Services Used:** Wayland, DRM/KMS, virtio-gpu.
*   **CI/CD Pipeline:** Expected standard Rust `cargo test` and `cargo fmt`/`clippy` in GitHub Actions.

## 7. Security Considerations

*   **Protocol Filtering (Safelist):** The proxy intercepts `wl_registry.global` events from the host. It only forwards known, supported protocols to the guest. This prevents guest applications from binding to host-specific, proprietary, or unparseable extensions which could desynchronize the proxy.
*   **Namespace Isolation:** Because standard FDs are meaningless outside their origin kernel namespace, the proxy intercepts and safely translates memory sharing requests (memfds) into secure cross-domain `dma-bufs` via `virtio-gpu`.

## 8. Development & Testing Environment

*   **Local Setup Instructions:** See `README.md` Quick Start. Requires a standard Rust toolchain and Linux Wayland/GBM/DRM development headers.
*   **Testing Frameworks:** standard `cargo test`.
*   **Code Quality Tools:** `cargo clippy`, `cargo fmt`.

## 9. Future Considerations / Roadmap

*   **Stabilize full async/await architecture:** Refactoring is underway utilizing `tokio` for connection dispatch (`proxy.rs`) and virtio-gpu actors (`virtgpu_channel.rs`). Further stabilization is needed for complex memory mapped states and robust handling.
*   **EGL/Vulkan Integration:** Ensuring the proxy can handle not just software-rendered `wl_shm`, but also fully hardware-accelerated guest applications using EGL or Vulkan natively over `virtio-gpu` without CPU copy overhead.

## 10. Project Identification

*   **Project Name:** Sommelier-rs
*   **Repository URL:** [Insert Repository URL]
*   **Primary Contact/Team:** [Insert Lead Developer/Team Name]
*   **Date of Last Update:** 2024-05-14

## 11. Glossary / Acronyms

*   **FD:** File Descriptor. A POSIX standard for referring to an open file or socket.
*   **VM:** Virtual Machine.
*   **VMM:** Virtual Machine Monitor (e.g., Crosvm, QEMU).
*   **dma-buf:** A Linux kernel subsystem for sharing buffers for hardware (DMA) access across multiple device drivers.
*   **Virtio-GPU:** A virtualization standard for exposing a virtual GPU to a guest OS.
*   **DRM:** Direct Rendering Manager. A subsystem of the Linux kernel responsible for interfacing with GPUs.
*   **GBM:** Generic Buffer Management. An API that provides a mechanism for allocating buffers for graphics rendering tied to Mesa.
