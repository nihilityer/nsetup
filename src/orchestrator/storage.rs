//! 安全路径解析、附属文件复制与原子文件写入。

use crate::config::set_mode;
use crate::constants::{COMPOSE_FILE, ENV_FILE};
use crate::template::{GeneratedFile, TemplateOutput};
use anyhow::Context;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

impl super::Orchestrator {
    /// 只在现有项目目录内更新上传资源，不替换目录也不重建容器。
    ///
    /// `--files-only` 用于同步配置文件内容：项目目录本身保持不变，运行中容器的
    /// bind mount 会立刻看到新内容。站点目录按 `--assets-mode` 语义同步，`files/`
    /// 目录与整体部署保持一致，只保留本次 `--files` 上传的内容。
    ///
    /// # 错误
    ///
    /// 项目尚未部署、路径不安全或写入失败时返回错误。
    pub(super) fn sync_files(
        &self,
        project: &str,
        generated: &TemplateOutput,
    ) -> anyhow::Result<()> {
        if generated.files.is_empty() {
            return Ok(());
        }
        let directory = self.existing_project_dir(project)?;
        // 先确认项目状态完整，避免把资源写进一个半成品目录。
        let _spec = crate::spec::StackSpec::load(&directory)?;
        validate_generated_files(&generated.files)?;
        let owned: Vec<&Path> = super::OWNED_DIRECTORIES.iter().map(Path::new).collect();
        for owned_directory in &owned {
            sync_owned_directory(&directory, owned_directory, &generated.files)?;
        }
        for file in &generated.files {
            if owned.iter().any(|owned| is_owned_file(file, owned)) {
                continue;
            }
            write_attachment(&directory, file, file.directory_mode)?;
        }
        Ok(())
    }
}

/// 判断一个附属文件是否属于某个受管目录。
///
/// 按目录分量比较，避免 `config` 把 `config/nginx` 之外的 `configx` 也算进来。
#[must_use]
pub(super) fn is_owned_file(file: &GeneratedFile, owned: &Path) -> bool {
    file.path.starts_with(owned)
}

/// 写入一个受管目录的全部附属文件，并保留声明为「不替换」的既有文件。
///
/// 受管目录在重新应用时整体重写，才能清掉上一次部署残留的陈旧文件；但 `replace`
/// 为假的上传（`--assets-mode merge`）必须保留未上传的既有文件，模板里由用户拥有的
/// 文件（例如 traefik 的 `config/dynamic/custom.yml`）也不允许被清掉。因此在重写前
/// 先把这些文件读进内存，重写后再还原，最后统一写盘。
///
/// # 错误
///
/// 目录不是普通目录、读取既有文件或写入失败时返回错误。
pub(super) fn sync_owned_directory(
    root: &Path,
    owned: &Path,
    files: &[GeneratedFile],
) -> anyhow::Result<()> {
    // 附属文件路径相对项目根（`files/x.yaml`），先折算成相对受管目录的路径，
    // 才能直接写进 `root/<受管目录>/`。
    let mut relative_files = Vec::new();
    for file in files.iter().filter(|file| is_owned_file(file, owned)) {
        relative_files.push(GeneratedFile {
            path: relative_path(&file.path, owned)?,
            ..file.clone()
        });
    }
    if relative_files.is_empty() {
        return Ok(());
    }
    let directory = root.join(owned);
    // 先把声明为「不替换」的既有文件读进内存，再决定是否清空目录。
    let preserved = stage_preserved_files(&directory, &relative_files)?;
    // 只有出现整体替换的上传时才清空目录，否则 `merge` 会丢掉既有文件。
    if relative_files.iter().any(|file| file.replace) {
        reset_owned_directory(&directory, true)?;
    }
    if !directory.exists() {
        fs::create_dir_all(&directory)
            .with_context(|| format!("无法创建受管附属目录: {}", directory.display()))?;
        set_mode(&directory, relative_files[0].directory_mode)?;
    }
    for file in preserved.iter().chain(relative_files.iter()) {
        write_attachment(&directory, file, file.directory_mode)?;
    }
    Ok(())
}

/// 剥掉受管目录前缀，得到相对该目录的路径。
///
/// # 错误
///
/// 路径不属于该受管目录时返回错误。
fn relative_path(path: &Path, owned: &Path) -> anyhow::Result<PathBuf> {
    path.strip_prefix(owned)
        .map(Path::to_path_buf)
        .map_err(|_| anyhow::anyhow!("附属文件越出受管目录: {}", path.display()))
}

/// 把受管目录中声明为「不替换」的既有文件读入内存，供目录重写后还原。
///
/// 返回的文件路径已改为相对受管目录，可以交给 [`write_attachment`] 直接写回。
/// 既有内容仍是 nsetup 播种过的旧版本（[`GeneratedFile::legacy_contents`]）时不进入
/// 保留列表，交给本次生成覆盖，旧骨架因此可以在升级后自愈。
///
/// # 错误
///
/// 既有路径不是普通文件或无法读取时返回错误。
fn stage_preserved_files(
    directory: &Path,
    relative_files: &[GeneratedFile],
) -> anyhow::Result<Vec<GeneratedFile>> {
    let mut preserved = Vec::new();
    for file in relative_files.iter().filter(|file| !file.replace) {
        let source = directory.join(&file.path);
        let metadata = match fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            anyhow::bail!("受管附属路径不是普通文件: {}", source.display());
        }
        let content = fs::read(&source)
            .with_context(|| format!("无法读取既有附属文件: {}", source.display()))?;
        if matches_legacy_content(file, &content) {
            continue;
        }
        preserved.push(GeneratedFile {
            content,
            directory_mode: metadata.permissions().mode() & 0o777,
            ..file.clone()
        });
    }
    Ok(preserved)
}

/// 判断既有内容是否与文件声明过的某个旧版本内容逐字节相同。
fn matches_legacy_content(file: &GeneratedFile, content: &[u8]) -> bool {
    file.legacy_contents.iter().any(|legacy| legacy == content)
}

/// 判断目标位置的既有内容是否仍是 nsetup 播种过的旧版本。
///
/// 文件不存在或没有声明旧版本内容时返回 `false`。
///
/// # 错误
///
/// 既有文件存在但无法读取时返回错误。
fn existing_is_legacy(path: &Path, file: &GeneratedFile) -> anyhow::Result<bool> {
    if file.legacy_contents.is_empty() {
        return Ok(false);
    }
    match fs::read(path) {
        Ok(existing) => Ok(matches_legacy_content(file, &existing)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("无法读取既有附属文件: {}", path.display()))
        }
    }
}

/// 删除一个受管目录，使其只保留本次写入的内容。
///
/// `reset` 为假时保持目录现状。
///
/// # 错误
///
/// 目标不是普通目录或删除失败时返回错误。
pub(super) fn reset_owned_directory(directory: &Path, reset: bool) -> anyhow::Result<()> {
    if !reset {
        return Ok(());
    }
    match fs::symlink_metadata(directory) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            anyhow::bail!("受管附属目录不安全: {}", directory.display());
        }
        Ok(_) => {
            fs::remove_dir_all(directory)
                .with_context(|| format!("无法替换受管附属目录: {}", directory.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

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
///
/// # 错误
///
/// 路径不是安全相对路径或出现重复时返回错误。
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
            // 附属目录要让容器内非 root 进程可以穿行，否则挂载点无法读取。
            set_mode(&destination, 0o755)?;
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
///
/// `directory_mode` 决定新建父目录的权限：模板附属文件沿用私有目录，上传给容器的
/// 资源使用 `0755`，使容器内的非 root 用户也能读取。
///
/// # 错误
///
/// 路径不安全、父目录被符号链接占用或写入失败时返回错误。
pub(super) fn write_attachment(
    root: &Path,
    file: &GeneratedFile,
    directory_mode: u32,
) -> anyhow::Result<()> {
    validate_relative_path(&file.path)?;
    let destination = root.join(&file.path);
    if !file.overwrite && destination.is_file() && !existing_is_legacy(&destination, file)? {
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("附属文件缺少父目录"))?;
    create_safe_directories(root, parent, directory_mode)?;
    if let Ok(metadata) = fs::symlink_metadata(&destination)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        anyhow::bail!("拒绝覆盖非普通文件: {}", destination.display());
    }
    write_project_file(&destination, &file.content, file.mode)
}

/// 原子替换项目内的单个受管附属文件。
///
/// 目标已存在时改为原地重写：改名会换掉 inode 属主，让容器内读取该文件的进程
/// （例如 Authelia 读取 OIDC 客户端片段）拿到一个属主不明的新文件，也会在只读
/// 挂载上留下孤儿临时文件。原地写入只改内容，属主与属组保持不变。
///
/// # 错误
///
/// 路径不安全、父目录被符号链接占用或写入失败时返回错误。
pub(super) fn replace_attachment(
    root: &Path,
    file: &GeneratedFile,
    directory_mode: u32,
) -> anyhow::Result<()> {
    validate_relative_path(&file.path)?;
    let destination = root.join(&file.path);
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("附属文件缺少父目录"))?;
    create_safe_directories(root, parent, directory_mode)?;
    let mut mode = file.mode;
    if let Ok(metadata) = fs::symlink_metadata(&destination) {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            anyhow::bail!("拒绝覆盖非普通文件: {}", destination.display());
        }
        // 只提权、不降权：容器内 entrypoint（例如 Authelia 的 `chown -R 0:0 /config`）
        // 可能已经把文件改成更严格或更宽松的属主与权限，原地重写时保留可读性。
        mode = mode.max(metadata.permissions().mode() & 0o777);
        return write_project_file(&destination, &file.content, mode);
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
fn create_safe_directories(
    root: &Path,
    destination: &Path,
    directory_mode: u32,
) -> anyhow::Result<()> {
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
                set_mode(&current, directory_mode)?;
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
    let mut file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            anyhow::anyhow!(
                "无法写入文件: {}（属主没有写权限；容器内的 entrypoint 可能已把它 chown 给别的用户，\
                 请以 root 运行 nsetup 或手工修正属主）",
                path.display()
            )
        } else {
            anyhow::Error::new(error).context(format!("无法写入文件: {}", path.display()))
        }
    })?;
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

    // 受管资源同步测试（R7）。
    use crate::config::Config;
    use crate::orchestrator::Orchestrator;
    use crate::template::{GeneratedFile, TemplateKind, TemplateOutput};
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;

    /// `--files-only` 只改内容：项目目录 inode 不变，运行中容器才不需要重建（R7）。
    #[test]
    fn files_only_sync_keeps_project_inode() -> anyhow::Result<()> {
        let root = crate::test_support::temp_directory("nsetup-files-only")?;
        let project = root.join("demo");
        std::fs::create_dir(&project)?;
        std::fs::write(
            project.join(crate::constants::COMPOSE_FILE),
            "services:\n  web:\n    image: example/web:1\n",
        )?;
        std::fs::write(project.join(crate::constants::ENV_FILE), "")?;
        std::fs::create_dir(project.join("files"))?;
        std::fs::write(project.join("files/stale.yaml"), "stale: true\n")?;
        let before = std::fs::metadata(&project)?.ino();

        let manager = Orchestrator::new(Config {
            stacks_root: root.clone(),
            ..Config::default()
        })?;
        let generated = TemplateOutput {
            spec: crate::spec::StackSpec::load(&project)?,
            files: vec![GeneratedFile {
                path: PathBuf::from("files/fresh.yaml"),
                content: b"fresh: true\n".to_vec(),
                mode: crate::template::ASSET_FILE_MODE,
                directory_mode: crate::template::ASSET_DIRECTORY_MODE,
                replace: true,
                overwrite: true,
                legacy_contents: Vec::new(),
            }],
            kind: TemplateKind::App,
        };
        manager.sync_files("demo", &generated)?;

        assert_eq!(before, std::fs::metadata(&project)?.ino());
        assert_eq!(
            std::fs::read_to_string(project.join("files/fresh.yaml"))?,
            "fresh: true\n"
        );
        assert!(
            !project.join("files/stale.yaml").exists(),
            "受管 files/ 目录应只保留本次上传的内容"
        );
        Ok(())
    }

    /// 未部署的项目不能只同步资源，否则会写出半成品目录（R7）。
    #[test]
    fn files_only_requires_deployed_project() -> anyhow::Result<()> {
        let root = crate::test_support::temp_directory("nsetup-files-only-missing")?;
        let manager = Orchestrator::new(Config {
            stacks_root: root,
            ..Config::default()
        })?;
        let generated = TemplateOutput {
            spec: crate::spec::StackSpec::parse(
                "demo",
                "services:\n  web:\n    image: example/web:1\n",
                "",
            )?,
            files: vec![GeneratedFile {
                path: PathBuf::from("files/fresh.yaml"),
                content: b"fresh: true\n".to_vec(),
                mode: crate::template::ASSET_FILE_MODE,
                directory_mode: crate::template::ASSET_DIRECTORY_MODE,
                replace: true,
                overwrite: true,
                legacy_contents: Vec::new(),
            }],
            kind: TemplateKind::App,
        };
        assert!(manager.sync_files("demo", &generated).is_err());
        Ok(())
    }

    /// 受管目录的写入语义：`replace` 清空陈旧文件，非 `replace` 保留既有内容。
    #[test]
    fn owned_directory_sync_follows_replace_semantics() -> anyhow::Result<()> {
        let root = crate::test_support::temp_directory("nsetup-reset-owned")?;
        let site = root.join("site");
        std::fs::create_dir(&site)?;
        std::fs::write(site.join("kept.html"), "kept")?;

        // merge：只覆盖同名文件，既有文件必须保留。
        let merge = vec![file("site/index.html", false)];
        super::sync_owned_directory(&root, std::path::Path::new("site"), &merge)?;
        assert!(site.join("kept.html").is_file());
        assert_eq!(
            std::fs::read_to_string(site.join("index.html"))?,
            "generated\n"
        );
        std::fs::write(site.join("stale.html"), "stale")?;

        // replace：清空目录后只写入本次上传的内容。
        let replace = vec![file("site/index.html", true)];
        super::sync_owned_directory(&root, std::path::Path::new("site"), &replace)?;
        assert!(!site.join("kept.html").exists());
        assert!(!site.join("stale.html").exists());
        assert!(site.join("index.html").is_file());

        // 用户拥有的文件（replace = false）在整体重写后必须被还原。
        let mixed = vec![
            file("config/dynamic/nsetup.yml", true),
            user_file("config/dynamic/custom.yml"),
        ];
        let dynamic = root.join("config/dynamic");
        std::fs::create_dir_all(&dynamic)?;
        std::fs::write(dynamic.join("custom.yml"), "用户自己的路由")?;
        super::sync_owned_directory(&root, std::path::Path::new("config/dynamic"), &mixed)?;
        assert_eq!(
            std::fs::read_to_string(dynamic.join("custom.yml"))?,
            "用户自己的路由"
        );
        Ok(())
    }

    /// R13：命中 nsetup 旧骨架的既有文件被换成新内容，用户改过的内容逐字节保留。
    ///
    /// 0.2.0–0.2.2 播种的 `dynamic/custom.yml` 含空映射，会让 Traefik 的 file provider
    /// 整体失败；这类文件只可能由 nsetup 自己写出，升级时必须自愈。用户动过的内容
    /// 不在 `legacy_contents` 里，仍然逐字节保留。
    #[test]
    fn legacy_content_is_replaced_but_user_content_is_preserved() -> anyhow::Result<()> {
        let root = crate::test_support::temp_directory("nsetup-legacy-content")?;
        let dynamic = root.join("config/dynamic");
        std::fs::create_dir_all(&dynamic)?;
        let owned = user_file("config/dynamic/custom.yml");
        let legacy = owned.legacy_contents[0].clone();
        let generated = owned.content.clone();
        let managed = file("config/dynamic/nsetup.yml", true);
        let files = vec![managed.clone(), owned.clone()];
        let owned_directory = std::path::Path::new("config/dynamic");

        // 旧骨架：受管目录整体重写时必须换成新内容，而不是被当作「用户文件」保留。
        std::fs::write(dynamic.join("custom.yml"), &legacy)?;
        super::sync_owned_directory(&root, owned_directory, &files)?;
        assert_eq!(std::fs::read(dynamic.join("custom.yml"))?, generated);
        assert_eq!(std::fs::read(dynamic.join("nsetup.yml"))?, managed.content);

        // 用户改过的内容：即使同一轮还会重写受管目录，也必须逐字节保留。
        let edited = "# 用户自己的路由\n".as_bytes().to_vec();
        std::fs::write(dynamic.join("custom.yml"), &edited)?;
        super::sync_owned_directory(&root, owned_directory, &files)?;
        assert_eq!(std::fs::read(dynamic.join("custom.yml"))?, edited);

        // 不走受管目录同步的写入路径（直接写入附属文件的调用方）同样要生效。
        let other = crate::test_support::temp_directory("nsetup-legacy-content-direct")?;
        let direct = user_file("custom.yml");
        std::fs::write(other.join("custom.yml"), &legacy)?;
        super::write_attachment(&other, &direct, crate::template::PRIVATE_DIRECTORY_MODE)?;
        assert_eq!(std::fs::read(other.join("custom.yml"))?, generated);
        std::fs::write(other.join("custom.yml"), &edited)?;
        super::write_attachment(&other, &direct, crate::template::PRIVATE_DIRECTORY_MODE)?;
        assert_eq!(std::fs::read(other.join("custom.yml"))?, edited);
        Ok(())
    }

    /// 构造一个受管附属文件描述。
    fn file(path: &str, replace: bool) -> GeneratedFile {
        GeneratedFile {
            path: PathBuf::from(path),
            content: b"generated\n".to_vec(),
            mode: crate::template::ASSET_FILE_MODE,
            directory_mode: crate::template::ASSET_DIRECTORY_MODE,
            replace,
            overwrite: true,
            legacy_contents: Vec::new(),
        }
    }

    /// 构造一个「用户拥有」的附属文件描述：不整体替换，也不覆盖既有内容。
    ///
    /// 同时声明一份「旧骨架」内容：命中它的既有文件允许升级为新内容。
    fn user_file(path: &str) -> GeneratedFile {
        GeneratedFile {
            content: "# 新骨架\n".as_bytes().to_vec(),
            overwrite: false,
            legacy_contents: vec!["# 旧骨架\n".as_bytes().to_vec()],
            ..file(path, false)
        }
    }
}
