//! 最小且类型化的 Docker Compose 文档模型。

use anyhow::Context;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Default, Serialize)]
/// 可序列化的 Compose 文档。
pub struct Document {
    /// Compose 服务。
    pub services: BTreeMap<String, Service>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    /// Compose 网络。
    pub networks: BTreeMap<String, Network>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    /// Compose 命名卷。
    pub volumes: BTreeMap<String, Volume>,
}

/// Compose 命名卷定义。
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Volume {}

#[derive(Debug, Default, Serialize)]
/// Compose 服务定义。
pub struct Service {
    /// 容器镜像。
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 固定容器名。
    pub container_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    /// 覆盖镜像命令的参数。
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 重启策略。
    pub restart: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 网络模式。
    pub network_mode: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    /// 关联网络。
    pub networks: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    /// 发布端口。
    pub ports: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    /// 数据卷。
    pub volumes: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    /// 环境变量映射。
    pub environment: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    /// 环境变量文件。
    pub env_file: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    /// 容器标签。
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 健康检查。
    pub healthcheck: Option<Healthcheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 容器日志驱动配置。
    pub logging: Option<Logging>,
}

#[derive(Debug, Default, Serialize)]
/// Compose 网络定义。
pub struct Network {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 实际 Docker 网络名。
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 是否引用外部网络。
    pub external: Option<bool>,
}

#[derive(Debug, Serialize)]
/// Compose 健康检查定义。
pub struct Healthcheck {
    /// 检查命令。
    pub test: Vec<String>,
    /// 检查间隔。
    pub interval: String,
    /// 单次检查超时。
    pub timeout: String,
    /// 失败重试次数。
    pub retries: u32,
}

#[derive(Debug, Serialize)]
/// Compose 容器日志驱动配置。
pub struct Logging {
    /// Docker 日志驱动名。
    pub driver: String,
    /// 日志驱动选项。
    pub options: BTreeMap<String, String>,
}

/// 将 Compose 文档序列化为 YAML。
pub fn to_yaml(document: &Document) -> anyhow::Result<String> {
    serde_yaml::to_string(document).context("无法序列化生成的 Compose 配置")
}
