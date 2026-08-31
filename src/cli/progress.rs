//! 镜像拉取进度的交互式与稳定管道输出。

use crate::rpc::proto;
use std::io::{self, IsTerminal, Write};

/// 根据标准输出类型渲染 Docker 拉取进度。
pub(super) struct PullProgressRenderer {
    /// 是否允许 ANSI 原地刷新。
    interactive: bool,
    /// 当前是否已经显示一条临时进度。
    visible: bool,
}

impl PullProgressRenderer {
    /// 根据 stdout 是否连接终端选择输出模式。
    pub(super) fn new() -> Self {
        Self {
            interactive: io::stdout().is_terminal(),
            visible: false,
        }
    }

    /// 渲染一个拉取进度事件。
    pub(super) fn render(&mut self, progress: &proto::PullProgress) -> anyhow::Result<()> {
        if !self.interactive {
            return write_line(&plain_line(progress));
        }
        let mut stdout = io::stdout().lock();
        stdout.write_all(b"\r\x1b[2K")?;
        stdout.write_all(terminal_line(progress).as_bytes())?;
        stdout.flush()?;
        self.visible = true;
        Ok(())
    }

    /// 清除临时进度，并仅在成功时留下完成消息。
    pub(super) fn finish(&mut self, succeeded: bool) -> anyhow::Result<()> {
        if !self.interactive {
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        if self.visible {
            stdout.write_all(b"\r\x1b[2K")?;
        }
        if succeeded {
            stdout.write_all("镜像拉取完成\n".as_bytes())?;
        }
        stdout.flush()?;
        self.visible = false;
        Ok(())
    }
}

/// 保留原有的制表符分隔事件格式，供重定向和管道消费。
fn plain_line(progress: &proto::PullProgress) -> String {
    format!(
        "{}\t{}\t{}\t{}/{}",
        progress.id, progress.status, progress.text, progress.current, progress.total
    )
}

/// 生成适合单行原地刷新的可读进度。
fn terminal_line(progress: &proto::PullProgress) -> String {
    let id = if progress.id.is_empty() {
        "docker"
    } else {
        &progress.id
    };
    let text = if progress.text.is_empty() {
        &progress.status
    } else {
        &progress.text
    };
    if progress.total == 0 {
        return format!("{id:<12}  {text}");
    }
    let current = progress.current.min(progress.total);
    let percent = current.saturating_mul(100) / progress.total;
    format!(
        "{id:<12}  {text:<18} [{}] {:>7} / {:>7}  {:>3}%",
        progress_bar(current, progress.total),
        human_bytes(current),
        human_bytes(progress.total),
        percent,
    )
}

/// 构造固定宽度的 ASCII 进度条。
fn progress_bar(current: u64, total: u64) -> String {
    const WIDTH: usize = 16;
    let filled = usize::try_from(current.saturating_mul(WIDTH as u64) / total)
        .unwrap_or(WIDTH)
        .min(WIDTH);
    format!("{}{}", "=".repeat(filled), " ".repeat(WIDTH - filled))
}

/// 将字节数转换为稳定的一位小数 IEC 单位。
fn human_bytes(value: u64) -> String {
    const UNITS: &[(u64, &str)] = &[
        (1024 * 1024 * 1024, "GiB"),
        (1024 * 1024, "MiB"),
        (1024, "KiB"),
    ];
    for (unit, label) in UNITS {
        if value >= *unit {
            let whole = value / unit;
            let decimal = (value % unit).saturating_mul(10) / unit;
            return format!("{whole}.{decimal} {label}");
        }
    }
    format!("{value} B")
}

/// 写入一行稳定 stdout 数据。
fn write_line(value: &str) -> anyhow::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(value.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{human_bytes, plain_line, progress_bar, terminal_line};
    use crate::rpc::proto;

    /// 管道模式保持现有机器可读格式。
    #[test]
    fn plain_progress_format_is_stable() {
        let progress = sample_progress();
        assert_eq!(
            plain_line(&progress),
            "10a7721b2edd\tWorking\tDownloading\t5208146/50809668"
        );
    }

    /// 终端模式显示单位、百分比和固定宽度进度条。
    #[test]
    fn terminal_progress_is_compact() {
        let line = terminal_line(&sample_progress());
        assert!(line.contains("Downloading"));
        assert!(line.contains("4.9 MiB / 48.4 MiB"));
        assert!(line.contains("10%"));
        assert_eq!(progress_bar(1, 2).len(), 16);
        assert_eq!(human_bytes(1024), "1.0 KiB");
    }

    /// 返回问题中展示的代表性 Docker 进度事件。
    fn sample_progress() -> proto::PullProgress {
        proto::PullProgress {
            id: String::from("10a7721b2edd"),
            status: String::from("Working"),
            text: String::from("Downloading"),
            current: 5_208_146,
            total: 50_809_668,
        }
    }
}
