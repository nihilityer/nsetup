//! 服务级镜像与 Traefik 路由语义转换。

use super::value::validate_tagged_image;
use super::{
    DEFAULT_ENTRYPOINT, HTTPS_ENTRYPOINT, Route, RouteBinding, RouteIdentity, RouteProtocol,
    Service, split_tagged_image, validate_version,
};
use crate::constants::PROXY_NETWORK;
use anyhow::Context;
use std::collections::{BTreeMap, BTreeSet};

impl Service {
    /// 返回镜像仓库和明确版本标签。
    ///
    /// # 错误
    ///
    /// 镜像引用由变量支撑或格式无效时返回错误。
    pub fn image_version(&self) -> anyhow::Result<(String, String)> {
        if self.image.contains("${") {
            anyhow::bail!("变量镜像不能直接反解版本: {}", self.image);
        }
        split_tagged_image(&self.image)
    }

    /// 替换镜像仓库和/或版本标签。
    ///
    /// # 错误
    ///
    /// 镜像由变量支撑或新值无效时返回错误。
    pub fn set_image_version(
        &mut self,
        repository: Option<&str>,
        version: Option<&str>,
    ) -> anyhow::Result<()> {
        let (current_repository, current_version) = self.image_version()?;
        let repository = repository.unwrap_or(&current_repository);
        if repository.contains(':')
            && repository
                .rsplit('/')
                .next()
                .is_some_and(|part| part.contains(':'))
        {
            anyhow::bail!("--image 必须是不含标签的镜像仓库: {repository}");
        }
        let version = version.unwrap_or(&current_version);
        validate_version(version)?;
        let image = format!("{repository}:{version}");
        validate_tagged_image(&image)?;
        self.image = image;
        Ok(())
    }

    /// 从 Traefik label 派生由 nsetup 生成的语义路由。
    ///
    /// # 错误
    ///
    /// 生成的 label 含无效值时返回错误。
    pub fn routes(&self, stack_name: &str, service_name: &str) -> anyhow::Result<Vec<Route>> {
        let labels = label_map(&self.labels)?;
        let generated_prefix = format!("nsetup-{stack_name}-{service_name}-");
        let mut routers = BTreeSet::new();
        for key in labels.keys() {
            if let Some(rest) = key.strip_prefix("traefik.http.routers.")
                && let Some(router) = rest.strip_suffix(".rule")
                && router.starts_with(&generated_prefix)
            {
                routers.insert(router.to_string());
            }
        }
        let mut routes = Vec::new();
        for router in routers {
            let name = router
                .strip_prefix(&generated_prefix)
                .ok_or_else(|| anyhow::anyhow!("路由 {router} 不属于服务 {service_name}"))?
                .to_string();
            let prefix = format!("traefik.http.routers.{router}");
            let rule = labels
                .get(&format!("{prefix}.rule"))
                .ok_or_else(|| anyhow::anyhow!("路由 {router} 缺少 rule"))?;
            let hosts = parse_hosts(rule)?;
            let path_prefix = parse_path_prefix(rule);
            let backend = labels
                .get(&format!("{prefix}.service"))
                .cloned()
                .unwrap_or_else(|| router.clone());
            let service_prefix = format!("traefik.http.services.{backend}.loadbalancer");
            let container_port = labels
                .get(&format!("{service_prefix}.server.port"))
                .ok_or_else(|| anyhow::anyhow!("路由 {router} 缺少容器端口"))?
                .parse::<u16>()
                .with_context(|| format!("路由 {router} 容器端口无效"))?;
            let protocol = labels
                .get(&format!("{service_prefix}.server.scheme"))
                .map_or(Ok(RouteProtocol::Http), |value| RouteProtocol::parse(value))?;
            let middlewares = labels
                .get(&format!("{prefix}.middlewares"))
                .map(|value| {
                    value
                        .split(',')
                        .filter(|item| !item.is_empty())
                        .map(|item| item.strip_suffix("@file").unwrap_or(item).to_string())
                        .collect()
                })
                .unwrap_or_default();
            let entrypoint = labels
                .get(&format!("{prefix}.entrypoints"))
                .map(|value| normalize_entrypoints(value))
                .unwrap_or_else(|| String::from(DEFAULT_ENTRYPOINT));
            let sticky_cookie =
                parse_optional_bool(labels.get(&format!("{service_prefix}.sticky.cookie")))?
                    .unwrap_or(false);
            let pass_host_header =
                parse_optional_bool(labels.get(&format!("{service_prefix}.passhostheader")))?;
            let priority = labels
                .get(&format!("{prefix}.priority"))
                .map(|value| value.parse::<u32>().context("Traefik priority 无效"))
                .transpose()?;
            routes.push(Route {
                name,
                hosts,
                path_prefix,
                container_port,
                middlewares,
                protocol,
                entrypoint,
                sticky_cookie,
                pass_host_header,
                priority,
            });
        }
        Ok(routes)
    }

    /// 返回用户手写 router label 声明的路由身份。
    ///
    /// `labels` 是受支持的逃生舱，因此这里对非法规则保持宽容：无法解析出主机名的
    /// 自定义 router（例如 `HostRegexp`）不参与 host 冲突判定，而不是让整个项目
    /// 校验失败。`traefik.enable=false` 的路由不会被 Traefik 加载，同样跳过。
    ///
    /// # 错误
    ///
    /// labels 本身不是合法 `KEY=VALUE` 列表时返回错误。
    pub fn user_route_identities(&self) -> anyhow::Result<Vec<RouteIdentity>> {
        let labels = label_map(&self.labels)?;
        if labels
            .get("traefik.enable")
            .is_some_and(|value| value.trim() == "false")
        {
            return Ok(Vec::new());
        }
        let mut identities = Vec::new();
        for (key, rule) in &labels {
            let Some(rest) = key.strip_prefix("traefik.http.routers.") else {
                continue;
            };
            let Some(router) = rest.strip_suffix(".rule") else {
                continue;
            };
            if router.starts_with("nsetup-") || !rule.contains("Host(") {
                continue;
            }
            let Ok(hosts) = parse_hosts(rule) else {
                continue;
            };
            let path_prefix = parse_path_prefix(rule);
            let entrypoint = labels
                .get(&format!("traefik.http.routers.{router}.entrypoints"))
                .map_or_else(
                    || String::from(DEFAULT_ENTRYPOINT),
                    |value| normalize_entrypoints(value),
                );
            let protocol = labels
                .get(&format!(
                    "traefik.http.services.{router}.loadbalancer.server.scheme"
                ))
                .and_then(|value| RouteProtocol::parse(value).ok())
                .unwrap_or_default();
            identities.extend(hosts.into_iter().map(|host| {
                RouteIdentity::new(host, path_prefix.as_deref(), &entrypoint, protocol)
            }));
        }
        identities.sort();
        identities.dedup();
        Ok(identities)
    }

    /// 替换为此项目服务生成的全部 label，并保留用户手写的 Traefik label。
    ///
    /// # 错误
    ///
    /// 路由无效或自定义 label 键重复时返回错误。
    pub fn set_routes(
        &mut self,
        stack_name: &str,
        service_name: &str,
        routes: &[Route],
    ) -> anyhow::Result<()> {
        let generated_prefix = format!("nsetup-{stack_name}-{service_name}-");
        let mut labels = label_map(&self.labels)?;
        labels.retain(|key, _| !is_generated_traefik_key(key));
        // 用户显式声明的 `traefik.enable` 是逃生舱的一部分，必须原样保留；只有在
        // 确实声明了路由时才由 nsetup 补上该字段。
        let declared_enable =
            match labels.get("traefik.enable") {
                Some(value) => Some(value.trim().parse::<bool>().map_err(|_| {
                    anyhow::anyhow!("traefik.enable 必须是 true 或 false: {value}")
                })?),
                None => None,
            };
        if declared_enable != Some(false) && !routes.is_empty() {
            labels.insert(String::from("traefik.enable"), String::from("true"));
        }
        if !routes.is_empty() {
            // 服务同时接入多个网络时，必须固定 Traefik 走代理网络，否则容器有
            // 多个 IP，Traefik 可能选到项目网络而无法回源。
            if self.networks.len() > 1 {
                labels.insert(
                    String::from("traefik.docker.network"),
                    String::from(PROXY_NETWORK),
                );
            }
        }
        let mut route_names = BTreeSet::new();
        for route in routes {
            route.validate()?;
            if !route_names.insert(route.name.as_str()) {
                anyhow::bail!("Traefik 路由名重复: {}", route.name);
            }
            let name = format!("{generated_prefix}{}", route.name);
            let router = format!("traefik.http.routers.{name}");
            let backend = format!("traefik.http.services.{name}.loadbalancer");
            let mut rule = format!(
                "Host({})",
                route
                    .hosts
                    .iter()
                    .map(|host| format!("`{host}`"))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            if let Some(path) = &route.path_prefix {
                rule.push_str(&format!(" && PathPrefix(`{path}`)"));
            }
            labels.insert(format!("{router}.rule"), rule);
            labels.insert(
                format!("{router}.entrypoints"),
                route.entrypoint_name().to_string(),
            );
            labels.insert(format!("{router}.tls"), String::from("true"));
            labels.insert(
                format!("{router}.tls.certresolver"),
                String::from("cloudflare"),
            );
            labels.insert(format!("{router}.service"), name.clone());
            if !route.middlewares.is_empty() {
                labels.insert(
                    format!("{router}.middlewares"),
                    route
                        .middlewares
                        .iter()
                        .map(|value| format!("{value}@file"))
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
            if let Some(priority) = route.priority {
                labels.insert(format!("{router}.priority"), priority.to_string());
            }
            labels.insert(
                format!("{backend}.server.port"),
                route.container_port.to_string(),
            );
            if route.protocol != RouteProtocol::Http {
                labels.insert(
                    format!("{backend}.server.scheme"),
                    route.protocol.as_str().to_string(),
                );
            }
            if route.sticky_cookie {
                labels.insert(format!("{backend}.sticky.cookie"), String::from("true"));
            }
            if let Some(pass_host_header) = route.pass_host_header {
                labels.insert(
                    format!("{backend}.passhostheader"),
                    pass_host_header.to_string(),
                );
            }
        }
        self.labels = labels
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        Ok(())
    }
}

impl Route {
    /// 返回去掉 `@file` 后缀并附带 provider 后缀的中间件名称。
    #[must_use]
    pub fn middleware_references(&self) -> Vec<String> {
        self.middlewares
            .iter()
            .map(|name| format!("{name}@file"))
            .collect()
    }

    /// 返回此路由声明的 entrypoint；省略时使用 HTTPS 入口。
    #[must_use]
    pub fn entrypoint_name(&self) -> &str {
        if self.entrypoint.trim().is_empty() {
            HTTPS_ENTRYPOINT
        } else {
            self.entrypoint.as_str()
        }
    }

    /// 将路由展开为按主机名拆分的冲突判定身份。
    #[must_use]
    pub fn identities(&self) -> Vec<RouteIdentity> {
        self.hosts
            .iter()
            .map(|host| {
                RouteIdentity::new(
                    host.clone(),
                    self.path_prefix.as_deref(),
                    self.entrypoint_name(),
                    self.protocol,
                )
            })
            .collect()
    }

    /// 将路由转换为带归属的绑定，用于诊断与冲突报错。
    #[must_use]
    pub fn bindings(&self, project: &str, service: &str) -> Vec<RouteBinding> {
        self.identities()
            .into_iter()
            .map(|identity| RouteBinding {
                identity,
                project: project.to_string(),
                service: service.to_string(),
                router: format!("nsetup-{project}-{service}-{}", self.name),
                managed: true,
            })
            .collect()
    }
}

/// 校验列表形式的 Docker label 和重复键。
pub(super) fn validate_labels(labels: &[String]) -> anyhow::Result<()> {
    let _labels = label_map(labels)?;
    Ok(())
}

/// 将列表形式的 Docker label 解析为确定顺序的映射。
pub(super) fn label_map(labels: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut output = BTreeMap::new();
    for label in labels {
        let (key, value) = label
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Docker label 必须是 KEY=VALUE: {label}"))?;
        if key.is_empty() {
            anyhow::bail!("Docker label 键不能为空");
        }
        if output.insert(key.to_string(), value.to_string()).is_some() {
            anyhow::bail!("Docker label 重复: {key}");
        }
    }
    Ok(output)
}

/// 从 Traefik 规则提取主机名参数。
pub(super) fn parse_hosts(rule: &str) -> anyhow::Result<Vec<String>> {
    let mut hosts = Vec::new();
    let mut remaining = rule;
    while let Some(offset) = remaining.find("Host(") {
        let arguments = &remaining[offset + 5..];
        let end = arguments
            .find(')')
            .ok_or_else(|| anyhow::anyhow!("Traefik Host() 未闭合: {rule}"))?;
        hosts.extend(
            arguments[..end]
                .split(',')
                .map(|value| value.trim().trim_matches('`').to_string())
                .filter(|value| !value.is_empty()),
        );
        remaining = &arguments[end + 1..];
    }
    if hosts.is_empty() {
        anyhow::bail!("Traefik rule 缺少非空 Host(): {rule}");
    }
    Ok(hosts)
}

/// 识别由 nsetup 管理的路由器、服务与回源网络 label。
fn is_generated_traefik_key(key: &str) -> bool {
    if key == "traefik.docker.network" {
        return true;
    }
    ["traefik.http.routers.", "traefik.http.services."]
        .iter()
        .any(|prefix| {
            key.strip_prefix(prefix)
                .and_then(|rest| rest.split('.').next())
                .is_some_and(|name| name.starts_with("nsetup-"))
        })
}

/// 从生成的 Traefik 规则提取可选路径前缀。
fn parse_path_prefix(rule: &str) -> Option<String> {
    let start = rule.find("PathPrefix(`")? + 12;
    let end = rule[start..].find("`)")? + start;
    Some(rule[start..end].to_string())
}

/// 归一化 label 中的 entrypoint 列表。
fn normalize_entrypoints(value: &str) -> String {
    let mut names: Vec<&str> = value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_unstable();
    names.dedup();
    names.join(",")
}

/// 解析可选布尔 label 值。
fn parse_optional_bool(value: Option<&String>) -> anyhow::Result<Option<bool>> {
    value
        .map(|value| value.parse::<bool>().context("Traefik bool label 无效"))
        .transpose()
}
