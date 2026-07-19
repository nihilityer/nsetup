//! Compose 项目的持久化与编排逻辑。

mod container;
mod generated;
mod types;

pub use generated::{deploy_generated_stack, ensure_no_conflicts};
pub use types::{
    ContainerHealth, ContainerInfo, ContainerState, InvalidInput, StackAction, StackInfo,
    StackNotFound,
};

use crate::config::{Config, set_mode};
use crate::constants::{COMPOSE_FILE, ENV_FILE};
use crate::services::docker;
use anyhow::Context;
use container::parse_container_status;
use serde_yaml::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// 列出所有由守护进程管理的 Compose 项目
pub fn list_stacks(config: &Config) -> anyhow::Result<Vec<StackInfo>> {
    ensure_stacks_root(config)?;
    let mut names = Vec::new();
    for entry in fs::read_dir(&config.paths.apps_root).with_context(|| {
        format!(
            "无法读取 Compose 项目目录: {}",
            config.paths.apps_root.display()
        )
    })? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if !path.join(COMPOSE_FILE).is_file() || !path.join(ENV_FILE).is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str()
            && validate_stack_name(name).is_ok()
        {
            names.push(name.to_string());
        }
    }
    names.sort();
    names.iter().map(|name| get_stack(config, name)).collect()
}

/// 读取单个 Compose 项目的信息
pub fn get_stack(config: &Config, name: &str) -> anyhow::Result<StackInfo> {
    let project_directory = stack_dir(config, name)?;
    ensure_regular_stack_dir(&project_directory)?;
    let compose_file = project_directory.join(COMPOSE_FILE);
    let env_file = project_directory.join(ENV_FILE);
    if !compose_file.is_file() || !env_file.is_file() {
        return Err(StackNotFound(format!("项目 {name} 缺少 {COMPOSE_FILE} 或 {ENV_FILE}")).into());
    }

    let containers = docker::compose_ps_json(&project_directory)
        .map(|output| parse_container_status(&output))
        .unwrap_or_default();
    Ok(StackInfo {
        name: name.to_string(),
        project_directory,
        compose_file,
        env_file,
        containers,
    })
}

/// 创建或更新 Compose 项目，并可选择立即启动
pub fn deploy_stack(
    config: &Config,
    name: &str,
    compose_yaml: &str,
    env_file: &str,
    start: bool,
) -> anyhow::Result<()> {
    validate_stack_name(name)?;
    validate_compose(compose_yaml)?;
    if env_file.contains('\0') {
        return Err(InvalidInput(String::from("环境变量文件不能包含空字符")).into());
    }
    ensure_stacks_root(config)?;

    let project_directory = stack_dir(config, name)?;
    let created = !project_directory.exists();
    if created {
        fs::create_dir(&project_directory)
            .with_context(|| format!("无法创建项目目录: {}", project_directory.display()))?;
    } else {
        ensure_regular_stack_dir(&project_directory)?;
    }
    set_mode(&project_directory, 0o750)?;

    let compose_path = project_directory.join(COMPOSE_FILE);
    let env_path = project_directory.join(ENV_FILE);
    let old_compose = fs::read(&compose_path).ok();
    let old_env = fs::read(&env_path).ok();

    write_atomic(&compose_path, compose_yaml.as_bytes(), 0o640)?;
    write_atomic(&env_path, env_file.as_bytes(), 0o600)?;

    let validation =
        docker::compose_config(&project_directory).and_then(|resolved| validate_compose(&resolved));
    if let Err(error) = validation {
        restore_file(&compose_path, old_compose.as_deref(), 0o640)?;
        restore_file(&env_path, old_env.as_deref(), 0o600)?;
        if created {
            fs::remove_dir(&project_directory).with_context(|| {
                format!("无法清理无效项目目录: {}", project_directory.display())
            })?;
        }
        return Err(InvalidInput(format!("Compose 配置验证失败，已恢复原文件: {error}")).into());
    }
    if start {
        docker::compose_up(&project_directory)?;
    }
    Ok(())
}

/// 修改已存在的 Compose 项目，并在未提供环境文件时保留原内容。
pub fn update_stack(
    config: &Config,
    name: &str,
    compose_yaml: &str,
    env_file: Option<&str>,
    start: bool,
) -> anyhow::Result<()> {
    let project_directory = stack_dir(config, name)?;
    ensure_regular_stack_dir(&project_directory)?;
    let current_env = match env_file {
        Some(content) => content.to_string(),
        None => fs::read_to_string(project_directory.join(ENV_FILE))
            .with_context(|| format!("项目 {name} 缺少现有环境变量文件"))?,
    };
    deploy_stack(config, name, compose_yaml, &current_env, start)
}

/// 修改 Compose 服务的镜像版本，拉取新镜像并重新创建该服务。
pub fn upgrade_application(
    config: &Config,
    name: &str,
    service: Option<&str>,
    version: &str,
) -> anyhow::Result<(String, String)> {
    if !valid_image_version(version) {
        return Err(InvalidInput(String::from(
            "镜像版本不能使用 latest，且必须以字母、数字或下划线开头，只能包含字母、数字、点、下划线和连字符，且不超过 128 个字符",
        ))
        .into());
    }
    let project_directory = stack_dir(config, name)?;
    ensure_regular_stack_dir(&project_directory)?;
    let compose_path = project_directory.join(COMPOSE_FILE);
    let env_path = project_directory.join(ENV_FILE);
    let compose = fs::read_to_string(&compose_path)
        .with_context(|| format!("无法读取 Compose 文件: {}", compose_path.display()))?;
    let env_file = fs::read_to_string(&env_path)
        .with_context(|| format!("无法读取环境变量文件: {}", env_path.display()))?;
    let (compose, selected_service, image) = update_compose_image(&compose, service, version)?;
    deploy_stack(config, name, &compose, &env_file, false)?;
    docker::compose_pull_service(&project_directory, &selected_service)?;
    docker::compose_up_service(&project_directory, &selected_service)?;
    Ok((selected_service, image))
}

/// 判断字符串是否是有效的 Docker 镜像标签。
#[must_use]
pub fn valid_image_version(version: &str) -> bool {
    let mut bytes = version.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    !version.eq_ignore_ascii_case("latest")
        && version.len() <= 128
        && (first.is_ascii_alphanumeric() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// 在 Compose 文档中选择服务并替换镜像标签。
fn update_compose_image(
    content: &str,
    requested_service: Option<&str>,
    version: &str,
) -> anyhow::Result<(String, String, String)> {
    let mut document: Value = serde_yaml::from_str(content)
        .map_err(|error| InvalidInput(format!("Compose YAML 格式错误: {error}")))?;
    let services = document
        .as_mapping_mut()
        .and_then(|mapping| mapping.get_mut(Value::String(String::from("services"))))
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| InvalidInput(String::from("Compose YAML 必须包含 services 对象")))?;
    let selected = match requested_service {
        Some(service) => {
            validate_service_name(service)?;
            service.to_string()
        }
        None if services.len() == 1 => services
            .keys()
            .next()
            .and_then(Value::as_str)
            .ok_or_else(|| InvalidInput(String::from("Compose 服务名必须是字符串")))?
            .to_string(),
        None => {
            return Err(InvalidInput(String::from(
                "该应用包含多个服务，请使用 --service 指定要升级的 Compose 服务",
            ))
            .into());
        }
    };
    let definition = services
        .get_mut(Value::String(selected.clone()))
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| InvalidInput(format!("Compose 服务不存在: {selected}")))?;
    let image_value = definition
        .get_mut(Value::String(String::from("image")))
        .ok_or_else(|| InvalidInput(format!("Compose 服务 {selected} 没有声明 image")))?;
    let current = image_value
        .as_str()
        .ok_or_else(|| InvalidInput(format!("Compose 服务 {selected} 的 image 必须是字符串")))?;
    let repository = image_repository(current)?;
    let image = format!("{repository}:{version}");
    *image_value = Value::String(image.clone());
    let content = serde_yaml::to_string(&document).context("无法序列化更新后的 Compose 配置")?;
    Ok((content, selected, image))
}

/// 校验 Compose 服务名是否可安全传给 Docker 命令。
fn validate_service_name(service: &str) -> anyhow::Result<()> {
    let valid = !service.is_empty()
        && service.len() <= 128
        && service
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid {
        return Err(InvalidInput(String::from(
            "Compose 服务名只能包含字母、数字、点、下划线和连字符，且不超过 128 个字符",
        ))
        .into());
    }
    Ok(())
}

/// 从完整镜像引用中去掉标签或摘要并保留仓库端口。
fn image_repository(image: &str) -> anyhow::Result<&str> {
    let image = image.trim();
    if image.is_empty() || image.contains(['\n', '\r', '\0', '$']) {
        return Err(InvalidInput(String::from(
            "image 必须是明确的镜像引用，不能包含环境变量或控制字符",
        ))
        .into());
    }
    let without_digest = image
        .split_once('@')
        .map_or(image, |(repository, _)| repository);
    let last_slash = without_digest.rfind('/');
    let repository = match without_digest.rfind(':') {
        Some(colon) if last_slash.is_none_or(|slash| colon > slash) => &without_digest[..colon],
        _ => without_digest,
    };
    if repository.is_empty() {
        return Err(InvalidInput(String::from("image 缺少镜像仓库名")).into());
    }
    Ok(repository)
}

/// 停止并删除 Compose 项目配置
pub fn remove_stack(config: &Config, name: &str, remove_volumes: bool) -> anyhow::Result<()> {
    let project_directory = stack_dir(config, name)?;
    ensure_regular_stack_dir(&project_directory)?;
    docker::compose_down(&project_directory, remove_volumes)?;
    fs::remove_dir_all(&project_directory)
        .with_context(|| format!("无法删除项目目录: {}", project_directory.display()))
}

/// 获取经过校验的 Compose 项目目录
pub fn stack_dir(config: &Config, name: &str) -> anyhow::Result<PathBuf> {
    validate_stack_name(name)?;
    Ok(config.paths.apps_root.join(name))
}

/// 校验项目名，避免路径穿越并满足 Docker Compose 项目名规则
pub fn validate_stack_name(name: &str) -> anyhow::Result<()> {
    let valid_length = !name.is_empty() && name.len() <= 63;
    let valid_start = name.as_bytes().first().is_some_and(u8::is_ascii_lowercase);
    let valid_chars = name.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
    });
    if !valid_length || !valid_start || !valid_chars {
        return Err(InvalidInput(String::from(
            "项目名必须以小写字母开头，只能包含小写字母、数字、-、_，且不超过 63 个字符",
        ))
        .into());
    }
    Ok(())
}

/// 确保 Compose 根目录存在且不是符号链接
fn ensure_stacks_root(config: &Config) -> anyhow::Result<()> {
    if config.paths.apps_root.exists() {
        let metadata = fs::symlink_metadata(&config.paths.apps_root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            anyhow::bail!(
                "Compose 项目根路径必须是普通目录: {}",
                config.paths.apps_root.display()
            );
        }
    } else {
        fs::create_dir_all(&config.paths.apps_root).with_context(|| {
            format!(
                "无法创建 Compose 项目目录: {}",
                config.paths.apps_root.display()
            )
        })?;
    }
    set_mode(&config.paths.apps_root, 0o750)
}

/// 确保项目路径是普通目录而不是符号链接
fn ensure_regular_stack_dir(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        StackNotFound(format!("Compose 项目不存在 {}: {error}", path.display()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("Compose 项目路径不是普通目录: {}", path.display());
    }
    Ok(())
}

/// 对 Compose YAML 做基础结构校验
fn validate_compose(content: &str) -> anyhow::Result<()> {
    let value: Value = serde_yaml::from_str(content)
        .map_err(|error| InvalidInput(format!("Compose YAML 格式错误: {error}")))?;
    let mapping = value
        .as_mapping()
        .ok_or_else(|| InvalidInput(String::from("Compose YAML 顶层必须是对象")))?;
    let services = mapping
        .get(Value::String(String::from("services")))
        .and_then(Value::as_mapping)
        .ok_or_else(|| InvalidInput(String::from("Compose YAML 必须包含 services 对象")))?;
    if services.is_empty() {
        return Err(InvalidInput(String::from("Compose YAML 的 services 不能为空")).into());
    }
    validate_pinned_images(services)?;
    Ok(())
}

/// 要求 Compose 中声明的每个镜像都使用明确的非 `latest` 标签或摘要。
fn validate_pinned_images(services: &serde_yaml::Mapping) -> anyhow::Result<()> {
    let image_key = Value::String(String::from("image"));
    for (service_name, definition) in services {
        let name = service_name
            .as_str()
            .ok_or_else(|| InvalidInput(String::from("Compose 服务名必须是字符串")))?;
        let Some(image) = definition
            .as_mapping()
            .and_then(|mapping| mapping.get(&image_key))
        else {
            continue;
        };
        let image = image
            .as_str()
            .ok_or_else(|| InvalidInput(format!("Compose 服务 {name} 的 image 必须是字符串")))?;
        if !is_pinned_image(image) {
            return Err(InvalidInput(format!(
                "Compose 服务 {name} 必须为 image 指定明确版本，且不能使用 latest: {image}"
            ))
            .into());
        }
    }
    Ok(())
}

/// 判断镜像引用是否包含非 `latest` 标签或不可变摘要。
fn is_pinned_image(image: &str) -> bool {
    let image = image.trim();
    if image.contains('$') {
        return true;
    }
    if let Some((repository, digest)) = image.split_once('@') {
        return !repository.is_empty() && !digest.is_empty() && digest.contains(':');
    }
    let last_slash = image.rfind('/');
    let Some(colon) = image.rfind(':') else {
        return false;
    };
    if last_slash.is_some_and(|slash| colon < slash) {
        return false;
    }
    let tag = &image[colon + 1..];
    !tag.is_empty() && !tag.eq_ignore_ascii_case("latest")
}

/// 原子写入文件并设置最终权限
pub fn write_atomic(path: &Path, content: &[u8], mode: u32) -> anyhow::Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("无效文件路径: {}", path.display()))?;
    let nonce = rand::random::<u64>();
    let temporary = path.with_file_name(format!(
        ".{file_name}.tmp-{}-{nonce:016x}",
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(mode);
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("无法创建临时文件: {}", temporary.display()))?;
    set_mode(&temporary, mode)?;
    file.write_all(content)?;
    file.sync_all()?;
    set_mode(&temporary, mode)?;
    fs::rename(&temporary, path).with_context(|| format!("无法替换文件: {}", path.display()))?;
    set_mode(path, mode)
}

/// 在部署校验失败时恢复原文件
fn restore_file(path: &Path, content: Option<&[u8]>, mode: u32) -> anyhow::Result<()> {
    if let Some(content) = content {
        write_atomic(path, content, mode)
    } else if path.exists() {
        fs::remove_file(path).with_context(|| format!("无法清理文件: {}", path.display()))
    } else {
        Ok(())
    }
}
