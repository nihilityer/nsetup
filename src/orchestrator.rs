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

    if let Err(error) = docker::compose_validate(&project_directory) {
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
    Ok(())
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
