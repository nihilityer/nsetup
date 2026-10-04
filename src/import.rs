//! 既有 Compose YAML 的宽容导入。
//!
//! `nsetup import` 的目标是把已经跑起来的 Compose 项目接管进来，因此遇到 IR 不
//! 支持的字段时不再直接失败：受支持字段照常转换，其余字段被忽略并列进报告，由
//! 操作者决定是否手工翻译。真正影响安全的字段（命名卷、相对 bind mount、未钉定
//! 版本镜像）仍由 [`crate::spec::StackSpec::validate`] 拒绝。

use crate::spec::{Document, StackSpec};
use anyhow::Context;
use std::collections::BTreeSet;

/// 顶层 Compose 中受支持的键。
const TOP_LEVEL_KEYS: [&str; 2] = ["services", "networks"];
/// 当前 IR 真正能承载的服务级键；其余键会被忽略并记录。
const SERVICE_KEYS: [&str; 15] = [
    "image",
    "container_name",
    "command",
    "restart",
    "network_mode",
    "networks",
    "ports",
    "volumes",
    "environment",
    "env_file",
    "labels",
    "user",
    "group_add",
    "healthcheck",
    "logging",
];
/// 网络级 Compose 中受支持的键。
const NETWORK_KEYS: [&str; 5] = ["external", "name", "driver", "internal", "labels"];
/// 健康检查中受支持的键。
const HEALTHCHECK_KEYS: [&str; 6] = [
    "test",
    "interval",
    "timeout",
    "start_period",
    "retries",
    "disable",
];
/// 报告里最多列出的忽略字段数量。
const MAX_REPORTED: usize = 40;

/// 一次宽容导入的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    /// 解析并校验后的项目状态。
    pub spec: StackSpec,
    /// 被忽略的字段路径，按字典序排列。
    pub ignored: Vec<String>,
}

impl ImportOutcome {
    /// 返回可直接显示给操作者的导入说明。
    #[must_use]
    pub fn summary(&self) -> String {
        if self.ignored.is_empty() {
            return String::from("；未发现被忽略的 Compose 字段");
        }
        let shown = self
            .ignored
            .iter()
            .take(MAX_REPORTED)
            .cloned()
            .collect::<Vec<_>>()
            .join("、");
        let extra = self.ignored.len().saturating_sub(MAX_REPORTED);
        if extra == 0 {
            format!("；已忽略 IR 不支持的 Compose 字段：{shown}")
        } else {
            format!(
                "；已忽略 IR 不支持的 Compose 字段：{shown} 等 {} 项",
                self.ignored.len()
            )
        }
    }
}

/// 宽容导入 Compose YAML，并报告被忽略的字段。
///
/// # 错误
///
/// YAML 不是映射、缺少 `services`，或受支持字段本身无效时返回错误。
pub fn import_compose(
    name: &str,
    compose_yaml: &str,
    env_file: &str,
) -> anyhow::Result<ImportOutcome> {
    let mut value: serde_yaml::Value =
        serde_yaml::from_str(compose_yaml).context("Compose YAML 格式错误")?;
    let mut ignored = BTreeSet::new();
    prune_document(&mut value, &mut ignored)?;
    let document: Document = serde_yaml::from_value(value)
        .context("Compose YAML 的受支持字段类型不正确（例如 ports 必须是列表）")?;
    let spec = StackSpec::parse_compose(name, document, env_file)?;
    Ok(ImportOutcome {
        spec,
        ignored: ignored.into_iter().collect(),
    })
}

/// 删除顶层、服务级、网络级与健康检查级的不支持键。
fn prune_document(
    value: &mut serde_yaml::Value,
    ignored: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    let mapping = value
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("Compose YAML 顶层必须是映射"))?;
    prune_keys(mapping, "", &TOP_LEVEL_KEYS, ignored);
    let Some(services) = mapping
        .get_mut("services")
        .and_then(serde_yaml::Value::as_mapping_mut)
    else {
        anyhow::bail!("Compose YAML 缺少 services 映射");
    };
    let names: Vec<String> = services
        .keys()
        .filter_map(serde_yaml::Value::as_str)
        .map(str::to_string)
        .collect();
    for name in names {
        let Some(service) = services
            .get_mut(serde_yaml::Value::String(name.clone()))
            .and_then(serde_yaml::Value::as_mapping_mut)
        else {
            anyhow::bail!("服务 {name} 必须是映射");
        };
        prune_keys(service, &format!("services.{name}"), &SERVICE_KEYS, ignored);
        if let Some(healthcheck) = service
            .get_mut("healthcheck")
            .and_then(serde_yaml::Value::as_mapping_mut)
        {
            prune_keys(
                healthcheck,
                &format!("services.{name}.healthcheck"),
                &HEALTHCHECK_KEYS,
                ignored,
            );
            // `disable: true` 表示沿用镜像自带探针，等价于不声明。
            let disabled = healthcheck
                .get("disable")
                .and_then(serde_yaml::Value::as_bool)
                .unwrap_or(false);
            if healthcheck.remove("disable").is_some() {
                ignored.insert(format!("services.{name}.healthcheck.disable"));
            }
            if disabled {
                let _removed = service.remove("healthcheck");
            }
        }
    }
    if let Some(networks) = mapping
        .get_mut("networks")
        .and_then(serde_yaml::Value::as_mapping_mut)
    {
        let names: Vec<String> = networks
            .keys()
            .filter_map(serde_yaml::Value::as_str)
            .map(str::to_string)
            .collect();
        for name in names {
            let Some(network) = networks
                .get_mut(serde_yaml::Value::String(name.clone()))
                .and_then(serde_yaml::Value::as_mapping_mut)
            else {
                continue;
            };
            prune_keys(network, &format!("networks.{name}"), &NETWORK_KEYS, ignored);
        }
    }
    Ok(())
}

/// 从映射中删除白名单之外的全部键并记录完整路径。
fn prune_keys(
    mapping: &mut serde_yaml::Mapping,
    prefix: &str,
    allowed: &[&str],
    ignored: &mut BTreeSet<String>,
) {
    let removed: Vec<serde_yaml::Value> = mapping
        .keys()
        .filter(|key| key.as_str().is_none_or(|name| !allowed.contains(&name)))
        .cloned()
        .collect();
    for key in removed {
        let Some(name) = key.as_str().map(str::to_string) else {
            mapping.remove(&key);
            continue;
        };
        mapping.remove(&key);
        ignored.insert(if prefix.is_empty() {
            name
        } else {
            format!("{prefix}.{name}")
        });
    }
}

#[cfg(test)]
mod tests {
    use super::import_compose;

    /// 常见生产 Compose 的不支持字段被忽略而不是整体拒绝。
    #[test]
    fn ignores_unsupported_fields() -> anyhow::Result<()> {
        let outcome = import_compose(
            "media",
            r#"
version: "3.9"
x-common: &common
  restart: unless-stopped
services:
  web:
    image: example/web:1.2
    restart: unless-stopped
    depends_on:
      - db
    cap_drop: [ALL]
    healthcheck:
      test: [CMD, /app/healthcheck]
      interval: 30s
      start_period: 10s
    deploy:
      resources:
        limits:
          memory: 512M
networks: {}
"#,
            "",
        )?;
        assert_eq!(outcome.spec.document.services.len(), 1);
        assert!(outcome.ignored.iter().any(|value| value == "version"));
        assert!(outcome.ignored.iter().any(|value| value == "x-common"));
        assert!(
            outcome
                .ignored
                .iter()
                .any(|value| value == "services.web.depends_on")
        );
        assert!(
            outcome
                .ignored
                .iter()
                .any(|value| value == "services.web.cap_drop")
        );
        assert!(
            outcome
                .ignored
                .iter()
                .any(|value| value == "services.web.deploy")
        );
        assert!(outcome.summary().contains("已忽略"));
        let healthcheck = outcome.spec.document.services["web"]
            .healthcheck
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing healthcheck"))?;
        assert_eq!(healthcheck.test, ["CMD", "/app/healthcheck"]);
        Ok(())
    }

    /// `healthcheck.disable: true` 等价于沿用镜像自带探针。
    #[test]
    fn disabled_healthcheck_is_dropped() -> anyhow::Result<()> {
        let outcome = import_compose(
            "media",
            "services:\n  web:\n    image: example/web:1.2\n    healthcheck:\n      disable: true\n",
            "",
        )?;
        assert!(outcome.spec.document.services["web"].healthcheck.is_none());
        Ok(())
    }

    /// 受支持字段本身无效时仍然报错，不静默丢弃。
    #[test]
    fn invalid_supported_field_still_fails() {
        assert!(
            import_compose(
                "media",
                "services:\n  web:\n    image: example/web:latest\n",
                ""
            )
            .is_err()
        );
    }
}
