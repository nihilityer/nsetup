//! 单元测试共用的独立临时目录。
//!
//! 测试目录固定创建在构建目录下的 `target/test-tmp`：它位于仓库目录内，权限与普通
//! 工作目录一致，不依赖 `/tmp` 是否可写（daemon 的 systemd 沙箱会把 `/tmp` 设为只读），
//! 也不会与并行运行的其它用例互相覆盖。

use std::path::{Path, PathBuf};

/// 在构建目录下创建一个全新的空测试目录。
///
/// # 错误
///
/// 构建目录无法创建或已存在同名临时目录时返回错误。
pub fn temp_directory(label: &str) -> anyhow::Result<PathBuf> {
    let path = temp_path(label)?;
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// 在构建目录下返回一个全新且尚未创建的测试路径。
///
/// # 错误
///
/// 同名路径已被占用时返回错误。
pub fn temp_path(label: &str) -> anyhow::Result<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
    std::fs::create_dir_all(&root)?;
    let path = root.join(format!(
        "{label}-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    if path.exists() {
        anyhow::bail!("测试临时路径已存在: {}", path.display());
    }
    Ok(path)
}
