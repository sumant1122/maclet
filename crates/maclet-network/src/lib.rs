use maclet_common::Result;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(target_os = "linux")]
use maclet_common::MacletError;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::Command;

pub struct NetworkManager {
    pub bridge_name: String,
    pub gateway: Ipv4Addr,
    pub netmask: String,
    next_ip_offset: AtomicU32,
}

impl Default for NetworkManager {
    fn default() -> Self {
        Self {
            bridge_name: "maclet0".to_string(),
            gateway: Ipv4Addr::new(10, 88, 0, 1),
            netmask: "16".to_string(),
            next_ip_offset: AtomicU32::new(2),
        }
    }
}

impl NetworkManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a new sequential IPv4 address in the 10.88.0.0/16 subnet
    pub fn allocate_ip(&self) -> Ipv4Addr {
        let offset = self.next_ip_offset.fetch_add(1, Ordering::SeqCst);
        let third = (offset / 254) as u8;
        let fourth = ((offset % 254) + 1) as u8;
        Ipv4Addr::new(10, 88, third, fourth)
    }

    /// Initializes the Linux bridge, IP forwarding, and NAT masquerading
    pub fn init_host_network(&self) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            // 1. Enable IP forwarding
            let _ = fs::write("/proc/sys/net/ipv4/ip_forward", "1\n");

            // 2. Ensure bridge exists
            let bridge_exists = Path::new(&format!("/sys/class/net/{}", self.bridge_name)).exists();
            if !bridge_exists {
                let _ = Command::new("ip")
                    .args(["link", "add", "name", &self.bridge_name, "type", "bridge"])
                    .status();

                let cidr = format!("{}/{}", self.gateway, self.netmask);
                let _ = Command::new("ip")
                    .args(["addr", "add", &cidr, "dev", &self.bridge_name])
                    .status();

                let _ = Command::new("ip")
                    .args(["link", "set", &self.bridge_name, "up"])
                    .status();
            }

            // 3. Set up NAT MASQUERADE via iptables if available
            let _ = Command::new("iptables")
                .args([
                    "-t", "nat",
                    "-A", "POSTROUTING",
                    "-s", "10.88.0.0/16",
                    "!", "-o", &self.bridge_name,
                    "-j", "MASQUERADE",
                ])
                .status();
        }
        Ok(())
    }

    /// Sets up veth pair, connects one end to maclet0, and moves the other end into the container's netns
    pub fn setup_container_network(
        &self,
        container_id: &str,
        container_pid: u32,
        assigned_ip: Ipv4Addr,
    ) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let short = if container_id.len() > 8 { &container_id[..8] } else { container_id };
            let host_veth = format!("veth_{}", short);
            let peer_veth = format!("ceth_{}", short);

            // Clean up any stale interface
            let _ = Command::new("/sbin/ip").args(["link", "delete", &host_veth]).status();

            // 1. Create veth pair
            let status = Command::new("/sbin/ip")
                .args([
                    "link", "add", "name", &host_veth,
                    "type", "veth",
                    "peer", "name", &peer_veth,
                ])
                .status()
                .map_err(|e| MacletError::Guest(format!("Failed to create veth pair: {e}")))?;

            if !status.success() {
                return Err(MacletError::Guest("ip link add veth failed".to_string()));
            }

            // 2. Attach host end to bridge
            let _ = Command::new("/sbin/ip")
                .args(["link", "set", &host_veth, "master", &self.bridge_name])
                .status();
            let _ = Command::new("/sbin/ip")
                .args(["link", "set", &host_veth, "up"])
                .status();

            // 3. Move peer end into container netns
            let _ = Command::new("/sbin/ip")
                .args(["link", "set", &peer_veth, "netns", &container_pid.to_string()])
                .status();

            // 4. Configure peer interface inside container netns using nsenter
            let ip_cidr = format!("{}/{}", assigned_ip, self.netmask);
            let gw = self.gateway.to_string();
            let pid_str = container_pid.to_string();

            // Rename peer veth to eth0 inside container
            let _ = Command::new("nsenter")
                .args(["-t", &pid_str, "-n", "/sbin/ip", "link", "set", &peer_veth, "name", "eth0"])
                .status();

            // Assign IP and bring up loopback and eth0
            let _ = Command::new("nsenter")
                .args(["-t", &pid_str, "-n", "/sbin/ip", "addr", "add", &ip_cidr, "dev", "eth0"])
                .status();
            let _ = Command::new("nsenter")
                .args(["-t", &pid_str, "-n", "/sbin/ip", "link", "set", "lo", "up"])
                .status();
            let _ = Command::new("nsenter")
                .args(["-t", &pid_str, "-n", "/sbin/ip", "link", "set", "eth0", "up"])
                .status();

            // Default gateway route
            let _ = Command::new("nsenter")
                .args(["-t", &pid_str, "-n", "/sbin/ip", "route", "add", "default", "via", &gw, "dev", "eth0"])
                .status();

            // Write DNS configuration to container's /etc/resolv.conf
            let _ = fs::create_dir_all(format!("/proc/{}/root/etc", container_pid));
            let cnt_resolv = format!("/proc/{}/root/etc/resolv.conf", container_pid);
            let _ = fs::write(cnt_resolv, "nameserver 1.1.1.1\nnameserver 8.8.8.8\n");
        }
        let _ = (container_id, container_pid, assigned_ip);
        Ok(())
    }

    /// Teardown veth interface upon container exit
    pub fn teardown_container_network(&self, container_id: &str) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let short = if container_id.len() > 8 { &container_id[..8] } else { container_id };
            let host_veth = format!("veth_{}", short);
            let _ = Command::new("ip").args(["link", "delete", &host_veth]).status();
        }
        let _ = container_id;
        Ok(())
    }

    /// Port forward host port to container IP:port
    pub fn forward_port(&self, host_port: u16, container_ip: Ipv4Addr, container_port: u16) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let dest = format!("{}:{}", container_ip, container_port);
            let _ = Command::new("iptables")
                .args([
                    "-t", "nat",
                    "-A", "PREROUTING",
                    "-p", "tcp",
                    "--dport", &host_port.to_string(),
                    "-j", "DNAT",
                    "--to-destination", &dest,
                ])
                .status();
        }
        let _ = (host_port, container_ip, container_port);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ip_allocation() {
        let net = NetworkManager::new();
        let ip1 = net.allocate_ip();
        let ip2 = net.allocate_ip();
        assert_eq!(ip1, Ipv4Addr::new(10, 88, 0, 3));
        assert_eq!(ip2, Ipv4Addr::new(10, 88, 0, 4));
    }
}
