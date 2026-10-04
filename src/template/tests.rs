//! `template` 模块行为测试。

use super::{TemplateKind, apply, export};
use crate::config::Config;

/// Authelia OIDC provider 与应用客户端行为。
mod oidc;

/// 通用应用骨架本身可直接应用，且不会启用注释中的有副作用选项。
#[test]
fn app_skeleton_is_valid_with_optional_defaults_disabled() -> anyhow::Result<()> {
    let generated = apply(super::skeleton::APP_SKELETON, &Config::default(), None)?;
    assert_eq!(generated.kind, TemplateKind::App);
    let service = &generated.spec.document.services["web"];
    assert!(service.container_name.is_none());
    assert!(service.ports.is_empty());
    assert!(service.volumes.is_empty());
    assert!(service.environment.is_empty());
    assert!(service.command.is_empty());
    assert!(service.restart.is_none());
    assert!(service.env_file.is_empty());
    assert!(service.healthcheck.is_none());
    assert!(service.logging.is_none());
    let routes = service.routes("media", "web")?;
    assert_eq!(routes.len(), 1);
    assert!(routes[0].middlewares.is_empty());
    assert!(!routes[0].sticky_cookie);
    assert!(routes[0].pass_host_header.is_none());
    assert!(routes[0].priority.is_none());
    Ok(())
}

/// 骨架展示应用模型的全部字段和详细路由字段。
#[test]
fn app_skeleton_documents_all_fields() {
    for example in [
        "format = 1",
        "# template = \"app\"",
        "name = \"media\"",
        "[services.web]",
        "image = \"ghcr.io/example/media\"",
        "version = \"1.0\"",
        "# container_name = \"media\"",
        "port = 8080",
        "# publish =",
        "# volumes =",
        "# environment =",
        "# command =",
        "# restart =",
        "# env_file =",
        "# network =",
        "# external_network =",
        "# labels =",
        "# [services.web.healthcheck]",
        "# command = \"wget",
        "# interval =",
        "# timeout =",
        "# start_period =",
        "# retries =",
        "# [services.web.logging]",
        "# driver =",
        "# options =",
        "[services.web.traefik]",
        "hosts = [\"media\"]",
        "# path_prefix =",
        "# middlewares =",
        "# protocol =",
        "# sticky_cookie =",
        "# pass_host_header =",
        "# priority =",
        "# [services.web.traefik.routes.api]",
        "# port = 9090",
        "# user = \"0:0\"",
        "# group_add =",
        "# [services.web.hooks]",
        "# pre_start =",
        "# post_start =",
        "# command = [\"/usr/bin/curl\"",
        "# entrypoint = \"https\"",
        "--files",
    ] {
        assert!(
            super::skeleton::APP_SKELETON.contains(example),
            "应用骨架缺少字段示例: {example}"
        );
    }
}

#[test]
fn app_template_round_trip() -> anyhow::Result<()> {
    let input = r#"
format = 1
name = "demo"
[services.web]
image = "example/web"
version = "1.2"
port = 8080
[services.web.traefik]
hosts = ["demo"]
middlewares = ["gzip"]
"#;
    let config = Config::default();
    let generated = apply(input, &config, None)?;
    assert_eq!(generated.kind, TemplateKind::App);
    let exported = export(&generated.spec, &config)?;
    let regenerated = apply(&exported, &config, None)?;
    assert_eq!(generated.spec, regenerated.spec);
    Ok(())
}

/// 应用路由可引用由基础设施提供的 Authelia 中间件。
#[test]
fn app_template_uses_authelia_middleware() -> anyhow::Result<()> {
    let input = r#"
format = 1
name = "protected"
[services.web]
image = "example/web"
version = "1.2"
port = 8080
[services.web.traefik]
hosts = ["protected"]
middlewares = ["authelia", "tls"]
"#;
    let generated = apply(input, &Config::default(), None)?;
    let labels = &generated.spec.document.services["web"].labels;
    assert!(labels.iter().any(|label| {
        label
            == "traefik.http.routers.nsetup-protected-web-default.middlewares=authelia@file,tls@file"
    }));
    Ok(())
}

/// 带路由的服务同时接入代理网络与项目网络，其余服务留在项目网络。
#[test]
fn routed_service_joins_proxy_and_project_networks() -> anyhow::Result<()> {
    let input = r#"
format = 1
name = "observability"
[services.grafana]
image = "grafana/grafana"
version = "13.2.0"
port = 3000
[services.grafana.traefik]
hosts = ["grafana"]
[services.prometheus]
image = "prom/prometheus"
version = "v3.13.3"
"#;
    let generated = apply(input, &Config::default(), None)?;
    let document = &generated.spec.document;
    let grafana = &document.services["grafana"];
    assert_eq!(grafana.networks, vec!["proxy", "project"]);
    assert!(
        grafana
            .labels
            .iter()
            .any(|label| label == "traefik.enable=true")
    );
    assert!(
        grafana
            .labels
            .iter()
            .any(|label| label == "traefik.docker.network=nsetup-proxy"),
        "多网络服务必须固定 Traefik 回源网络: {:?}",
        grafana.labels
    );
    let prometheus = &document.services["prometheus"];
    assert!(prometheus.networks.is_empty());
    assert!(
        !prometheus
            .labels
            .iter()
            .any(|label| label.starts_with("traefik.")),
        "无路由的服务不应携带任何 Traefik label: {:?}",
        prometheus.labels
    );
    assert_eq!(
        document.networks["proxy"].name.as_deref(),
        Some("nsetup-proxy")
    );
    assert!(document.networks["proxy"].external);
    assert_eq!(
        document.networks["project"].name.as_deref(),
        Some("observability_default")
    );
    assert!(!document.networks["project"].external);
    Ok(())
}

/// 历史单网络项目不会平白多出 Traefik 回源网络标签。
#[test]
fn single_network_service_skips_traefik_network_label() -> anyhow::Result<()> {
    let route = crate::spec::Route {
        name: String::from("default"),
        hosts: vec![String::from("legacy.example.com")],
        path_prefix: None,
        container_port: 8080,
        middlewares: Vec::new(),
        protocol: crate::spec::RouteProtocol::Http,
        entrypoint: String::from("https"),
        sticky_cookie: false,
        pass_host_header: None,
        priority: None,
    };
    let mut single = crate::spec::Service {
        image: String::from("example/legacy:1.0"),
        networks: vec![String::from("proxy")],
        ..crate::spec::Service::default()
    };
    single.set_routes("legacy", "web", std::slice::from_ref(&route))?;
    assert!(
        !single
            .labels
            .iter()
            .any(|label| label.starts_with("traefik.docker.network"))
    );

    let mut dual = crate::spec::Service {
        image: String::from("example/legacy:1.0"),
        networks: vec![String::from("proxy"), String::from("project")],
        ..crate::spec::Service::default()
    };
    dual.set_routes("legacy", "web", &[route])?;
    assert!(
        dual.labels
            .iter()
            .any(|label| label == "traefik.docker.network=nsetup-proxy")
    );
    Ok(())
}

/// 多个子路由按名称绑定域名和端口，生成标签与导出都不依赖声明顺序。
#[test]
fn named_routes_keep_host_port_mapping() -> anyhow::Result<()> {
    let input = r#"
format = 1
name = "storage"
[services.gateway]
image = "example/storage"
version = "1.2"

[services.gateway.traefik.routes.api]
hosts = ["s3"]
port = 9000

[services.gateway.traefik.routes.console]
hosts = ["s3c"]
port = 9001
"#;
    let config = Config::default();
    let generated = apply(input, &config, None)?;
    let service = &generated.spec.document.services["gateway"];
    let routes = service.routes("storage", "gateway")?;
    assert_eq!(routes[0].name, "api");
    assert_eq!(routes[0].hosts, ["s3.example.com"]);
    assert_eq!(routes[0].container_port, 9000);
    assert_eq!(routes[1].name, "console");
    assert_eq!(routes[1].hosts, ["s3c.example.com"]);
    assert_eq!(routes[1].container_port, 9001);
    assert!(service.labels.iter().any(|label| {
        label == "traefik.http.services.nsetup-storage-gateway-api.loadbalancer.server.port=9000"
    }));
    assert!(service.labels.iter().any(|label| {
        label
            == "traefik.http.services.nsetup-storage-gateway-console.loadbalancer.server.port=9001"
    }));

    let exported = export(&generated.spec, &config)?;
    assert!(exported.contains("[services.gateway.traefik.routes.api]"));
    assert!(exported.contains("[services.gateway.traefik.routes.console]"));
    assert!(!exported.contains("[[services.gateway.traefik.routes]]"));
    assert_eq!(generated, apply(&exported, &config, None)?);
    Ok(())
}

/// 顺序数组路由已被具名映射替代，不再隐式生成数字身份。
#[test]
fn legacy_positional_routes_are_rejected() {
    let input = r#"
format = 1
name = "storage"
[services.gateway]
image = "example/storage"
version = "1.2"

[[services.gateway.traefik.routes]]
hosts = ["s3"]
port = 9000
"#;
    assert!(apply(input, &Config::default(), None).is_err());
}

/// Authelia 的声明、Compose 状态、用户库和文件型密钥可稳定重建。
#[test]
fn authelia_template_round_trip() -> anyhow::Result<()> {
    let input = valid_authelia_config();
    let config = Config::default();
    let generated = apply(input, &config, None)?;
    assert_eq!(generated.kind, TemplateKind::Authelia);
    assert_eq!(generated.spec.name, "authelia");
    let service = &generated.spec.document.services["authelia"];
    assert_eq!(service.image, "authelia/authelia:${AUTHELIA_VERSION}");
    assert!(
        service
            .volumes
            .iter()
            .any(|mount| mount.ends_with("/authelia:/data"))
    );
    let jwt = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("JWT_SECRET"))
        .ok_or_else(|| anyhow::anyhow!("missing JWT_SECRET"))?;
    assert_eq!(jwt.mode, 0o600);
    let configuration = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("configuration.yml"))
        .ok_or_else(|| anyhow::anyhow!("missing configuration.yml"))?;
    let configuration = String::from_utf8(configuration.content.clone())?;
    let _configuration_yaml: serde_yaml::Value = serde_yaml::from_str(&configuration)?;
    assert!(configuration.contains("implementation: 'ForwardAuth'"));
    assert!(configuration.contains("default_2fa_method: 'totp'"));
    assert!(configuration.contains("totp:\n  disable: false"));
    assert!(configuration.contains("webauthn:\n  disable: true"));
    assert!(!configuration.contains("jwt-secret-value"));
    let exported = export(&generated.spec, &config)?;
    let regenerated = apply(&exported, &config, None)?;
    assert_eq!(generated, regenerated);
    Ok(())
}

/// Authelia 模板拒绝占位密钥和占位密码哈希。
#[test]
fn authelia_template_rejects_placeholders() {
    let result = apply(super::skeleton::AUTHELIA_SKELETON, &Config::default(), None);
    assert!(result.is_err());
}

/// Authelia 骨架明确说明存储密钥不能随普通重新应用而变化。
#[test]
fn authelia_skeleton_documents_storage_key_stability() {
    let skeleton = super::skeleton::AUTHELIA_SKELETON;
    assert!(skeleton.contains("storage_encryption_key 在数据库首次初始化后必须保持不变"));
    assert!(skeleton.contains("authelia storage encryption change-key"));
}

/// Traefik 状态、密钥与模板类型在 IR 导出后保持不变。
#[test]
fn traefik_template_round_trip() -> anyhow::Result<()> {
    let input = r#"
format = 1
template = "traefik"
domain = "example.com"
acme_email = "admin@example.com"
cloudflare_token = "secret"
version = "v3.8.0"
http_port = 8080
https_port = 8443
dashboard_authelia = true
"#;
    let config = Config::default();
    let generated = apply(input, &config, None)?;
    assert_eq!(generated.kind, TemplateKind::Traefik);
    let service = &generated.spec.document.services["traefik"];
    assert!(service.labels.contains(&String::from(
        "traefik.http.routers.dashboard.service=api@internal"
    )));
    assert!(service.labels.contains(&String::from(
        "traefik.http.routers.dashboard.middlewares=internal-only@file,authelia@file"
    )));
    assert!(service.labels.iter().all(|label| {
        !label.starts_with("traefik.http.services.dashboard.loadbalancer.server.port=")
    }));
    assert!(service.command.contains(&String::from(
        "--certificatesresolvers.cloudflare.acme.keytype=EC256"
    )));
    assert!(service.command.contains(&String::from(
        "--certificatesresolvers.cloudflare.acme.dnschallenge.propagation.delaybeforechecks=30s"
    )));
    assert_eq!(
        service.logging.as_ref().map(|value| value.driver.as_str()),
        Some("json-file")
    );
    assert!(service.healthcheck.is_some());
    let acme = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("acme.json"))
        .ok_or_else(|| anyhow::anyhow!("missing acme.json"))?;
    assert!(!acme.replace);
    let dynamic = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("dynamic/nsetup.yml"))
        .ok_or_else(|| anyhow::anyhow!("missing dynamic/nsetup.yml"))?;
    let dynamic = String::from_utf8(dynamic.content.clone())?;
    assert!(dynamic.contains("address: 'http://authelia:9091/api/authz/forward-auth'"));
    assert!(dynamic.contains("X-Forwarded-Port: '8443'"));
    assert!(!dynamic.contains("defaultGeneratedCert"));
    let user_file = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("dynamic/custom.yml"))
        .ok_or_else(|| anyhow::anyhow!("missing dynamic/custom.yml"))?;
    assert!(!user_file.replace, "用户可编辑的动态配置不能被整体替换");
    assert!(service.command.contains(&String::from(
        "--providers.file.directory=/etc/traefik/dynamic"
    )));
    assert!(
        service
            .command
            .contains(&String::from("--metrics.prometheus=true"))
    );
    assert!(
        service
            .ports
            .iter()
            .any(|port| port == "127.0.0.1:8081:8081/tcp")
    );
    assert!(
        !service
            .command
            .iter()
            .any(|argument| { argument == "--metrics.prometheus=false" })
    );
    let exported = export(&generated.spec, &config)?;
    assert!(exported.contains("dashboard_authelia = true"));
    let regenerated = apply(&exported, &config, None)?;
    assert_eq!(generated.spec, regenerated.spec);
    Ok(())
}

/// Traefik / Authelia 骨架可以直接应用，且核心开关真的落到 command/配置里（R1）。
#[test]
fn infrastructure_skeletons_apply_and_round_trip() -> anyhow::Result<()> {
    let config = Config::default();
    let traefik = apply(super::skeleton::TRAEFIK_SKELETON, &config, None)?;
    assert_eq!(traefik.kind, TemplateKind::Traefik);
    assert_eq!(traefik.spec.name, "traefik");
    let service = &traefik.spec.document.services["traefik"];
    assert!(
        service
            .command
            .contains(&String::from("--metrics.prometheus=true"))
    );
    assert!(
        service
            .ports
            .iter()
            .any(|port| port == "127.0.0.1:8081:8081/tcp")
    );
    let exported = export(&traefik.spec, &config)?;
    assert!(exported.contains("name = \"traefik\""));
    let regenerated = apply(&exported, &config, None)?;
    assert_eq!(traefik.spec, regenerated.spec);

    let authelia_input = super::skeleton::AUTHELIA_SKELETON
        .replace(
            "replace-with-at-least-32-random-characters",
            "0123456789abcdef0123456789abcdef",
        )
        .replace(
            "'$argon2id$replace-with-generated-password-hash'",
            "'$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$aGFzaA'",
        );
    let authelia = apply(&authelia_input, &config, None)?;
    assert_eq!(authelia.kind, TemplateKind::Authelia);
    assert_eq!(authelia.spec.name, "authelia");
    let configuration = authelia
        .files
        .iter()
        .find(|file| file.path.ends_with("config/configuration.yml"))
        .ok_or_else(|| anyhow::anyhow!("Authelia 模板缺少 configuration.yml"))?;
    let configuration = String::from_utf8(configuration.content.clone())?;
    assert!(configuration.contains("forward-auth"));
    let exported = export(&authelia.spec, &config)?;
    assert!(exported.contains("name = \"authelia\""));
    let regenerated = apply(&exported, &config, None)?;
    assert_eq!(authelia.spec, regenerated.spec);
    Ok(())
}

/// traefik / authelia 的 `name` 可以省略、也可以显式写出，两者语义一致（R1）。
#[test]
fn infrastructure_templates_accept_optional_name() -> anyhow::Result<()> {
    let skeleton = super::skeleton::TRAEFIK_SKELETON.replace("name = \"traefik\"\n", "");
    let generated = apply(&skeleton, &Config::default(), None)?;
    assert_eq!(generated.spec.name, "traefik");
    assert_eq!(generated.kind, TemplateKind::Traefik);

    let explicit = apply(super::skeleton::TRAEFIK_SKELETON, &Config::default(), None)?;
    assert_eq!(generated.spec, explicit.spec);

    let wrong = apply(
        &super::skeleton::TRAEFIK_SKELETON.replace("name = \"traefik\"", "name = \"proxy\""),
        &Config::default(),
        None,
    );
    assert!(wrong.is_err(), "固定项目名的模板不应接受其它 name");
    Ok(())
}

/// 未开启 Authelia 时 dashboard 仍只允许内网访问。
#[test]
fn traefik_dashboard_authelia_defaults_to_disabled() -> anyhow::Result<()> {
    let input = r#"
format = 1
template = "traefik"
domain = "example.com"
acme_email = "admin@example.com"
cloudflare_token = "secret"
version = "v3.8.0"
"#;
    let generated = apply(input, &Config::default(), None)?;
    let labels = &generated.spec.document.services["traefik"].labels;
    assert!(labels.contains(&String::from(
        "traefik.http.routers.dashboard.middlewares=internal-only@file"
    )));
    assert!(labels.iter().all(|label| !label.contains("authelia@file")));
    Ok(())
}

/// 返回包含有效占位测试值的 Authelia TOML。
fn valid_authelia_config() -> &'static str {
    r#"
format = 1
template = "authelia"
host = "auth"
version = "4.39.20"
default_redirection_url = "https://example.com"
default_policy = "two_factor"
jwt_secret = "jwt-secret-value-0123456789-abcdef"
session_secret = "session-secret-value-0123456789-ab"
storage_encryption_key = "storage-secret-value-0123456789-a"

[users.admin]
display_name = "Administrator"
password_hash = '$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$aGFzaA'
email = "admin@example.com"
groups = ["admins"]
"#
}

/// 静态模板保留镜像版本、主机名与中间件语义。
#[test]
fn static_template_round_trip() -> anyhow::Result<()> {
    let input = r#"
format = 1
template = "static"
name = "docs"
host = "docs"
version = "1.27"
middlewares = ["gzip"]
"#;
    let config = Config::default();
    let generated = apply(input, &config, None)?;
    let exported = export(&generated.spec, &config)?;
    let regenerated = apply(&exported, &config, None)?;
    assert_eq!(generated.spec, regenerated.spec);
    Ok(())
}

/// 同一 host 上的多条路由按 `path_prefix` 与协议共存，且自定义中间件可被引用（A1/A4）。
#[test]
fn same_host_paths_and_custom_middlewares() -> anyhow::Result<()> {
    let input = r#"
format = 1
name = "netbird"
[services.dashboard]
image = "netbirdio/dashboard"
version = "2.0"
port = 80
labels = ["traefik.enable=true"]
[services.dashboard.traefik.routes.web]
hosts = ["netbird"]
path_prefix = "/"
[services.dashboard.traefik.routes.api]
hosts = ["netbird"]
path_prefix = "/api"
middlewares = ["replace-path", "authelia"]
[services.dashboard.traefik.routes.grpc]
hosts = ["netbird"]
path_prefix = "/grpc"
port = 10000
protocol = "h2c"
"#;
    let config = Config::default();
    let generated = apply(input, &config, None)?;
    let service = &generated.spec.document.services["dashboard"];
    let routes = service.routes("netbird", "dashboard")?;
    assert_eq!(routes.len(), 3);
    let mut identities: Vec<String> = service
        .routes("netbird", "dashboard")?
        .iter()
        .flat_map(|route| route.identities())
        .map(|identity| identity.describe())
        .collect();
    identities.sort();
    identities.dedup();
    assert_eq!(
        identities.len(),
        3,
        "三条路由身份必须互不相同: {identities:?}"
    );
    assert!(service.labels.iter().any(|label| {
        label
            == "traefik.http.routers.nsetup-netbird-dashboard-api.middlewares=replace-path@file,authelia@file"
    }));
    let exported = export(&generated.spec, &config)?;
    assert!(
        exported.contains("[services.dashboard.traefik.routes.api]")
            && exported.contains("\"replace-path\",\n    \"authelia\","),
        "具名路由必须导出自定义中间件:\n{exported}"
    );
    assert_eq!(generated, apply(&exported, &config, None)?);
    Ok(())
}

/// 没有 shell 的镜像可以使用 argv 形式的健康检查（B4）。
#[test]
fn exec_healthcheck_round_trip() -> anyhow::Result<()> {
    let input = r#"
format = 1
name = "matrix"
[services.tuwunel]
image = "ghcr.io/matrix-construct/tuwunel"
version = "1.0"
user = "0:0"
group_add = ["988"]
[services.tuwunel.healthcheck]
command = ["/usr/bin/curl", "-f", "http://127.0.0.1:8008/health"]
interval = "30s"
retries = 3
"#;
    let config = Config::default();
    let generated = apply(input, &config, None)?;
    let service = &generated.spec.document.services["tuwunel"];
    let healthcheck = service
        .healthcheck
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing healthcheck"))?;
    assert_eq!(healthcheck.test[0], "CMD");
    assert_eq!(
        healthcheck.exec_arguments()?,
        ["/usr/bin/curl", "-f", "http://127.0.0.1:8008/health"]
    );
    assert_eq!(service.user.as_deref(), Some("0:0"));
    assert_eq!(service.group_add, ["988"]);
    let exported = export(&generated.spec, &config)?;
    assert!(exported.contains("command = ["));
    assert!(exported.contains("user = \"0:0\""));
    assert!(exported.contains("group_add = [\"988\"]"));
    assert_eq!(generated, apply(&exported, &config, None)?);
    Ok(())
}

/// 不声明健康检查时不能覆盖镜像自带探针（B4）。
#[test]
fn omitted_healthcheck_is_not_generated() -> anyhow::Result<()> {
    let input = r#"
format = 1
name = "plain"
[services.web]
image = "example/web"
version = "1.0"
"#;
    let generated = apply(input, &Config::default(), None)?;
    assert!(
        generated.spec.document.services["web"]
            .healthcheck
            .is_none(),
        "未声明 healthcheck 时不能生成任何探针"
    );
    Ok(())
}

/// 启动钩子可往返导出，并且不进入容器环境变量（B6）。
#[test]
fn service_hooks_round_trip() -> anyhow::Result<()> {
    let input = r#"
format = 1
name = "gitea"
[services.gitea]
image = "gitea/gitea"
version = "1.24"
[services.gitea.hooks]
pre_start = ["install -d -m 0755 data"]
post_start = ["docker exec gitea gitea admin user create --admin"]
"#;
    let config = Config::default();
    let generated = apply(input, &config, None)?;
    let hooks = generated.spec.project_hooks()?;
    assert_eq!(hooks["gitea"].pre_start, ["install -d -m 0755 data"]);
    assert_eq!(hooks.len(), 1);
    let exported = export(&generated.spec, &config)?;
    assert!(exported.contains("[services.gitea.hooks]"));
    assert_eq!(generated, apply(&exported, &config, None)?);
    Ok(())
}

/// `--files` 上传的目录以只读方式挂进每个服务（B1）。
#[test]
fn files_directory_is_mounted_read_only() -> anyhow::Result<()> {
    let input = r#"
format = 1
name = "netbird"
[services.dashboard]
image = "netbirdio/dashboard"
version = "2.0"
"#;
    let config = Config {
        stacks_root: "/srv/stacks".into(),
        ..Config::default()
    };
    let generated = apply(input, &config, Some("/opt/nsetup/files"))?;
    let volumes = &generated.spec.document.services["dashboard"].volumes;
    assert_eq!(
        volumes.as_slice(),
        ["/srv/stacks/netbird/files:/opt/nsetup/files:ro"]
    );
    assert!(
        !export(&generated.spec, &config)?.contains("/opt/nsetup/files"),
        "files/ 挂载不应重复导出为普通挂载"
    );
    Ok(())
}

/// `export --keep-comments` 追加带注释骨架且仍是合法 TOML（C4）。
#[test]
fn annotated_export_stays_parseable() -> anyhow::Result<()> {
    let input = r#"
format = 1
name = "media"
[services.web]
image = "example/web"
version = "1.0"
port = 8080
[services.web.traefik]
hosts = ["media"]
"#;
    let config = Config::default();
    let generated = apply(input, &config, None)?;
    let exported = export(&generated.spec, &config)?;
    let annotated = super::annotate(&exported, TemplateKind::App)?;
    assert!(annotated.contains("# 通用容器应用模板"));
    assert!(annotated.contains("# format = 1"));
    assert!(annotated.contains("name = \"media\""));
    let reparsed: toml::Value = toml::from_str(&annotated)?;
    assert_eq!(
        reparsed.get("name").and_then(toml::Value::as_str),
        Some("media")
    );
    assert!(super::annotate("not toml", TemplateKind::App).is_err());
    Ok(())
}

/// traefik 模板把自定义中间件写进拥有的动态配置，并保留用户目录条目（A4/A5）。
#[test]
fn traefik_custom_middleware_is_rendered() -> anyhow::Result<()> {
    let input = r#"
format = 1
template = "traefik"
domain = "example.com"
acme_email = "admin@example.com"
cloudflare_token = "secret"
version = "v3.8.0"
metrics = false

[middlewares.replace-path]
kind = "replacePath"
args = { path = "/status" }

[middlewares.strip-api]
kind = "stripPrefix"
args = { prefixes = ["/api", "/v1"], forceSlash = true }
"#;
    let config = Config::default();
    let generated = apply(input, &config, None)?;
    let dynamic = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("dynamic/nsetup.yml"))
        .ok_or_else(|| anyhow::anyhow!("missing dynamic/nsetup.yml"))?;
    let dynamic = String::from_utf8(dynamic.content.clone())?;
    let _yaml: serde_yaml::Value = serde_yaml::from_str(&dynamic)?;
    assert!(dynamic.contains("    replace-path:\n      replacePath:\n        path: /status"));
    assert!(dynamic.contains("        prefixes:\n          - /api\n          - /v1\n"));
    assert!(dynamic.contains("        forceSlash: true\n"));
    let service = &generated.spec.document.services["traefik"];
    assert!(
        !service
            .command
            .iter()
            .any(|argument| argument == "--metrics.prometheus=true")
    );
    assert!(
        !service
            .ports
            .iter()
            .any(|port| port.starts_with("127.0.0.1:8081"))
    );
    let exported = export(&generated.spec, &config)?;
    assert!(exported.contains("[middlewares.replace-path]"));
    assert!(exported.contains("metrics = false"));
    assert_eq!(generated, apply(&exported, &config, None)?);
    Ok(())
}

/// Authelia 遥测默认关闭，声明后写进生成的 YAML（D4）。
#[test]
fn authelia_telemetry_is_optional() -> anyhow::Result<()> {
    let base = r#"
format = 1
template = "authelia"
version = "4.39.20"
default_redirection_url = "https://example.com"
jwt_secret = "jwt-secret-value-0123456789-abcdef"
session_secret = "session-secret-value-0123456789-ab"
storage_encryption_key = "storage-secret-value-0123456789-a"
[users.admin]
display_name = "Administrator"
password_hash = '$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$aGFzaA'
email = "admin@example.com"
"#;
    let config = Config::default();
    let without = apply(base, &config, None)?;
    let configuration = without
        .files
        .iter()
        .find(|file| file.path.ends_with("configuration.yml"))
        .ok_or_else(|| anyhow::anyhow!("missing configuration.yml"))?;
    let configuration = String::from_utf8(configuration.content.clone())?;
    assert!(!configuration.contains("telemetry:"));

    let with = format!(
        "{base}\n[telemetry]\nmetrics_address = \"tcp://0.0.0.0:9959\"\ntracing_address = \"udp://otel-collector:4318\"\ntracing_sample_rate = 0.5\n"
    );
    let generated = apply(&with, &config, None)?;
    let configuration = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("configuration.yml"))
        .ok_or_else(|| anyhow::anyhow!("missing configuration.yml"))?;
    let configuration = String::from_utf8(configuration.content.clone())?;
    let _yaml: serde_yaml::Value = serde_yaml::from_str(&configuration)?;
    assert!(configuration.contains(
        "telemetry:\n  metrics:\n    enabled: true\n    address: 'tcp://0.0.0.0:9959'\n"
    ));
    assert!(configuration.contains("    sample_rate: 0.5\n"));
    let exported = export(&generated.spec, &config)?;
    assert!(exported.contains("[telemetry]"));
    assert_eq!(generated, apply(&exported, &config, None)?);
    Ok(())
}

/// Traefik 与 Authelia 骨架说明 0.2.0 新增的可观测性与目录配置。
#[test]
fn infrastructure_skeletons_document_new_options() {
    for example in [
        "# metrics = true",
        "# metrics_port = 8081",
        "# [middlewares.replace-path]",
        "config/dynamic/nsetup.yml",
        "custom.yml",
    ] {
        assert!(
            super::skeleton::TRAEFIK_SKELETON.contains(example),
            "Traefik 骨架缺少字段示例: {example}"
        );
    }
    for example in [
        "# [telemetry]",
        "# metrics_address =",
        "# tracing_address =",
        "require_pkce = true 时才输出 pkce_challenge_method",
    ] {
        assert!(
            super::skeleton::AUTHELIA_SKELETON.contains(example),
            "Authelia 骨架缺少字段示例: {example}"
        );
    }
    for example in [
        "--assets-mode merge",
        "/usr/share/nginx/html",
        "/opt/nsetup",
        "config/nginx/default.conf",
    ] {
        assert!(
            super::skeleton::STATIC_SKELETON.contains(example),
            "静态骨架缺少说明: {example}"
        );
    }
}

/// 站点资源的权限策略：默认 a+rX，可显式收紧（R2）。
#[test]
fn asset_permissions_default_to_world_readable() {
    use super::{ASSET_DIRECTORY_MODE, ASSET_FILE_MODE, AssetPermissions};
    assert_eq!(
        AssetPermissions::default().modes(),
        (ASSET_DIRECTORY_MODE, ASSET_FILE_MODE)
    );
    assert_eq!(AssetPermissions::default().modes(), (0o755, 0o644));
    assert_eq!(AssetPermissions::Private.modes(), (0o750, 0o640));
}

/// static 模板支持 `user` / `group_add` / `hooks`，站点侧无需外部脚本修权限（R2）。
#[test]
fn static_template_supports_user_groups_and_hooks() -> anyhow::Result<()> {
    let input = r#"
format = 1
template = "static"
name = "docs"
host = "docs"
version = "1.27"
user = "101:101"
group_add = ["988"]
[hooks]
pre_start = ["chmod -R a+rX site"]
post_start = ["docker exec docs-web-1 nginx -s reload"]
"#;
    let config = Config::default();
    let generated = apply(input, &config, None)?;
    let service = &generated.spec.document.services["web"];
    assert_eq!(service.user.as_deref(), Some("101:101"));
    assert_eq!(service.group_add, vec![String::from("988")]);
    let pre_start = generated
        .spec
        .service_hooks(crate::spec::HookStage::PreStart)?;
    assert_eq!(pre_start.len(), 1);
    assert_eq!(pre_start[0].1, "chmod -R a+rX site");
    let post_start = generated
        .spec
        .service_hooks(crate::spec::HookStage::PostStart)?;
    assert_eq!(post_start.len(), 1);

    let exported = export(&generated.spec, &config)?;
    assert!(exported.contains("user = \"101:101\""));
    let regenerated = apply(&exported, &config, None)?;
    assert_eq!(generated.spec, regenerated.spec);
    Ok(())
}

/// Authelia 的 config 目录必须可写，否则官方 entrypoint 的 chown 会刷只读报错（R8）。
#[test]
fn authelia_config_mount_is_writable() -> anyhow::Result<()> {
    let input = r#"
format = 1
template = "authelia"
version = "4.39.20"
default_redirection_url = "https://example.com"
jwt_secret = "0123456789abcdef0123456789abcdef"
session_secret = "1123456789abcdef0123456789abcdef"
storage_encryption_key = "2123456789abcdef0123456789abcdef"
[users.admin]
display_name = "Administrator"
password_hash = '$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$aGFzaA'
email = "admin@example.com"
"#;
    let generated = apply(input, &Config::default(), None)?;
    let volumes = &generated.spec.document.services["authelia"].volumes;
    let config_mount = volumes
        .iter()
        .find(|value| value.ends_with(":/config"))
        .ok_or_else(|| anyhow::anyhow!("Authelia 缺少 /config 挂载: {volumes:?}"))?;
    assert!(
        !config_mount.ends_with(":ro"),
        "/config 必须以可写方式挂载: {config_mount}"
    );
    assert!(
        volumes.iter().any(|value| value.ends_with(":/secrets:ro")),
        "secrets 目录必须保持只读: {volumes:?}"
    );
    Ok(())
}
