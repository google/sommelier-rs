Architecture Design: Virtio-GPU Cross-Domain Wayland Proxy

1. Introduction & Project Scope

This document outlines the architecture for a next-generation Wayland Proxy (a "Sommelier rewrite") designed for virtio-gpu cross-domain environments.

The Goal: Allow unmodified GUI applications running inside a Guest Virtual Machine (VM) to render windows seamlessly onto the Host machine's desktop, complete with native window management, clipboard sharing, and hardware-accelerated buffer transport.

To achieve this, our proxy must act as a complex "Man-in-the-Middle" between the Guest applications and the Host Compositor (e.g., Weston, Mutter, KWin).

2. Foundational Wayland Knowledge (Why this is hard)

To understand the design choices in this document, engineers must understand critical quirks of the Wayland protocol. Wayland is not a simple drawing API; it is an asynchronous, object-oriented IPC (Inter-Process Communication) protocol.

A. Everything is an Object (with an ID)

Every entity in Wayland—a window (wl_surface), a keyboard (wl_keyboard), or even a frame callback (wl_callback)—is an object identified by a 32-bit integer ID. Messages sent over the socket always target a specific Object ID.

B. Clients Allocate IDs, Not Servers (The Collision Problem)

Unlike many client-server models where the server returns an ID (e.g., SQL auto-increment), Wayland clients allocate their own IDs for new objects.

If a Guest App wants a new surface, it tells the server: "Create a surface and assign it to ID 5."

The Problem: If Guest App A claims ID 5, and Guest App B also claims ID 5, they are both valid in their own isolated VM connections. However, if our proxy blindly forwards "Create ID 5" to the Host Compositor twice over a single connection, the Host Compositor will immediately terminate the connection for a protocol violation.

C. Heavy Reliance on File Descriptors (FDs)

Wayland does not send large pixel arrays or clipboard text over the standard socket bytes. Instead, it relies on passing POSIX File Descriptors via sendmsg and SCM_RIGHTS.

To share CPU-rendered pixels, apps use wl_shm (Shared Memory), passing an FD created via memfd_create.

The Problem: File Descriptors are specific to the kernel namespace they were created in. Guest FD 12 is meaningless to the Host Kernel. We cannot forward FDs across the VM boundary.

D. The Binary Stream Lacks Self-Description

Wayland messages over the wire do not include a standard byte length header that describes the total payload size of complex arguments. The proxy must know the exact XML signature of a message to know how many bytes to consume from the socket and which bytes represent FDs or Object IDs.

The Problem: If the proxy encounters a message from an unknown or non-standard protocol extension, it cannot determine the message boundaries. It will lose its place in the byte stream, resulting in a fatal protocol desynchronization.

3. The Architecture Stack

Our system bridges the VM boundary using a strict division of labor.

Guest Application: Standard Wayland client. Thinks it is talking to a normal desktop compositor.

Our Proxy (The Brain): Runs in the VM. Appears as a Wayland Server to the app, but acts as a Wayland Client to the Host. It parses, modifies, and translates the protocol.

Virtio-GPU (The Pipe): The kernel driver mechanism moving data across the VM boundary. Allows mapping Guest RAM to Host DMA buffers.

Crosvm / Rutabaga (The Tunnel): The Host-side VMM component. It is Wayland-Protocol-Dumb. It does not parse Wayland state; it simply patches Host FDs into the byte stream based on tags provided by our Proxy, and writes the bytes to the Host Wayland socket.

Host Compositor: The real display server rendering the screen.

4. Core Design Pillar 1: The Shadow Table (State Management)

Because of the Client-Allocated ID problem, our proxy cannot simply forward byte streams. It must be a fully stateful Wayland protocol parser.

We implement a Shadow Table (HashMap<Guest_ID, Host_ID>).

The Proxy Lifecycle:

Intercept: The Proxy intercepts every message from the Guest.

Disassemble: It parses the binary Wayland packet to find the message header and any arguments.

Map & Allocate: * When the Guest says "Create new_id 10", the Proxy asks its internal connection to the Host: "Allocate a real Host ID" (e.g., Host ID 45).

It stores 10 -> 45 in the Shadow Table.

Reassemble & Forward: The Proxy rewrites the packet, replacing 10 with 45, and forwards it to the Host.

Reverse Mapping: When the Host sends an event targeting Host ID 45, the Proxy maps it back to Guest ID 10 before passing it to the Guest App.

Justification: Without this, multiplexing multiple Guest Apps over a single Host connection is impossible.

5. Core Design Pillar 2: "Split-Brain" Protocol Transmutation

Because of the FD Wall, we cannot use the standard wl_shm protocol to talk to the Host. If we send a Guest memfd to the host, it will fail.

Our proxy operates a "Split-Brain" architecture to handle memory:

Facing the Guest (The Lie): We advertise the wl_shm global. Guest apps allocate CPU memory, draw to it, and send us the FD.

The Translation: When we receive a Guest SHM FD, we allocate a Virtio-GPU Blob Resource. This tells the hypervisor to pin that Guest RAM and expose it to the Host as a hardware-level dma-buf. We then copy the Guest's drawn pixels into this Blob Resource.

Facing the Host: We do not use wl_shm on the Host connection. Instead, we use zwp_linux_dmabuf_v1. We tell the Host Compositor: "Here is a hardware-accelerated dma-buf buffer" (even though it contains pixels drawn by the Guest CPU).

Justification: Host compositors enforce strict typing. A buffer originating from virtio-gpu will appear to the Host as a dma-buf. Standard compositors will reject attempting to import a dma-buf via the wl_shm interface. We must translate the protocol from SHM to DMABUF on the fly.

6. Core Design Pillar 3: Meta-Programming (Codegen)

The original ChromeOS Sommelier proxy was written by hand in C++. Engineers manually wrote hundreds of "trampoline" functions (sl_surface_commit, sl_keyboard_key) to perform the ID mapping. This is verbose, error-prone, and a massive source of technical debt when Wayland protocols update.

Our rewrite will utilize Code Generation.

We will write a build script (e.g., in build.rs) that consumes standard Wayland XML protocol definitions (wayland.xml, xdg-shell.xml) and outputs the proxy logic automatically.

Step 1: Automated Dispatch Generation (95% of the Code)

The codegen will read the XML signatures. If it sees an <arg type="object"> or <arg type="new_id">, it knows exactly what to do. It will generate a massive match statement that automatically handles the Shadow Table lookups.

// Conceptual Generated Code Example
match opcode {
    // Generated from: <request name="commit" />
    WL_SURFACE_COMMIT => {
        let host_id = self.shadow_table.get_host_id(packet.header.id);
        self.host_connection.send(host_id, WL_SURFACE_COMMIT, &[]);
    },
    // Generated from: <request name="create_surface"><arg name="id" type="new_id" .../></request>
    WL_COMPOSITOR_CREATE_SURFACE => {
        let guest_compositor_id = packet.header.id;
        let guest_new_surface_id = packet.read_u32();
        
        let host_compositor_id = self.shadow_table.get_host_id(guest_compositor_id);
        let host_new_surface_id = self.host_connection.allocate_id(); // The Magic
        
        self.shadow_table.insert(guest_new_surface_id, host_new_surface_id);
        
        self.host_connection.send(host_compositor_id, WL_COMPOSITOR_CREATE_SURFACE, &[Arg::Id(host_new_surface_id)]);
    }
}


Step 2: Manual Overrides (5% of the Code)

The codegen will allow us to define trait overrides for requests that deal with File Descriptors or complex memory management. Engineers will only manually write the logic for protocols like wl_shm_create_pool, zwp_linux_dmabuf_v1, and wl_data_device (clipboard).

Justification: Meta-programming eliminates boilerplate, guarantees 100% ID mapping accuracy across the entire Wayland spec, and decouples our logic from the specific versions of the Wayland protocols.

7. Implementation Roadmap & Edge Cases

When developing this architecture, the team must be aware of the following edge cases:

wl_registry.bind Tracking: Wayland uses generic new_id types during registry binding. The codegen cannot automatically know what interface is being bound. Your proxy must intercept this manually to know the type (interface) of the newly created ID, allowing you to route future messages for that ID to the correct generated dispatcher.

Global Filtering (The Safelist): Because of the binary stream limitations (Section 2.D), the proxy cannot blindly forward unknown protocols (like KDE or GNOME specific extensions). The proxy must intercept wl_registry.global events from the Host. It must check the interface name against a "safelist" of known protocols generated by the Codegen. If the interface is unknown, the proxy must silently drop the event. This prevents the Guest client from ever binding to it and sending unparseable messages.

Format Sniffing: When proxying wl_shm to zwp_linux_dmabuf_v1, the proxy must first listen to the Host's dmabuf modifier events upon connection. You must use those events to determine which pixel formats (e.g., ARGB8888) the Host GPU actually supports, and then expose only those formats to the Guest via your fake wl_shm global advertisement.

Hidden IDs: Remember that IDs aren't just in the message header. Many functions pass IDs as arguments (e.g., wl_data_device.start_drag(source, origin, icon)). The codegen parser must deeply inspect the XML arguments to patch them.