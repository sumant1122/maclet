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
}
