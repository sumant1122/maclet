use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Virtual Machine configuration for Maclet
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmConfig {
    pub cpu_count: usize,
    pub memory_mb: u64,
    pub kernel_path: PathBuf,
    pub initramfs_path: PathBuf,
    pub cmdline: String,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            cpu_count: 2,
            memory_mb: 1024,
            kernel_path: PathBuf::from("Image"),
            initramfs_path: PathBuf::from("initrd.img"),
            cmdline: "console=hvc0 rdinit=/init quiet".to_string(),
        }
    }
}

/// Resource limits enforced via cgroups v2
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResourceLimits {
    pub memory_bytes: Option<u64>,
    pub cpu_quota_usec: Option<u64>,
    pub cpu_period_usec: Option<u64>,
    pub pids_max: Option<u64>,
}

impl ResourceLimits {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_memory(mut self, bytes: u64) -> Self {
        self.memory_bytes = Some(bytes);
        self
    }

    pub fn with_cpus(mut self, cpus: f64) -> Self {
        let period = 100_000u64; // standard 100ms
        let quota = (cpus * period as f64).round() as u64;
        self.cpu_period_usec = Some(period);
        self.cpu_quota_usec = Some(quota);
        self
    }

    pub fn with_pids(mut self, pids: u64) -> Self {
        self.pids_max = Some(pids);
        self
    }
}

/// Resource accounting and live statistics read from cgroups v2
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CgroupMetrics {
    pub memory_current: u64,
    pub memory_max: Option<u64>,
    pub cpu_usage_usec: u64,
    pub cpu_user_usec: u64,
    pub cpu_system_usec: u64,
    pub pids_current: u64,
    pub pids_max: Option<u64>,
}

/// Helper to parse human-readable memory strings (e.g. "512m", "1g", "64k")
pub fn parse_memory_bytes(input: &str) -> Result<u64> {
    let s = input.trim().to_lowercase();
    if s.is_empty() {
        return Err(MacletError::Protocol("Empty memory string".to_string()));
    }

    let (num_str, multiplier) = if s.ends_with("kb") || s.ends_with('k') {
        let n = s.trim_end_matches("kb").trim_end_matches('k');
        (n, 1024u64)
    } else if s.ends_with("mb") || s.ends_with('m') {
        let n = s.trim_end_matches("mb").trim_end_matches('m');
        (n, 1024u64 * 1024)
    } else if s.ends_with("gb") || s.ends_with('g') {
        let n = s.trim_end_matches("gb").trim_end_matches('g');
        (n, 1024u64 * 1024 * 1024)
    } else if s.ends_with('b') {
        let n = s.trim_end_matches('b');
        (n, 1u64)
    } else {
        (s.as_str(), 1u64)
    };

    let val = num_str.parse::<u64>().map_err(|e| {
        MacletError::Protocol(format!("Invalid memory number '{num_str}': {e}"))
    })?;

    Ok(val * multiplier)
}

/// Commands and requests sent from macOS Host to Linux Guest Agent
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum HostMessage {
    #[serde(rename = "ping")]
    Ping { id: String },

    #[serde(rename = "exec")]
    Exec {
        id: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
    },

    #[serde(rename = "run_container")]
    RunContainer {
        id: String,
        #[serde(default)]
        image: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(default)]
        hostname: Option<String>,
        #[serde(default)]
        limits: Option<ResourceLimits>,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
    },

    #[serde(rename = "inspect_container")]
    InspectContainer { id: String },

    #[serde(rename = "shutdown")]
    Shutdown { id: String },
}

fn default_timeout() -> u64 {
    30
}

/// Events and responses sent from Linux Guest Agent to macOS Host
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum GuestMessage {
    #[serde(rename = "ready")]
    Ready {
        version: String,
        arch: String,
        uptime_secs: u64,
    },

    #[serde(rename = "pong")]
    Pong { id: String },

    #[serde(rename = "exec_result")]
    ExecResult {
        id: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
    },

    #[serde(rename = "container_result")]
    ContainerResult {
        id: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
        #[serde(default)]
        metrics: Option<CgroupMetrics>,
    },

    #[serde(rename = "inspect_result")]
    InspectResult {
        id: String,
        state: String,
        metrics: CgroupMetrics,
    },

    #[serde(rename = "error")]
    Error {
        id: Option<String>,
        message: String,
    },
}

#[derive(thiserror::Error, Debug)]
pub enum MacletError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("VM error: {0}")]
    Vm(String),

    #[error("Guest agent error: {0}")]
    Guest(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Operation timed out: {0}")]
    Timeout(String),
}

pub type Result<T> = std::result::Result<T, MacletError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_message_serialization() {
        let msg = HostMessage::Exec {
            id: "req-1".to_string(),
            command: "uname".to_string(),
            args: vec!["-a".to_string()],
            env: HashMap::new(),
            timeout_secs: 10,
        };
        let serialized = serde_json::to_string(&msg).unwrap();
        assert!(serialized.contains("\"type\":\"exec\""));
        assert!(serialized.contains("\"command\":\"uname\""));

        let deserialized: HostMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_guest_message_serialization() {
        let msg = GuestMessage::ExecResult {
            id: "req-1".to_string(),
            exit_code: 0,
            stdout: "Linux guest 6.6\n".to_string(),
            stderr: String::new(),
        };
        let serialized = serde_json::to_string(&msg).unwrap();
        assert!(serialized.contains("\"type\":\"exec_result\""));
        assert!(serialized.contains("\"exit_code\":0"));

        let deserialized: GuestMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_memory_parsing() {
        assert_eq!(parse_memory_bytes("512m").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_memory_bytes("1g").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_memory_bytes("64k").unwrap(), 64 * 1024);
        assert_eq!(parse_memory_bytes("1024").unwrap(), 1024);
    }

    #[test]
    fn test_resource_limits_builder() {
        let limits = ResourceLimits::new()
            .with_memory(parse_memory_bytes("256m").unwrap())
            .with_cpus(1.5)
            .with_pids(100);
        assert_eq!(limits.memory_bytes, Some(256 * 1024 * 1024));
        assert_eq!(limits.cpu_quota_usec, Some(150_000));
        assert_eq!(limits.cpu_period_usec, Some(100_000));
        assert_eq!(limits.pids_max, Some(100));
    }

    #[test]
    fn test_container_message_serialization() {
        let host_msg = HostMessage::RunContainer {
            id: "cnt-1".to_string(),
            image: "alpine".to_string(),
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "echo hi".to_string()],
            env: HashMap::new(),
            hostname: Some("mybox".to_string()),
            limits: Some(ResourceLimits::new().with_pids(50)),
            timeout_secs: 30,
        };
        let s = serde_json::to_string(&host_msg).unwrap();
        assert!(s.contains("\"type\":\"run_container\""));
        let d: HostMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(host_msg, d);

        let guest_msg = GuestMessage::ContainerResult {
            id: "cnt-1".to_string(),
            exit_code: 0,
            stdout: "hi\n".to_string(),
            stderr: String::new(),
            metrics: Some(CgroupMetrics {
                memory_current: 1024,
                memory_max: Some(512 * 1024 * 1024),
                cpu_usage_usec: 500,
                cpu_user_usec: 300,
                cpu_system_usec: 200,
                pids_current: 1,
                pids_max: Some(50),
            }),
        };
        let s2 = serde_json::to_string(&guest_msg).unwrap();
        assert!(s2.contains("\"type\":\"container_result\""));
        let d2: GuestMessage = serde_json::from_str(&s2).unwrap();
        assert_eq!(guest_msg, d2);
    }
}
