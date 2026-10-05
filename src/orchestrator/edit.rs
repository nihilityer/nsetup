//! 服务局部编辑、标签合并与网络语义修改。

use super::operations::oidc_update_suffix;
use super::{Edit, NetworkEdit, Orchestrator};
use crate::docker;
use crate::spec::{BindMount, Document, Network, PublishedPort, Service, validate_name};
use std::collections::{BTreeMap, BTreeSet};

impl Orchestrator {
    /// 应用服务局部修改，并按需启动该服务。
    ///
    /// # 错误
    ///
    /// 修改无效时在替换状态前返回错误。
    pub fn edit(&self, name: &str, edit: Edit) -> anyhow::Result<String> {
        let mut spec = self.load(name)?;
        let service_name = select_service(&spec.document, edit.service.as_deref())?;
        let mut service = spec
            .document
            .services
            .remove(&service_name)
            .ok_or_else(|| anyhow::anyhow!("服务不存在: {service_name}"))?;
        if edit.image.is_some() || edit.version.is_some() {
            service.set_image_version(edit.image.as_deref(), edit.version.as_deref())?;
        }
        if let Some(command) = edit.command {
            service.command = command;
        }
        let mut routes = if let Some(routes) = edit.routes {
            routes
        } else {
            service.routes(name, &service_name)?
        };
        if let Some(port) = edit.container_port {
            if port == 0 {
                anyhow::bail!("容器端口不能为 0");
            }
            for route in &mut routes {
                route.container_port = Some(port);
            }
        }
        if let Some(middlewares) = edit.middlewares {
            validate_middlewares(&middlewares)?;
            for route in &mut routes {
                route.middlewares.clone_from(&middlewares);
            }
        }
        if let Some(ports) = edit.published_ports {
            service.ports = ports.iter().map(PublishedPort::compose_value).collect();
        }
        service
            .volumes
            .extend(edit.volumes.iter().map(BindMount::compose_value));
        service.environment.extend(edit.environment);
        replace_labels(&mut service.labels, &edit.labels)?;
        if edit.remove_healthcheck && edit.healthcheck.is_some() {
            anyhow::bail!("不能同时设置和移除 healthcheck");
        }
        if edit.remove_healthcheck {
            service.healthcheck = None;
        } else if edit.healthcheck.is_some() {
            service.healthcheck = edit.healthcheck;
        }
        if let Some(network) = edit.network {
            apply_network(
                &mut spec.document,
                name,
                &service_name,
                &mut service,
                network,
                !routes.is_empty(),
            )?;
        } else if service.network_mode.as_deref() == Some("host") && !routes.is_empty() {
            anyhow::bail!("host 网络模式不能使用 Traefik 容器路由");
        }
        if !routes.is_empty() {
            // 有路由的服务同时接入代理网络与项目网络，才能既被 Traefik 回源、
            // 又能访问同项目其它服务。
            crate::spec::add_proxy_network(&mut spec.document, true);
            crate::spec::add_project_network(&mut spec.document, name);
            if !service.networks.iter().any(|value| value == "proxy") {
                service.networks.push(String::from("proxy"));
            }
            if !service.networks.iter().any(|value| value == "project") {
                service.networks.push(String::from("project"));
            }
        }
        service.set_routes(name, &service_name, &routes)?;
        spec.document.services.insert(service_name.clone(), service);
        clean_unused_networks(&mut spec.document);
        spec.validate()?;
        let oidc_change = self.prepare_oidc_change(&spec)?;
        self.deploy(&spec, &[], true)?;
        let (oidc_updated, oidc_restarted) = self.apply_oidc_change(name, oidc_change, true)?;
        if edit.start {
            docker::compose_up(
                &self.config,
                &self.project_dir(name)?,
                Some(&service_name),
                true,
            )?;
        }
        Ok(format!(
            "项目 {name} 的服务 {service_name} 已更新{}",
            oidc_update_suffix(oidc_updated, oidc_restarted)
        ))
    }
}

/// 选择明确指定的服务，或选择文档中的唯一服务。
fn select_service(document: &Document, requested: Option<&str>) -> anyhow::Result<String> {
    if let Some(name) = requested {
        validate_name("服务名", name)?;
        if !document.services.contains_key(name) {
            anyhow::bail!("服务不存在: {name}");
        }
        return Ok(name.to_string());
    }
    if document.services.len() != 1 {
        anyhow::bail!("多服务项目必须使用 --service 指定服务");
    }
    document
        .services
        .keys()
        .next()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("项目没有服务"))
}

/// 按键替换自定义 label，同时保护生成的元数据。
fn replace_labels(target: &mut Vec<String>, replacements: &[String]) -> anyhow::Result<()> {
    let mut map = labels_to_map(target)?;
    for label in replacements {
        let (key, value) = label
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("label 必须是 KEY=VALUE: {label}"))?;
        if key.starts_with("traefik.http.routers.nsetup-")
            || key.starts_with("traefik.http.services.nsetup-")
            || key == "io.nsetup.template"
        {
            anyhow::bail!("不能直接修改 nsetup 生成标签: {key}");
        }
        map.insert(key.to_string(), value.to_string());
    }
    *target = map
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    Ok(())
}

/// 将列表形式的 label 解析为确定顺序的键值映射。
fn labels_to_map(values: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut output = BTreeMap::new();
    for value in values {
        let (key, content) = value
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("label 必须是 KEY=VALUE: {value}"))?;
        output.insert(key.to_string(), content.to_string());
    }
    Ok(output)
}

/// 对暂时从文档移出的服务应用一次逻辑网络修改。
fn apply_network(
    document: &mut Document,
    stack_name: &str,
    service_name: &str,
    service: &mut Service,
    network: NetworkEdit,
    has_routes: bool,
) -> anyhow::Result<()> {
    service.network_mode = None;
    service.networks.clear();
    match network {
        NetworkEdit::Bridge if has_routes => {
            crate::spec::add_proxy_network(document, true);
            crate::spec::add_project_network(document, stack_name);
            service.networks.push(String::from("proxy"));
            service.networks.push(String::from("project"));
        }
        NetworkEdit::Bridge => {}
        NetworkEdit::Host if has_routes => {
            anyhow::bail!("host 网络模式不能使用 Traefik 容器路由");
        }
        NetworkEdit::Host => service.network_mode = Some(String::from("host")),
        NetworkEdit::External(name) => {
            let key = format!("external-{service_name}");
            document.networks.insert(
                key.clone(),
                Network {
                    external: true,
                    name: Some(name),
                },
            );
            service.networks.push(key);
            if has_routes {
                crate::spec::add_proxy_network(document, true);
                crate::spec::add_project_network(document, stack_name);
                service.networks.push(String::from("proxy"));
                service.networks.push(String::from("project"));
            }
        }
    }
    Ok(())
}

/// 编辑后移除没有任何服务引用的顶层网络。
fn clean_unused_networks(document: &mut Document) {
    let used: BTreeSet<String> = document
        .services
        .values()
        .flat_map(|service| service.networks.iter().cloned())
        .collect();
    document.networks.retain(|name, _| used.contains(name));
}

/// 根据内置 Traefik 注册表检查中间件名称。
fn validate_middlewares(values: &[String]) -> anyhow::Result<()> {
    for value in values {
        if !matches!(
            value.as_str(),
            "authelia" | "gzip" | "forwarded-headers" | "internal-only" | "tls"
        ) {
            anyhow::bail!("未知内置 Traefik middleware: {value}");
        }
    }
    Ok(())
}
