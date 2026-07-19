//! 生成项目的安全部署与冲突检测。

use super::{InvalidInput, deploy_stack, stack_dir, write_atomic};
use crate::config::{Config, set_mode};
use crate::generator::{GeneratedFile, GeneratedStack, PortProtocol, PublishedPort, Route};
use crate::services::docker;
use anyhow::Context;
use serde_yaml::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};

/// 部署生成的 Compose 项目及其附属文件。
pub fn deploy_generated_stack(
    config: &Config,
    generated: &GeneratedStack,
    force: bool,
    start: bool,
) -> anyhow::Result<()> {
    let directory = stack_dir(config, &generated.name)?;
    if directory.exists() && !force {
        return Err(InvalidInput(format!(
            "项目 {} 已存在；确认覆盖请使用 --force",
            generated.name
        ))
        .into());
    }
    deploy_stack(
        config,
        &generated.name,
        &generated.compose_yaml,
        &generated.env_file,
        false,
    )?;
    for file in &generated.files {
        write_generated_file(&directory, file)?;
    }
    if start {
        docker::compose_up(&directory)?;
    }
    Ok(())
}

/// 检查请求中的域名和宿主机端口是否与已有项目冲突。
pub fn ensure_no_conflicts(
    config: &Config,
    project: &str,
    routes: &[Route],
    ports: &[PublishedPort],
) -> anyhow::Result<()> {
    let requested_hosts: BTreeSet<&str> = routes.iter().map(|route| route.host.as_str()).collect();
    let requested_ports: BTreeSet<(u16, PortProtocol)> = ports
        .iter()
        .map(|port| (port.host_port, port.protocol))
        .collect();
    if requested_hosts.len() != routes.len() {
        return Err(InvalidInput(String::from("同一应用不能重复声明域名")).into());
    }
    if requested_ports.len() != ports.len() {
        return Err(InvalidInput(String::from("同一应用不能重复发布宿主机端口")).into());
    }
    if !config.paths.apps_root.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&config.paths.apps_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || entry.file_name() == project {
            continue;
        }
        let compose_path = entry.path().join(crate::constants::COMPOSE_FILE);
        let Ok(content) = fs::read_to_string(&compose_path) else {
            continue;
        };
        let Ok(document) = serde_yaml::from_str::<Value>(&content) else {
            continue;
        };
        for host in existing_hosts(&document) {
            if requested_hosts.contains(host.as_str()) {
                return Err(InvalidInput(format!(
                    "域名 {host} 已被项目 {} 使用",
                    entry.file_name().to_string_lossy()
                ))
                .into());
            }
        }
        for (port, protocol) in existing_ports(&document) {
            if requested_ports.contains(&(port, protocol)) {
                return Err(InvalidInput(format!(
                    "宿主机端口 {port}/{} 已被项目 {} 使用",
                    protocol.compose_suffix(),
                    entry.file_name().to_string_lossy()
                ))
                .into());
            }
        }
    }
    Ok(())
}

/// 在项目范围内安全写入生成的附属文件。
fn write_generated_file(root: &Path, file: &GeneratedFile) -> anyhow::Result<()> {
    if file.path.as_os_str().is_empty()
        || file
            .path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(InvalidInput(format!("附属文件路径不安全: {}", file.path.display())).into());
    }
    let destination = root.join(&file.path);
    let parent = destination
        .parent()
        .ok_or_else(|| InvalidInput(String::from("附属文件缺少父目录")))?;
    ensure_directory_chain(root, parent)?;
    if let Ok(metadata) = fs::symlink_metadata(&destination)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(InvalidInput(format!("拒绝覆盖非普通文件: {}", destination.display())).into());
    }
    write_atomic(&destination, &file.content, file.mode)
}

/// 创建父目录并拒绝路径链中的符号链接。
fn ensure_directory_chain(root: &Path, destination: &Path) -> anyhow::Result<()> {
    let relative = destination
        .strip_prefix(root)
        .map_err(|_| InvalidInput(String::from("附属文件目录越出项目范围")))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(
                    InvalidInput(format!("目录不是普通目录: {}", current.display())).into(),
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .with_context(|| format!("无法创建目录: {}", current.display()))?;
                set_mode(&current, 0o750)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// 提取现有 Compose 文档中的 Traefik 域名。
fn existing_hosts(document: &Value) -> Vec<String> {
    service_strings(document, "labels")
        .filter_map(|label| {
            let start = label.find("Host(`")? + 6;
            let end = label[start..].find("`)")? + start;
            Some(label[start..end].to_string())
        })
        .collect()
}

/// 提取现有 Compose 文档发布的宿主机端口。
fn existing_ports(document: &Value) -> Vec<(u16, PortProtocol)> {
    service_strings(document, "ports")
        .filter_map(|mapping| {
            let (mapping, protocol) = mapping.rsplit_once('/').map_or(
                (mapping, PortProtocol::Tcp),
                |(mapping, protocol)| {
                    let protocol = match protocol {
                        "udp" => PortProtocol::Udp,
                        _ => PortProtocol::Tcp,
                    };
                    (mapping, protocol)
                },
            );
            Some((mapping.split(':').next()?.parse().ok()?, protocol))
        })
        .collect()
}

/// 遍历全部 Compose 服务中指定数组字段的字符串值。
fn service_strings<'a>(document: &'a Value, field: &str) -> impl Iterator<Item = &'a str> {
    document
        .get("services")
        .and_then(Value::as_mapping)
        .into_iter()
        .flat_map(|services| services.values())
        .filter_map(move |service| service.get(field).and_then(Value::as_sequence))
        .flatten()
        .filter_map(Value::as_str)
}
