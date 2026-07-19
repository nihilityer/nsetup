//! systemd 服务控制命令。
//!
//! 服务可以由系统包或单文件初始化安装；本模块只调用 `systemctl`
//! 管理已经安装的服务。

use crate::services::process;
use anyhow::Context;
use std::process::Command;

/// systemd 服务名称。
pub const SERVICE_NAME: &str = "nsetup.service";

/// 支持的 systemd 服务操作。
#[derive(Debug, Clone, Copy)]
pub enum ServiceAction {
    /// 启动服务。
    Start,
    /// 停止服务。
    Stop,
    /// 重启服务。
    Restart,
    /// 显示完整状态。
    Status,
}

/// 管理已安装的服务。
pub fn control(action: ServiceAction) -> anyhow::Result<()> {
    ensure_systemd()?;
    let args: &[&str] = match action {
        ServiceAction::Start => &["start", SERVICE_NAME],
        ServiceAction::Stop => &["stop", SERVICE_NAME],
        ServiceAction::Restart => &["restart", SERVICE_NAME],
        ServiceAction::Status => &["--no-pager", "--full", "status", SERVICE_NAME],
    };
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .with_context(|| format!("无法执行 systemctl {}", args.join(" ")))?;
    if matches!(action, ServiceAction::Status) && status.code() == Some(3) {
        Ok(())
    } else {
        process::ensure_status(status, &format!("systemctl {}", args.join(" ")))
    }
}

/// 确保当前系统由 systemd 管理。
fn ensure_systemd() -> anyhow::Result<()> {
    let output = Command::new("systemctl")
        .arg("--version")
        .output()
        .context("系统未安装 systemctl，无法管理 nsetup 服务")?;
    process::ensure_success(&output, "systemctl --version")
}
