//! CLI 文件读取、静态资源收集与标准输出。

use crate::constants::{MAX_CONFIG_SIZE, MAX_RPC_MESSAGE_SIZE};
use crate::rpc::proto;
use anyhow::Context;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

/// 在严格大小限制下读取 UTF-8 文本文件。
///
/// # 错误
///
/// 元数据读取、大小检查、文件读取或 UTF-8 解码失败时返回错误。
pub(super) fn read_limited(path: &Path, limit: usize) -> anyhow::Result<String> {
    let metadata =
        std::fs::metadata(path).with_context(|| format!("无法读取文件信息: {}", path.display()))?;
    if metadata.len() > u64::try_from(limit)? {
        anyhow::bail!("文件超过 {} 字节限制: {}", limit, path.display());
    }
    std::fs::read_to_string(path).with_context(|| format!("无法读取文件: {}", path.display()))
}

/// 从静态站点目录递归加载普通文件。
///
/// # 错误
///
/// 目录为空、遇到符号链接、特殊文件、不安全路径或超大上传时返回错误。
pub(super) fn read_assets(root: &Path) -> anyhow::Result<Vec<proto::Asset>> {
    if !root.is_dir() {
        anyhow::bail!("assets 不是目录: {}", root.display());
    }
    let mut output = Vec::new();
    read_assets_at(root, root, &mut output)?;
    if output.is_empty() {
        anyhow::bail!("assets 目录为空，没有可上传的文件: {}", root.display());
    }
    output.sort_by(|left, right| left.path.cmp(&right.path));
    let total: usize = output.iter().map(|asset| asset.content.len()).sum();
    if total > MAX_RPC_MESSAGE_SIZE - MAX_CONFIG_SIZE {
        anyhow::bail!("静态站点文件总大小超过 RPC 限制");
    }
    Ok(output)
}

/// 从文件或目录参数加载 `--files` 上传内容。
///
/// 目录参数以其自身为基准展开，单个文件参数按文件名上传；重复的目标路径会被拒绝，
/// 避免同一份声明产生不确定的结果。
///
/// # 错误
///
/// 参数不存在、不是普通文件或目录、含符号链接、目标路径重复或超大时返回错误。
pub(super) fn read_files(inputs: &[PathBuf]) -> anyhow::Result<Vec<proto::Asset>> {
    let mut output = Vec::new();
    for input in inputs {
        let metadata = std::fs::symlink_metadata(input)
            .with_context(|| format!("无法读取 --files 路径: {}", input.display()))?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!("--files 不允许符号链接: {}", input.display());
        }
        if metadata.is_dir() {
            read_assets_at(input, input, &mut output)?;
        } else if metadata.is_file() {
            let name = input
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| anyhow::anyhow!("--files 文件名无效: {}", input.display()))?;
            output.push(proto::Asset {
                path: name.to_string(),
                content: std::fs::read(input)
                    .with_context(|| format!("无法读取 --files 文件: {}", input.display()))?,
            });
        } else {
            anyhow::bail!("--files 仅支持普通文件和目录: {}", input.display());
        }
    }
    output.sort_by(|left, right| left.path.cmp(&right.path));
    let mut seen = std::collections::BTreeSet::new();
    for asset in &output {
        if !seen.insert(asset.path.clone()) {
            anyhow::bail!("--files 目标路径重复: {}", asset.path);
        }
    }
    let total: usize = output.iter().map(|asset| asset.content.len()).sum();
    if total > MAX_RPC_MESSAGE_SIZE - MAX_CONFIG_SIZE {
        anyhow::bail!("--files 文件总大小超过 RPC 限制");
    }
    Ok(output)
}

/// 判断当前标准输入是否连接到终端。
#[must_use]
pub(super) fn stdin_is_terminal() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdin())
}

/// 遍历一层静态资源目录且不跟随符号链接。
///
/// # 错误
///
/// 文件系统条目不安全或类型不受支持时返回错误。
fn read_assets_at(
    root: &Path,
    directory: &Path,
    output: &mut Vec<proto::Asset>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            anyhow::bail!("assets 不允许符号链接: {}", entry.path().display());
        }
        if file_type.is_dir() {
            read_assets_at(root, &entry.path(), output)?;
        } else if file_type.is_file() {
            let relative = entry.path().strip_prefix(root)?.to_path_buf();
            if relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            {
                anyhow::bail!("asset 路径不安全: {}", relative.display());
            }
            output.push(proto::Asset {
                path: relative.to_string_lossy().replace('\\', "/"),
                content: std::fs::read(entry.path())?,
            });
        } else {
            anyhow::bail!("assets 仅允许普通文件和目录: {}", entry.path().display());
        }
    }
    Ok(())
}

/// 以仅所有者可读写权限写入导出文件且不覆盖现有路径。
///
/// # 错误
///
/// 目标已存在或无法写入时返回错误。
pub(super) fn write_new_file(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("无法创建输出文件（不会覆盖已有文件）: {}", path.display()))?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}

/// 不使用 lint 禁止的打印宏，将文本写入标准输出。
///
/// # 错误
///
/// 标准输出写入失败时返回错误。
pub(super) fn write_text(value: &str) -> anyhow::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(value.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

/// 将一个值连同换行符写入标准输出。
///
/// # 错误
///
/// 标准输出写入失败时返回错误。
pub(super) fn write_line(value: &str) -> anyhow::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(value.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

/// 将非最终操作进度写入标准错误，不污染稳定 stdout 结果。
pub(super) fn write_diagnostic(value: &str) -> anyhow::Result<()> {
    let mut stderr = io::stderr().lock();
    stderr.write_all(value.as_bytes())?;
    stderr.write_all(b"\n")?;
    stderr.flush()?;
    Ok(())
}

/// 为列表输出生成紧凑的单行状态。
pub(super) fn compact_status(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::from("stopped");
    }

    let Some(containers) = parse_compose_status(value) else {
        return value.lines().next().unwrap_or("unknown").to_string();
    };
    if containers.is_empty() {
        return String::from("stopped");
    }
    let mut counts = BTreeMap::<String, usize>::new();
    for container in containers {
        let Some(state) = container
            .get("State")
            .and_then(serde_json::Value::as_str)
            .filter(|state| !state.is_empty())
        else {
            continue;
        };
        let health = container
            .get("Health")
            .and_then(serde_json::Value::as_str)
            .filter(|health| !health.is_empty());
        let status =
            health.map_or_else(|| state.to_string(), |health| format!("{state} ({health})"));
        *counts.entry(status).or_default() += 1;
    }
    if counts.is_empty() {
        return String::from("unknown");
    }
    counts
        .into_iter()
        .map(|(status, count)| {
            if count == 1 {
                status
            } else {
                format!("{status} x{count}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// 兼容 Compose 输出的单个对象、JSON 数组和逐行 JSON 对象。
fn parse_compose_status(value: &str) -> Option<Vec<serde_json::Value>> {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(value) {
        return match parsed {
            serde_json::Value::Array(containers) => Some(containers),
            container @ serde_json::Value::Object(_) => Some(vec![container]),
            _ => None,
        };
    }
    value
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::{compact_status, write_new_file};
    use std::os::unix::fs::PermissionsExt;

    /// 含密钥的导出文件始终只允许所有者读写。
    #[test]
    fn export_file_uses_private_permissions() -> anyhow::Result<()> {
        let path = crate::test_support::temp_path("nsetup-export-test")?.with_extension("toml");
        write_new_file(&path, b"format = 1\n")?;
        let mode = std::fs::metadata(&path)?.permissions().mode() & 0o777;
        std::fs::remove_file(&path)?;
        assert_eq!(mode, 0o600);
        Ok(())
    }

    /// 列表状态忽略 Compose JSON 中体积很大的无关字段。
    #[test]
    fn list_status_keeps_only_state_and_health() {
        let status =
            r#"{"State":"running","Health":"healthy","Labels":"very-long","Mounts":"/data"}"#;
        assert_eq!(compact_status(status), "running (healthy)");
    }

    /// 多容器逐行 JSON 状态会按相同状态聚合。
    #[test]
    fn list_status_aggregates_line_delimited_containers() {
        let status = concat!(
            "{\"State\":\"running\",\"Health\":\"healthy\"}\n",
            "{\"State\":\"running\",\"Health\":\"healthy\"}\n"
        );
        assert_eq!(compact_status(status), "running (healthy) x2");
    }

    /// 没有运行容器时显示明确且紧凑的状态。
    #[test]
    fn list_status_reports_stopped_for_empty_output() {
        assert_eq!(compact_status("\n"), "stopped");
        assert_eq!(compact_status("[]"), "stopped");
    }
}
