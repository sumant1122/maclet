# Maclet Networking Architecture

## 1. Overview

Maclet implements a dual-tier networking architecture designed for seamless developer experience on macOS combined with true Linux container network namespace isolation inside the virtual machine.

```
+-------------------------------------------------------------------------+
|                              macOS Host                                 |
|                                                                         |
|   maclet CLI / Host Applications                                        |
|   localhost:8080 -------------------------------------------+           |
+-------------------------------------------------------------|-----------+
                                                              | NAT Port Forward
                                                              v
+-------------------------------------------------------------------------+
|                            Linux Guest VM                               |
|                                                                         |
|   +-------------------+                                                 |
|   |   eth0 (DHCP)     | <==== Apple VZ NAT (192.168.64.0/24)            |
|   +-------------------+                                                 |
|             |                                                           |
|             v                                                           |
|   +-------------------+                                                 |
|   |  maclet0 Bridge   | (10.88.0.1/16, IP forwarding, iptables NAT)     |
|   +-------------------+                                                 |
|         |           |                                                   |
|         | veth_cnt1 | veth_cnt2                                         |
|         v           v                                                   |
|   +-----------+ +-----------+                                           |
|   |Container 1| |Container 2|                                           |
|   |  netns    | |  netns    |                                           |
|   |10.88.0.2  | |10.88.0.3  |                                           |
|   |eth0 (ceth)| |eth0 (ceth)|                                           |
|   +-----------+ +-----------+                                           |
+-------------------------------------------------------------------------+
```

---

## 2. Linux VM Egress (Apple VZ NAT)

- The virtual machine is configured with a Virtio Network device backed by `VZNATNetworkDeviceAttachment`.
- At boot, the guest agent automatically detects `eth0`, brings the link up, and requests an IP lease via `udhcpc`.
- The guest VM receives an IP in the `192.168.64.0/24` subnet with default route pointing to the macOS gateway (`192.168.64.1`).
- This grants the Linux VM transparent internet access with zero external configuration.

---

## 3. Container Bridge Networking (`maclet0`)

Inside the Linux VM, `maclet-network` creates and manages a dedicated software bridge:

- **Bridge Name**: `maclet0`
- **Subnet**: `10.88.0.0/16`
- **Gateway**: `10.88.0.1`
- **IP Forwarding**: Enabled via `/proc/sys/net/ipv4/ip_forward`
- **NAT Masquerade**: `iptables -t nat -A POSTROUTING -s 10.88.0.0/16 ! -o maclet0 -j MASQUERADE`

---

## 4. Container Network Namespace Lifecycle

When a container runs with an isolated network namespace (`CLONE_NEWNET`):

1. **IPAM Allocation**: `NetworkManager::allocate_ip()` atomically reserves an IP address from the `10.88.0.0/16` pool (e.g. `10.88.0.2`, `10.88.0.3`).
2. **Virtual Ethernet Pair**:
   - Creates a veth pair: `veth_<cnt_id>` (host end) and `ceth_<cnt_id>` (peer end).
   - Attaches `veth_<cnt_id>` to the `maclet0` bridge and brings it up.
3. **Netns Migration**:
   - Migrates `ceth_<cnt_id>` into the container's network namespace:
     ```bash
     ip link set ceth_<id> netns <container_pid>
     ```
4. **Container Interface Configuration**:
   - Renames `ceth_<id>` to `eth0` inside the container.
   - Assigns allocated IP address (e.g. `10.88.0.2/16`).
   - Brings up `lo` and `eth0`.
   - Sets the default route: `ip route add default via 10.88.0.1 dev eth0`.
   - Generates `/etc/resolv.conf` with DNS nameservers `1.1.1.1` and `8.8.8.8`.
5. **Teardown**:
   - Upon container exit, the host end `veth_<id>` is unattached and deleted, reclaiming IPAM allocations and iptables rules.

---

## 5. Port Forwarding

Port mappings configured via `-p / --publish <host_port>:<container_port>` (e.g. `-p 8080:80`):
- Recorded in container metadata.
- Routed via Linux kernel DNAT:
  ```bash
  iptables -t nat -A PREROUTING -p tcp --dport 8080 -j DNAT --to-destination 10.88.0.2:80
  ```
