//! Traefik 接管情况自查：比对容器 label 与 Traefik 实际加载的路由。
//!
//! `A3`（`traefik.enable` 被丢弃）和 `B4`（探针失败导致容器 unhealthy）这类故障
//! 的共同表现是"容器运行正常但域名 404"，两边都不报错。这里把判断依据摊开：
//! 容器是否声明了 router label、Traefik 是否加载了同名 router、服务是否位于共享
//! 回源网络上。

use crate::config::Config;
use crate::constants::{COMPOSE_FILE, PROXY_NETWORK};
use crate::docker;
use anyhow::Context;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Traefik 内部 API 的默认端口，与 traefik 模板的 metrics 入口一致。
const DEFAULT_API_PORT: u16 = crate::spec::DEFAULT_METRICS_PORT;
/// Traefik 容器的默认名称。
const TRAEFIK_CONTAINER: &str = "traefik";
/// 单次 HTTP 请求的上限，避免异常响应占满内存。
const MAX_RESPONSE: usize = 8 * 1024 * 1024;
/// 连接与读写超时。
const TIMEOUT: Duration = Duration::from_secs(5);

/// 诊断报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// 可直接显示给操作者的多行文本。
    pub text: String,
    /// 发现的问题数量。
    pub problems: u32,
}

/// Traefik 加载的单个 router。
#[derive(Debug, Clone, serde::Deserialize)]
struct LoadedRouter {
    /// router 名。
    #[serde(default)]
    name: String,
    /// 匹配规则。
    #[serde(default)]
    rule: String,
}

/// 预期存在的 router。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpectedRouter {
    /// router 名。
    name: String,
    /// 匹配规则。
    rule: String,
    /// 归属说明。
    owner: String,
    /// 承载该 router 的容器名。
    container: String,
}

/// 生成诊断报告。
///
/// 报告不会因为 Traefik API 不可达而失败：无法连接时降级为纯 label 检查，并在
/// 结论中说明降级原因。
///
/// # 错误
///
/// Docker CLI 无法执行或受管项目状态无法解析时返回错误。
pub fn report(config: &Config) -> anyhow::Result<Report> {
    let mut lines = vec![String::from("nsetup doctor 诊断报告")];
    let mut problems = 0_u32;
    let containers = docker::running_containers(config).unwrap_or_default();
    if containers.is_empty() {
        lines.push(String::from("- 没有运行中的容器"));
    }
    let expected = expected_routers(config, &containers, &mut lines)?;
    check_container_labels(config, &containers, &expected, &mut lines, &mut problems);
    match traefik_api(config, &containers) {
        Some(loaded) => {
            compare_with_traefik(&expected, &loaded, &mut lines, &mut problems);
            if problems == 0 {
                lines.push(format!(
                    "- 全部检查通过：{} 条 nsetup 路由均已由 Traefik 加载",
                    expected.len()
                ));
            }
        }
        None => {
            lines.push(String::from(
                "- 无法连接 Traefik API（指标入口未开放或容器未运行），已降级为仅检查容器 label",
            ));
            if expected.is_empty() && problems == 0 {
                lines.push(String::from("- 没有从受管项目解析出 Traefik 路由"));
            }
        }
    }
    Ok(Report {
        text: lines.join("\n"),
        problems,
    })
}

/// 检查容器 label 层面的常见接管失败。
fn check_container_labels(
    config: &Config,
    containers: &[docker::ContainerSummary],
    expected: &[ExpectedRouter],
    lines: &mut Vec<String>,
    problems: &mut u32,
) {
    let addresses = proxy_addresses(config, containers);
    for container in containers {
        let declares_router = container
            .labels
            .keys()
            .any(|key| key.starts_with("traefik.http.routers."));
        if declares_router {
            let enabled = container
                .labels
                .get("traefik.enable")
                .is_some_and(|value| value == "true");
            if !enabled {
                *problems += 1;
                lines.push(format!(
                    "- 容器 {} 声明了 router label，但没有 traefik.enable=true；docker provider \
                     默认 exposedByDefault=false，Traefik 不会加载它",
                    container.name
                ));
            }
        }
        let routed = expected
            .iter()
            .any(|router| router.container == container.name);
        if (declares_router || routed)
            && container.name != TRAEFIK_CONTAINER
            && !addresses.contains_key(&container.name)
        {
            *problems += 1;
            lines.push(format!(
                "- 容器 {} 不在共享网络 {PROXY_NETWORK} 上，Traefik 无法回源",
                container.name
            ));
        }
    }
}

/// 比对预期 router 与 Traefik 实际加载的 router；两侧名字先归一化再比。
fn compare_with_traefik(
    expected: &[ExpectedRouter],
    loaded: &[LoadedRouter],
    lines: &mut Vec<String>,
    problems: &mut u32,
) {
    let loaded_names: BTreeSet<&str> = loaded
        .iter()
        .map(|router| normalized_router_name(&router.name))
        .collect();
    for router in expected {
        if !loaded_names.contains(normalized_router_name(&router.name)) {
            *problems += 1;
            lines.push(format!(
                "- 路由 {} 未被 Traefik 加载（{}，规则 {}）",
                router.name, router.owner, router.rule
            ));
        }
    }
    let expected_names: BTreeSet<&str> = expected
        .iter()
        .map(|router| normalized_router_name(&router.name))
        .collect();
    for router in loaded {
        let name = normalized_router_name(&router.name);
        if name.starts_with("nsetup-") && !expected_names.contains(name) {
            *problems += 1;
            lines.push(format!(
                "- Traefik 中存在 nsetup 已不再声明的路由 {}（规则 {}）",
                router.name, router.rule
            ));
        }
    }
}

/// 去掉 router 名的 `@<提供者>` 后缀，使两侧可以按同一形式比对。
fn normalized_router_name(name: &str) -> &str {
    name.split_once('@').map_or(name, |(head, _provider)| head)
}

/// 从全部受管项目收集预期存在的 nsetup router。
fn expected_routers(
    config: &Config,
    containers: &[docker::ContainerSummary],
    lines: &mut Vec<String>,
) -> anyhow::Result<Vec<ExpectedRouter>> {
    let mut expected = Vec::new();
    if !config.stacks_root.is_dir() {
        return Ok(expected);
    }
    for entry in std::fs::read_dir(&config.stacks_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || !entry.path().join(COMPOSE_FILE).is_file()
            || entry.file_name().to_string_lossy().starts_with('.')
        {
            continue;
        }
        let spec = match crate::spec::StackSpec::load(&entry.path()) {
            Ok(spec) => spec,
            Err(error) => {
                lines.push(format!(
                    "- 项目 {} 状态无法解析：{error}",
                    entry.file_name().to_string_lossy()
                ));
                continue;
            }
        };
        for (service, item) in &spec.document.services {
            let routes = match item.routes(&spec.name, service) {
                Ok(routes) => routes,
                Err(error) => {
                    lines.push(format!(
                        "- 项目 {} 的服务 {service} 路由无法解析：{error}",
                        spec.name
                    ));
                    continue;
                }
            };
            for route in routes {
                let container = containers
                    .iter()
                    .find(|container| {
                        container
                            .labels
                            .get("com.docker.compose.project")
                            .is_some_and(|project| project == &spec.name)
                            && container
                                .labels
                                .get("com.docker.compose.service")
                                .is_some_and(|name| name == service)
                    })
                    .map_or_else(|| service.clone(), |container| container.name.clone());
                expected.push(ExpectedRouter {
                    name: format!("nsetup-{}-{}-{}", spec.name, service, route.name),
                    rule: route
                        .hosts
                        .iter()
                        .map(|host| match &route.path_prefix {
                            Some(path) => format!("Host(`{host}`) && PathPrefix(`{path}`)"),
                            None => format!("Host(`{host}`)"),
                        })
                        .collect::<Vec<_>>()
                        .join(" || "),
                    owner: format!("项目 {} 服务 {service}", spec.name),
                    container,
                });
            }
        }
    }
    Ok(expected)
}

/// 收集运行中容器在共享网络上的地址。
fn proxy_addresses(
    config: &Config,
    containers: &[docker::ContainerSummary],
) -> BTreeMap<String, String> {
    let mut addresses = BTreeMap::new();
    for container in containers {
        if let Ok(address) = docker::container_address(config, &container.id, PROXY_NETWORK) {
            addresses.insert(container.name.clone(), address);
        }
    }
    addresses
}

/// 查询 Traefik 的 HTTP router 列表；不可达时返回 `None`。
fn traefik_api(
    config: &Config,
    containers: &[docker::ContainerSummary],
) -> Option<Vec<LoadedRouter>> {
    let traefik = containers
        .iter()
        .find(|container| container.name == TRAEFIK_CONTAINER)
        .or_else(|| {
            containers.iter().find(|container| {
                container
                    .image
                    .split('/')
                    .next_back()
                    .is_some_and(|name| name.starts_with("traefik"))
            })
        })?;
    let address = docker::container_address(config, &traefik.id, PROXY_NETWORK).ok()?;
    let port = metrics_port(config).unwrap_or(DEFAULT_API_PORT);
    let socket: SocketAddr = format!("{address}:{port}").parse().ok()?;
    let body = http_get(socket, "/api/http/routers").ok()?;
    serde_json::from_str(&body).ok()
}

/// 从已部署的 traefik 项目读出指标入口端口。
fn metrics_port(config: &Config) -> Option<u16> {
    let directory = config.stacks_root.join(TRAEFIK_CONTAINER);
    if !directory.is_dir() {
        return None;
    }
    let spec = crate::spec::StackSpec::load(&directory).ok()?;
    let service = spec.document.services.get(TRAEFIK_CONTAINER)?;
    service
        .command
        .iter()
        .find_map(|argument| argument.strip_prefix("--entrypoints.metrics.address=:"))
        .and_then(|value| value.parse::<u16>().ok())
}

/// 发起一次最小 HTTP/1.1 GET 请求并返回响应正文。
fn http_get(socket: SocketAddr, path: &str) -> anyhow::Result<String> {
    let mut stream = TcpStream::connect_timeout(&socket, TIMEOUT)
        .with_context(|| format!("无法连接 {socket}"))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {socket}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    let mut raw = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 || raw.len() >= MAX_RESPONSE {
            break;
        }
        raw.extend_from_slice(&buffer[..read]);
    }
    decode_response(&raw)
}

/// 解析 HTTP 响应，支持 `Content-Length` 与 chunked 两种正文编码。
fn decode_response(raw: &[u8]) -> anyhow::Result<String> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("HTTP 响应缺少头部终止符"))?;
    let headers = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| anyhow::anyhow!("HTTP 状态行无效"))?;
    if status != 200 {
        anyhow::bail!("HTTP 状态码 {status}");
    }
    let body = &raw[split + 4..];
    if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return Ok(String::from_utf8_lossy(&decode_chunked(body)?).into_owned());
    }
    Ok(String::from_utf8_lossy(body).into_owned())
}

/// 解码 HTTP chunked 正文。
fn decode_chunked(body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut remaining = body;
    loop {
        let line_end = remaining
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| anyhow::anyhow!("chunked 长度行未闭合"))?;
        let header = String::from_utf8_lossy(&remaining[..line_end]);
        let size = usize::from_str_radix(header.split(';').next().unwrap_or_default().trim(), 16)
            .map_err(|error| anyhow::anyhow!("chunked 长度无效: {error}"))?;
        remaining = &remaining[line_end + 2..];
        if size == 0 {
            return Ok(output);
        }
        if remaining.len() < size {
            anyhow::bail!("chunked 正文被截断");
        }
        output.extend_from_slice(&remaining[..size]);
        if remaining.len() < size + 2 {
            return Ok(output);
        }
        remaining = &remaining[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExpectedRouter, LoadedRouter, compare_with_traefik, decode_chunked, decode_response,
    };

    /// Traefik API 的名字带 `@provider` 后缀，比对前必须归一化（R11）。
    #[test]
    fn compares_router_names_without_provider_suffix() {
        let expected = vec![
            ExpectedRouter {
                name: String::from("nsetup-authelia-authelia-default"),
                rule: String::from("Host(`auth.example.com`)"),
                owner: String::from("项目 authelia 服务 authelia"),
                container: String::from("authelia"),
            },
            ExpectedRouter {
                name: String::from("nsetup-netbird-dashboard-default"),
                rule: String::from("Host(`net.example.com`)"),
                owner: String::from("项目 netbird 服务 dashboard"),
                container: String::from("netbird-dashboard"),
            },
        ];
        let loaded = vec![
            LoadedRouter {
                name: String::from("nsetup-authelia-authelia-default@docker"),
                rule: String::from("Host(`auth.example.com`)"),
            },
            LoadedRouter {
                name: String::from("nsetup-netbird-dashboard-default@docker"),
                rule: String::from("Host(`net.example.com`)"),
            },
            LoadedRouter {
                name: String::from("api@internal"),
                rule: String::from("PathPrefix(`/api`)"),
            },
            LoadedRouter {
                name: String::from("dashboard@internal"),
                rule: String::from("(PathPrefix(`/api`) || PathPrefix(`/dashboard`))"),
            },
        ];
        let mut lines = Vec::new();
        let mut problems = 0;
        compare_with_traefik(&expected, &loaded, &mut lines, &mut problems);
        assert_eq!(problems, 0, "{lines:?}");
        assert!(lines.is_empty(), "{lines:?}");

        // 反向：既没有真正缺失，也不会把内置 router 当成「已不再声明」。
        let mut lines = Vec::new();
        let mut problems = 0;
        compare_with_traefik(&expected, &loaded[..1], &mut lines, &mut problems);
        assert_eq!(problems, 1, "{lines:?}");
        assert!(
            lines[0].contains("nsetup-netbird-dashboard-default"),
            "{lines:?}"
        );
    }

    /// 固定长度响应正文可直接读出。
    #[test]
    fn decodes_content_length_response() -> anyhow::Result<()> {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]";
        assert_eq!(decode_response(raw)?, "[]");
        Ok(())
    }

    /// Traefik 使用 chunked 传输，必须正确拼接分块正文。
    #[test]
    fn decodes_chunked_response() -> anyhow::Result<()> {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n[{\"a\r\n3\r\n\":1\r\n1\r\n]\r\n0\r\n\r\n";
        assert_eq!(decode_response(raw)?, "[{\"a\":1]");
        Ok(())
    }

    /// 非 200 状态必须报错而不是当作正文解析。
    #[test]
    fn rejects_error_status() {
        let raw = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        assert!(decode_response(raw).is_err());
    }

    /// 截断的分块正文不会静默返回半截数据。
    #[test]
    fn truncated_chunk_is_rejected() {
        assert!(decode_chunked(b"5\r\nab").is_err());
    }

    /// Traefik router 列表按 JSON 解析，未知字段被忽略。
    #[test]
    fn parses_traefik_router_list() -> anyhow::Result<()> {
        let body = r#"[{"name":"nsetup-media-web-default","status":"enabled",
            "rule":"Host(`media.example.com`)","service":"nsetup-media-web-default",
            "provider":"docker","priority":42,"extra":"ignored"}]"#;
        let routers: Vec<super::LoadedRouter> = serde_json::from_str(body)?;
        assert_eq!(routers.len(), 1);
        assert_eq!(routers[0].name, "nsetup-media-web-default");
        assert_eq!(routers[0].rule, "Host(`media.example.com`)");
        Ok(())
    }
}
