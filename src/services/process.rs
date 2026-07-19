//! 子进程退出状态与错误输出处理。

use std::process::{ExitStatus, Output};

/// 检查带输出的子进程结果，并保留最有用的错误详情。
pub fn ensure_success(output: &Output, operation: &str) -> anyhow::Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    anyhow::bail!("{operation} 执行失败: {detail}");
}

/// 检查不捕获输出的子进程状态，并在失败时报告退出码。
pub fn ensure_status(status: ExitStatus, operation: &str) -> anyhow::Result<()> {
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("{operation} 执行失败，退出码: {:?}", status.code());
    }
}
