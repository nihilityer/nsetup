//! CLI 文件读取、静态资源收集与标准输出。

use crate::constants::{MAX_CONFIG_SIZE, MAX_RPC_MESSAGE_SIZE};
use crate::rpc::proto;
use anyhow::Context;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};

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
/// 遇到符号链接、特殊文件、不安全路径或超大上传时返回错误。
pub(super) fn read_assets(root: &Path) -> anyhow::Result<Vec<proto::Asset>> {
    if !root.is_dir() {
        anyhow::bail!("assets 不是目录: {}", root.display());
    }
    let mut output = Vec::new();
    read_assets_at(root, root, &mut output)?;
    output.sort_by(|left, right| left.path.cmp(&right.path));
    let total: usize = output.iter().map(|asset| asset.content.len()).sum();
    if total > MAX_RPC_MESSAGE_SIZE - MAX_CONFIG_SIZE {
        anyhow::bail!("静态站点文件总大小超过 RPC 限制");
    }
    Ok(output)
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

/// 写入导出文件且不覆盖现有路径。
///
/// # 错误
///
/// 目标已存在或无法写入时返回错误。
pub(super) fn write_new_file(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o640)
        .open(path)
        .with_context(|| format!("无法创建输出文件（不会覆盖已有文件）: {}", path.display()))?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}

/// 写入一行操作响应。
///
/// # 错误
///
/// 标准输出写入失败时返回错误。
pub(super) fn write_operation(response: proto::OperationResponse) -> anyhow::Result<()> {
    let proto::OperationResponse { message } = response;
    write_line(&message)
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

/// 为列表输出生成紧凑的单行状态。
pub(super) fn compact_status(value: &str) -> String {
    value.lines().next().unwrap_or("未知").to_string()
}
