//! 安全路径解析、附属文件复制与原子文件写入。

use crate::config::set_mode;
use crate::constants::{COMPOSE_FILE, ENV_FILE};
use crate::template::GeneratedFile;
use anyhow::Context;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

/// 对可能不存在的路径，解析其最长现有前缀中的符号链接。
pub(super) fn resolve_existing_prefix(path: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("路径必须是绝对路径: {}", path.display());
    }
    let normalized = lexical_normalize(path)?;
    let mut existing = normalized.as_path();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("无法解析路径: {}", path.display()))?;
        suffix.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| anyhow::anyhow!("无法解析路径: {}", path.display()))?;
    }
    let mut resolved = existing
        .canonicalize()
        .with_context(|| format!("无法解析路径: {}", existing.display()))?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

/// 移除当前目录和父目录分量，同时禁止越出根目录。
fn lexical_normalize(path: &Path) -> anyhow::Result<PathBuf> {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => output.push(Path::new("/")),
            Component::Normal(value) => output.push(value),
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() {
                    anyhow::bail!("路径越出根目录: {}", path.display());
                }
            }
            Component::Prefix(_) => anyhow::bail!("不支持的平台路径: {}", path.display()),
        }
    }
    Ok(output)
}

/// 校验全部生成的附属文件路径并拒绝重复项。
pub(super) fn validate_generated_files(files: &[GeneratedFile]) -> anyhow::Result<()> {
    let mut paths = BTreeSet::new();
    for file in files {
        validate_relative_path(&file.path)?;
        if !paths.insert(file.path.clone()) {
            anyhow::bail!("附属文件路径重复: {}", file.path.display());
        }
    }
    Ok(())
}

/// 要求路径为非空相对路径，且仅包含普通分量。
fn validate_relative_path(path: &Path) -> anyhow::Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        anyhow::bail!("附属文件路径不安全: {}", path.display());
    }
    Ok(())
}

/// 不跟随符号链接，将不属于 IR 的项目文件复制到暂存目录。
pub(super) fn copy_auxiliary(source: &Path, target: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == COMPOSE_FILE || entry.file_name() == ENV_FILE {
            continue;
        }
        let metadata = entry.metadata()?;
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_symlink() {
            anyhow::bail!("项目附属路径不能是符号链接: {}", entry.path().display());
        }
        if metadata.is_dir() {
            fs::create_dir(&destination)?;
            set_mode(&destination, 0o750)?;
            copy_auxiliary(&entry.path(), &destination)?;
        } else if metadata.is_file() {
            fs::copy(entry.path(), &destination)?;
            set_mode(&destination, metadata.permissions().mode() & 0o777)?;
        } else {
            anyhow::bail!("项目附属路径类型不受支持: {}", entry.path().display());
        }
    }
    Ok(())
}

/// 将一个已校验的模板附属文件写入暂存目录。
pub(super) fn write_attachment(root: &Path, file: &GeneratedFile) -> anyhow::Result<()> {
    validate_relative_path(&file.path)?;
    let destination = root.join(&file.path);
    if !file.replace && destination.is_file() {
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("附属文件缺少父目录"))?;
    create_safe_directories(root, parent)?;
    if let Ok(metadata) = fs::symlink_metadata(&destination)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        anyhow::bail!("拒绝覆盖非普通文件: {}", destination.display());
    }
    write_project_file(&destination, &file.content, file.mode)
}

/// 原子替换项目内的单个受管附属文件。
pub(super) fn replace_attachment(root: &Path, file: &GeneratedFile) -> anyhow::Result<()> {
    validate_relative_path(&file.path)?;
    let destination = root.join(&file.path);
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("附属文件缺少父目录"))?;
    create_safe_directories(root, parent)?;
    if let Ok(metadata) = fs::symlink_metadata(&destination)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        anyhow::bail!("拒绝覆盖非普通文件: {}", destination.display());
    }
    let temporary = sibling_temporary(&destination, "replace")?;
    if temporary.exists() {
        anyhow::bail!("附属文件临时路径已存在: {}", temporary.display());
    }
    let result = (|| -> anyhow::Result<()> {
        write_project_file(&temporary, &file.content, file.mode)?;
        fs::rename(&temporary, &destination)?;
        Ok(())
    })();
    if result.is_err() && temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    result
}

/// 删除项目内的单个普通附属文件；文件不存在时返回 `false`。
pub(super) fn remove_attachment(root: &Path, path: &Path) -> anyhow::Result<bool> {
    validate_relative_path(path)?;
    let destination = root.join(path);
    let metadata = match fs::symlink_metadata(&destination) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        anyhow::bail!("拒绝删除非普通文件: {}", destination.display());
    }
    fs::remove_file(&destination)?;
    Ok(true)
}

/// 创建附属文件目录链，同时拒绝符号链接。
fn create_safe_directories(root: &Path, destination: &Path) -> anyhow::Result<()> {
    let relative = destination
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("附属文件越出项目目录"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                anyhow::bail!("附属目录不安全: {}", current.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                set_mode(&current, 0o750)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// 写入并同步暂存文件，然后应用明确权限。
pub(super) fn write_project_file(path: &Path, content: &[u8], mode: u32) -> anyhow::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true).mode(mode);
    let mut file = options
        .open(path)
        .with_context(|| format!("无法写入文件: {}", path.display()))?;
    file.write_all(content)?;
    file.sync_all()?;
    set_mode(path, mode)
}

/// 生成不易冲突的隐藏同级路径。
pub(super) fn sibling_temporary(target: &Path, kind: &str) -> anyhow::Result<PathBuf> {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("目标项目路径无效: {}", target.display()))?;
    Ok(target.with_file_name(format!(
        ".{name}.{kind}-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    )))
}

use std::os::unix::fs::PermissionsExt;

#[cfg(test)]
mod tests {
    use super::{lexical_normalize, validate_relative_path};
    use std::path::Path;

    #[test]
    fn rejects_asset_traversal() {
        assert!(validate_relative_path(Path::new("../secret")).is_err());
        assert!(validate_relative_path(Path::new("site/index.html")).is_ok());
    }

    #[test]
    fn normalizes_parent_components() -> anyhow::Result<()> {
        assert_eq!(
            lexical_normalize(Path::new("/srv/data/one/../two"))?,
            Path::new("/srv/data/two")
        );
        Ok(())
    }
}
