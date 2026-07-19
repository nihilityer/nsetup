//! Docker Compose 容器状态解析。

use super::{ContainerHealth, ContainerInfo, ContainerState};

/// 解析不同 Compose 版本返回的 JSON 数组或逐行 JSON。
pub(super) fn parse_container_status(content: &str) -> Vec<ContainerInfo> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        return match value {
            serde_json::Value::Array(items) => items.iter().filter_map(parse_container).collect(),
            serde_json::Value::Object(_) => parse_container(&value).into_iter().collect(),
            _ => Vec::new(),
        };
    }
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|value| parse_container(&value))
        .collect()
}

/// 从单个 Compose JSON 对象提取容器状态。
fn parse_container(value: &serde_json::Value) -> Option<ContainerInfo> {
    let service = json_string(value, "Service", "service");
    if service.is_empty() {
        return None;
    }
    Some(ContainerInfo {
        service,
        name: json_string(value, "Name", "name"),
        state: ContainerState::from_docker(&json_string(value, "State", "state")),
        health: ContainerHealth::from_docker(&json_string(value, "Health", "health")),
    })
}

/// 兼容 Compose JSON 字段的大小写差异。
fn json_string(value: &serde_json::Value, upper: &str, lower: &str) -> String {
    value
        .get(upper)
        .or_else(|| value.get(lower))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}
