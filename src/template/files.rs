//! `files/` 附属目录的挂载入口。
//!
//! 没有它时，唯一能放非 IR 文件的 bind mount 只能是 `data_roots` 下的路径，
//! 而这些路径属于 root，普通用户无法写入，于是 netbird 的 `config.yaml`、matrix
//! 的 `tuwunel.toml` 一类文件只能借助 sudo 或一次性特权容器写入。`files/` 由
//! daemon 在受管项目目录内创建并拷贝，因此可以在普通用户下完成日常部署。

use crate::spec::{BindMount, StackSpec};

/// `files/` 附属目录在受管项目目录中的相对路径。
pub const FILES_DIRECTORY: &str = "files";
/// `files/` 目录在容器中的默认挂载目标。
pub const DEFAULT_FILES_TARGET: &str = "/opt/nsetup/files";

/// 将 `files/` 目录以只读方式挂进项目中的每个服务。
///
/// 挂载源固定在项目目录内，因此不会绕过 daemon 的 bind mount 白名单；目标路径由
/// `--files-into` 提供，省略时为 [`DEFAULT_FILES_TARGET`]。
///
/// # 错误
///
/// 目标不是绝对路径、不是目录，或与既有挂载冲突时返回错误。
pub fn bind_directory(
    spec: &mut StackSpec,
    project_directory: &std::path::Path,
    target: &str,
) -> anyhow::Result<()> {
    if !project_directory.is_absolute() {
        anyhow::bail!("项目目录必须是绝对路径: {}", project_directory.display());
    }
    let container_path = target.to_string();
    if !std::path::Path::new(&container_path).is_absolute() {
        anyhow::bail!("--files-into 必须是容器内绝对路径: {target}");
    }
    if container_path == "/" {
        anyhow::bail!("--files-into 不能是容器根目录");
    }
    if spec.document.services.is_empty() {
        anyhow::bail!("项目至少需要一个服务才能挂载 files/ 目录");
    }
    for (name, service) in &mut spec.document.services {
        if service
            .volumes
            .iter()
            .filter_map(|value| BindMount::parse(value).ok())
            .any(|existing| existing.container_path == container_path)
        {
            anyhow::bail!("服务 {name} 已经挂载了 {container_path}");
        }
        service.volumes.push(format!(
            "{}/{FILES_DIRECTORY}:{container_path}:ro",
            project_directory.display()
        ));
    }
    Ok(())
}
