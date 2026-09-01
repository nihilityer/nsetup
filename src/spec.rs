//! 强类型 Compose 中间表示与语义视图。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 项目级 Compose 状态与环境文件转换。
mod project;
/// 服务级镜像与 Traefik 路由语义。
mod service;
/// `spec` 模块单元测试。
#[cfg(test)]
mod tests;
/// 端口、挂载、健康检查与公共校验。
mod value;

pub use value::{split_tagged_image, validate_name, validate_version};

/// 完整的受管项目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackSpec {
    /// Compose 项目名。
    pub name: String,
    /// 类型化 Compose 文档。
    pub document: Document,
    /// 持久化到 `.env` 的变量。
    pub environment: BTreeMap<String, String>,
}

/// 受支持的顶层 Compose 文档。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Document {
    /// 以 Compose 服务名为键的服务映射。
    pub services: BTreeMap<String, Service>,
    /// 以逻辑名称为键的项目网络映射。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub networks: BTreeMap<String, Network>,
}

/// 受支持的 Compose 服务字段。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Service {
    /// 包含完整标签的镜像引用。
    pub image: String,
    /// 可选的固定容器名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    /// 替换镜像默认命令的参数。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Compose 重启策略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<String>,
    /// 宿主机或其他直接 Docker 网络模式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_mode: Option<String>,
    /// 服务加入的具名 Compose 网络。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<String>,
    /// 短语法发布端口映射。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    /// 短语法 bind mount。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<String>,
    /// 映射形式的容器环境变量。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// 容器读取的环境变量文件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_file: Vec<String>,
    /// 列表形式的 Docker label。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// 可选容器健康检查。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<Healthcheck>,
    /// 可选 Docker 日志配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<Logging>,
}

/// 受支持的 Compose 网络定义。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// 是否要求 Docker 中已存在该网络。
    #[serde(default, skip_serializing_if = "is_false")]
    pub external: bool,
    /// 可选的固定 Docker 网络名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 受支持的 Compose 健康检查定义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Healthcheck {
    /// Compose 健康检查测试，通常为 `CMD-SHELL` 加一条命令。
    pub test: Vec<String>,
    /// 检查间隔。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// 超时时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// 启动宽限期。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_period: Option<String>,
    /// 重试次数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
}

/// 受支持的 Compose 日志定义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Logging {
    /// Docker 日志驱动。
    pub driver: String,
    /// 驱动专用选项。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, String>,
}

/// 从 label 派生的 Traefik 语义路由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// 在 TOML、Compose router 和 backend 中保持稳定的路由名。
    pub name: String,
    /// 此路由器处理的 DNS 主机名。
    pub hosts: Vec<String>,
    /// 可选 URL 路径前缀。
    pub path_prefix: Option<String>,
    /// 目标容器端口。
    pub container_port: u16,
    /// 不含提供者后缀的中间件名称。
    pub middlewares: Vec<String>,
    /// 后端协议。
    pub protocol: RouteProtocol,
    /// 是否启用负载均衡粘性 Cookie。
    pub sticky_cookie: bool,
    /// 可选的 `passHostHeader` 覆盖值。
    pub pass_host_header: Option<bool>,
    /// 可选路由优先级。
    pub priority: Option<u32>,
}

/// 受支持的 Traefik 后端协议。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RouteProtocol {
    /// 普通 HTTP。
    #[default]
    Http,
    /// 连接后端的 HTTPS。
    Https,
    /// 明文 HTTP/2。
    H2c,
}

impl RouteProtocol {
    /// 返回 Traefik scheme 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::H2c => "h2c",
        }
    }

    /// 解析受支持的协议。
    ///
    /// # 错误
    ///
    /// 值不受支持时返回错误。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            "h2c" => Ok(Self::H2c),
            _ => anyhow::bail!("不支持的路由协议: {value}"),
        }
    }
}

/// 用于校验和编辑的已解析发布端口。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublishedPort {
    /// 可选的宿主机 IP 绑定。
    pub host_ip: Option<String>,
    /// 宿主机端口。
    pub host_port: u16,
    /// 容器端口。
    pub container_port: u16,
    /// 传输协议。
    pub protocol: PortProtocol,
}

/// 受支持的发布端口协议。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum PortProtocol {
    /// 传输控制协议（TCP）。
    #[default]
    Tcp,
    /// 用户数据报协议（UDP）。
    Udp,
}

impl PortProtocol {
    /// 返回 Compose 后缀。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// 已解析的 bind mount。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindMount {
    /// 宿主机源绝对路径。
    pub host_path: String,
    /// 容器目标绝对路径。
    pub container_path: String,
    /// 只读标志。
    pub read_only: bool,
}

/// 用于省略 `false` 值的 Serde 辅助函数。
fn is_false(value: &bool) -> bool {
    !value
}
