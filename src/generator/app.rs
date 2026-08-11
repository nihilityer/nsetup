//! 常规应用与静态站点生成。

use super::compose::{self, Document, Network, Service};
use super::{
    AppSpec, GeneratedFile, GeneratedStack, Middleware, NetworkMode, StaticSiteSpec, Volume,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Traefik 外部网络名。
const TRAEFIK_NETWORK: &str = "nihility-traefik";

/// 生成常规单服务应用。
pub fn generate_application(spec: &AppSpec) -> anyhow::Result<GeneratedStack> {
    validate_app(spec)?;
    let mut document = Document::default();
    let environment = spec.environment.clone();
    let mut service = Service {
        image: format!("{}:{}", spec.image, spec.version),
        container_name: Some(format!("nihility-{}", spec.name)),
        command: spec.command.clone(),
        restart: Some(String::from("unless-stopped")),
        ports: spec.published_ports.iter().map(format_port).collect(),
        volumes: spec.volumes.iter().map(format_volume).collect(),
        env_file: vec![String::from(".env")],
        labels: service_labels(spec),
        ..Service::default()
    };
    for volume in &spec.named_volumes {
        service
            .volumes
            .push(format!("{}:{}", volume.name, volume.container_path));
        document
            .volumes
            .insert(volume.name.clone(), compose::Volume {});
    }

    match &spec.network_mode {
        NetworkMode::Bridge => {}
        NetworkMode::Host => service.network_mode = Some(String::from("host")),
        NetworkMode::External(name) => {
            service.networks.push(String::from("application"));
            document.networks.insert(
                String::from("application"),
                Network {
                    name: Some(name.clone()),
                    external: Some(true),
                },
            );
        }
    }
    if !spec.routes.is_empty() {
        add_external_network(&mut document, &mut service, "traefik", TRAEFIK_NETWORK);
    }
    document.services.insert(spec.service.clone(), service);
    Ok(GeneratedStack {
        name: spec.name.clone(),
        compose_yaml: compose::to_yaml(&document)?,
        env_file: environment
            .iter()
            .map(|(key, value)| format!("{key}={}\n", quote_env(value)))
            .collect(),
        files: Vec::new(),
    })
}

/// 生成 Nginx 静态站点。
pub fn generate_static_site(spec: &StaticSiteSpec) -> anyhow::Result<GeneratedStack> {
    if spec.assets.is_empty() {
        anyhow::bail!("静态站点至少需要一个文件");
    }
    validate_host(&spec.host)?;
    let app = AppSpec {
        name: spec.name.clone(),
        service: String::from("web"),
        image: String::from("nginx"),
        version: spec.nginx_version.clone(),
        command: Vec::new(),
        container_port: 80,
        routes: vec![super::Route {
            host: spec.host.clone(),
            path_prefix: None,
            container_port: 80,
        }],
        published_ports: Vec::new(),
        volumes: vec![Volume {
            host_path: String::from("./site"),
            container_path: String::from("/usr/share/nginx/html"),
            read_only: true,
        }],
        environment: BTreeMap::new(),
        network_mode: NetworkMode::Bridge,
        middlewares: spec.middlewares.clone(),
        labels: Vec::new(),
        named_volumes: Vec::new(),
    };
    let mut generated = generate_application(&app)?;
    generated.files = spec
        .assets
        .iter()
        .map(|asset| GeneratedFile {
            path: PathBuf::from("site").join(&asset.path),
            content: asset.content.clone(),
            mode: 0o640,
        })
        .collect();
    Ok(generated)
}

/// 合并自动生成的路由标签与用户附加的自定义标签。
fn service_labels(spec: &AppSpec) -> Vec<String> {
    let mut labels = route_labels(spec);
    labels.extend(spec.labels.iter().cloned());
    labels
}

/// 生成应用的 Traefik 标签。
fn route_labels(spec: &AppSpec) -> Vec<String> {
    let mut labels = Vec::new();
    if spec.routes.is_empty() {
        return labels;
    }
    labels.push(String::from("traefik.enable=true"));
    labels.push(String::from("traefik.docker.network=nihility-traefik"));
    for (index, route) in spec.routes.iter().enumerate() {
        let router = format!("{}-{index}", spec.name);
        let mut rule = format!("Host(`{}`)", route.host);
        if let Some(prefix) = &route.path_prefix {
            rule.push_str(&format!(" && PathPrefix(`{prefix}`)"));
        }
        labels.extend([
            format!("traefik.http.routers.{router}.rule={rule}"),
            format!("traefik.http.routers.{router}.entrypoints=https"),
            format!("traefik.http.routers.{router}.tls=true"),
            format!("traefik.http.routers.{router}.tls.certresolver=cloudflare"),
            format!("traefik.http.routers.{router}.service={router}"),
            format!(
                "traefik.http.services.{router}.loadbalancer.server.port={}",
                route.container_port
            ),
        ]);
        if !spec.middlewares.is_empty() {
            labels.push(format!(
                "traefik.http.routers.{router}.middlewares={}",
                spec.middlewares
                    .iter()
                    .copied()
                    .map(Middleware::label_ref)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
    }
    labels
}

/// 将外部网络加入文档和服务。
fn add_external_network(document: &mut Document, service: &mut Service, alias: &str, name: &str) {
    if !service.networks.iter().any(|network| network == alias) {
        service.networks.push(alias.to_string());
    }
    document.networks.insert(
        alias.to_string(),
        Network {
            name: Some(name.to_string()),
            external: Some(true),
        },
    );
}

/// 格式化 Compose 端口映射。
fn format_port(port: &super::PublishedPort) -> String {
    format!(
        "{}:{}/{}",
        port.host_port,
        port.container_port,
        port.protocol.compose_suffix()
    )
}

/// 格式化 Compose 卷挂载。
fn format_volume(volume: &Volume) -> String {
    let suffix = if volume.read_only { ":ro" } else { "" };
    format!("{}:{}{suffix}", volume.host_path, volume.container_path)
}

/// 校验常规应用生成参数。
fn validate_app(spec: &AppSpec) -> anyhow::Result<()> {
    crate::orchestrator::validate_stack_name(&spec.name)?;
    crate::orchestrator::validate_stack_name(&spec.service)?;
    if spec.image.trim().is_empty()
        || spec.image.contains(['\n', '\r', '\0', '@'])
        || image_has_tag(&spec.image)
    {
        anyhow::bail!("镜像名不能为空、不能包含摘要或版本标签；请使用 --version 指定标签");
    }
    if !crate::orchestrator::valid_image_version(&spec.version) {
        anyhow::bail!("镜像版本标签格式无效，且不能使用 latest");
    }
    if spec.container_port == 0 && !spec.routes.is_empty() {
        anyhow::bail!("配置 HTTP 路由时必须指定容器端口");
    }
    if matches!(spec.network_mode, NetworkMode::Host) && !spec.routes.is_empty() {
        anyhow::bail!("host 网络模式不能同时使用 Traefik 路由");
    }
    if let NetworkMode::External(name) = &spec.network_mode
        && (name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    {
        anyhow::bail!("外部网络名包含无效字符");
    }
    for (key, value) in &spec.environment {
        let valid_key = key
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
        if key.is_empty() || !valid_key || value.contains(['\0', '\n', '\r']) {
            anyhow::bail!("环境变量 {key} 的名称或值无效");
        }
    }
    for volume in &spec.volumes {
        if volume.host_path.is_empty()
            || !volume.container_path.starts_with('/')
            || volume.host_path.contains(['\0', '\n', '\r'])
            || volume.container_path.contains(['\0', '\n', '\r'])
        {
            anyhow::bail!("卷挂载路径无效");
        }
    }
    for label in &spec.labels {
        let Some((key, value)) = label.split_once('=') else {
            anyhow::bail!("自定义标签格式必须为 KEY=VALUE");
        };
        if key.trim().is_empty()
            || key.contains(['\0', '\n', '\r'])
            || value.contains(['\0', '\n', '\r'])
        {
            anyhow::bail!("自定义标签 {key} 的名称或值无效");
        }
    }
    for volume in &spec.named_volumes {
        let valid_name = !volume.name.is_empty()
            && volume.name.len() <= 63
            && volume.name.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.')
            });
        if !valid_name
            || !volume.container_path.starts_with('/')
            || volume.name.contains(['\0', '\n', '\r'])
            || volume.container_path.contains(['\0', '\n', '\r'])
        {
            anyhow::bail!("命名卷名称或容器路径无效");
        }
    }
    for route in &spec.routes {
        validate_host(&route.host)?;
        if let Some(prefix) = &route.path_prefix
            && (!prefix.starts_with('/') || prefix.contains(['`', '\n', '\r']))
        {
            anyhow::bail!("路由路径必须以 / 开头且不能包含控制字符");
        }
    }
    Ok(())
}

/// 判断镜像名最后一个路径片段是否已经包含标签。
fn image_has_tag(image: &str) -> bool {
    let last_slash = image.rfind('/');
    image
        .rfind(':')
        .is_some_and(|colon| last_slash.is_none_or(|slash| colon > slash))
}

/// 引用环境变量文件中的值。
fn quote_env(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// 校验完整域名。
fn validate_host(host: &str) -> anyhow::Result<()> {
    let valid = !host.is_empty()
        && host.contains('.')
        && host.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'.'
        });
    if !valid {
        anyhow::bail!("域名格式无效: {host}");
    }
    Ok(())
}

#[cfg(test)]
// 测试断言直接暴露失败原因，允许在测试中调用 unwrap。
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::generator::{NamedVolume, PortProtocol, PublishedPort, Route};

    /// 验证自定义标签、命名卷和以连字符开头的命令参数进入生成的 Compose。
    #[test]
    fn custom_parameters_reach_compose() {
        let spec = AppSpec {
            name: String::from("netbird-server"),
            service: String::from("app"),
            image: String::from("netbirdio/netbird-server"),
            version: String::from("0.76.3"),
            command: vec![
                String::from("--config"),
                String::from("/etc/netbird/config.yaml"),
            ],
            container_port: 80,
            routes: Vec::new(),
            published_ports: vec![PublishedPort {
                host_port: 3478,
                container_port: 3478,
                protocol: PortProtocol::Udp,
            }],
            volumes: vec![Volume {
                host_path: String::from("/etc/netbird/config.yaml"),
                container_path: String::from("/etc/netbird/config.yaml"),
                read_only: true,
            }],
            environment: BTreeMap::new(),
            network_mode: NetworkMode::External(String::from("nihility-traefik")),
            middlewares: Vec::new(),
            labels: vec![
                String::from(
                    "traefik.http.routers.netbird-grpc.rule=Host(`netbird.example.com`) && (PathPrefix(`/api`))",
                ),
                String::from("traefik.http.routers.netbird-grpc.priority=100"),
            ],
            named_volumes: vec![NamedVolume {
                name: String::from("netbird-data"),
                container_path: String::from("/var/lib/netbird"),
            }],
        };

        let generated = generate_application(&spec).unwrap();
        let document: serde_yaml::Value = serde_yaml::from_str(&generated.compose_yaml).unwrap();
        let service = &document["services"]["app"];

        assert_eq!(
            service["command"],
            serde_yaml::Value::Sequence(vec![
                serde_yaml::Value::String(String::from("--config")),
                serde_yaml::Value::String(String::from("/etc/netbird/config.yaml")),
            ])
        );
        let volumes = service["volumes"]
            .as_sequence()
            .map(|list| {
                list.iter()
                    .map(|value| value.as_str().unwrap())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert!(volumes.contains(&"netbird-data:/var/lib/netbird"));
        assert!(volumes.contains(&"/etc/netbird/config.yaml:/etc/netbird/config.yaml:ro"));
        let labels = service["labels"]
            .as_sequence()
            .map(|list| {
                list.iter()
                    .map(|value| value.as_str().unwrap())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert!(
            labels
                .iter()
                .any(|label| label.starts_with("traefik.http.routers.netbird-grpc.rule="))
        );
        assert!(document["volumes"]["netbird-data"].is_mapping());
        assert_eq!(
            document["networks"]["application"]["name"],
            serde_yaml::Value::String(String::from("nihility-traefik"))
        );
    }

    /// 验证自定义标签追加在自动生成的路由标签之后。
    #[test]
    fn custom_labels_append_after_route_labels() {
        let spec = AppSpec {
            name: String::from("netbird"),
            service: String::from("app"),
            image: String::from("netbirdio/dashboard"),
            version: String::from("v2.90.10"),
            command: Vec::new(),
            container_port: 80,
            routes: vec![Route {
                host: String::from("netbird.example.com"),
                path_prefix: None,
                container_port: 80,
            }],
            published_ports: Vec::new(),
            volumes: Vec::new(),
            environment: BTreeMap::new(),
            network_mode: NetworkMode::Bridge,
            middlewares: Vec::new(),
            labels: vec![String::from("traefik.http.routers.netbird-0.priority=1")],
            named_volumes: Vec::new(),
        };

        let generated = generate_application(&spec).unwrap();
        let document: serde_yaml::Value = serde_yaml::from_str(&generated.compose_yaml).unwrap();
        let labels = document["services"]["app"]["labels"]
            .as_sequence()
            .map(|list| {
                list.iter()
                    .map(|value| value.as_str().unwrap())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        let route_index = labels
            .iter()
            .position(|label| label.starts_with("traefik.http.routers.netbird-0.rule="))
            .unwrap();
        let custom_index = labels
            .iter()
            .position(|label| *label == "traefik.http.routers.netbird-0.priority=1")
            .unwrap();
        assert!(custom_index > route_index);
    }

    /// 验证非法标签和命名卷会被拒绝。
    #[test]
    fn invalid_custom_parameters_rejected() {
        let base = || AppSpec {
            name: String::from("demo"),
            service: String::from("app"),
            image: String::from("example/demo"),
            version: String::from("1.0"),
            command: Vec::new(),
            container_port: 80,
            routes: Vec::new(),
            published_ports: Vec::new(),
            volumes: Vec::new(),
            environment: BTreeMap::new(),
            network_mode: NetworkMode::Bridge,
            middlewares: Vec::new(),
            labels: Vec::new(),
            named_volumes: Vec::new(),
        };

        let bad_label = AppSpec {
            labels: vec![String::from("missing-equals")],
            ..base()
        };
        assert!(generate_application(&bad_label).is_err());

        let bad_volume = AppSpec {
            named_volumes: vec![NamedVolume {
                name: String::from("Bad_Name"),
                container_path: String::from("relative/path"),
            }],
            ..base()
        };
        assert!(generate_application(&bad_volume).is_err());
    }
}
