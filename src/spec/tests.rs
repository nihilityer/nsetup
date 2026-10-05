//! `spec` 模块行为测试。

use super::project::{parse_env_file, serialize_env_file};
use super::{Document, HTTPS_ENTRYPOINT, Route, RouteIdentity, RouteProtocol, Service, StackSpec};
use std::collections::BTreeMap;

#[test]
fn compose_unknown_field_is_rejected() {
    let result = StackSpec::parse(
        "demo",
        "services:\n  app:\n    image: example/app:1\n    depends_on: [db]\n",
        "",
    );
    assert!(result.is_err());
}

/// 内置后端路由（无 `loadbalancer.server.port`）往返 label 与 YAML（R10）。
#[test]
fn internal_backend_route_round_trips() -> anyhow::Result<()> {
    let expected = Route {
        name: String::from("dashboard"),
        hosts: vec![String::from("traefik.example.com")],
        path_prefix: None,
        container_port: None,
        middlewares: vec![String::from("internal-only")],
        protocol: RouteProtocol::Http,
        entrypoint: String::from(HTTPS_ENTRYPOINT),
        priority: Some(1000),
        service: Some(String::from("api@internal")),
        ..Route::default()
    };
    let mut service = Service {
        image: String::from("traefik:v3.8.0"),
        ..Service::default()
    };
    service.set_routes("traefik", "traefik", std::slice::from_ref(&expected))?;
    assert!(service.labels.contains(&String::from(
        "traefik.http.routers.nsetup-traefik-traefik-dashboard.service=api@internal"
    )));
    assert!(service.labels.iter().all(|label| {
        !label.contains("traefik.http.services.nsetup-traefik-traefik-dashboard.loadbalancer")
    }));
    assert_eq!(
        service.routes("traefik", "traefik")?,
        vec![expected.clone()]
    );

    let document = Document {
        services: BTreeMap::from([(String::from("traefik"), service)]),
        networks: BTreeMap::new(),
    };
    let yaml = serde_yaml::to_string(&document)?;
    let parsed: Document = serde_yaml::from_str(&yaml)?;
    assert_eq!(
        parsed.services["traefik"].routes("traefik", "traefik")?,
        vec![expected],
        "{yaml}"
    );
    Ok(())
}

/// 内置后端服务名必须是 `name@provider` 形式，否则 Traefik 会静默丢掉路由。
#[test]
fn rejects_internal_backend_without_provider() {
    for service in ["api", "api@", "@internal", "api internal@internal"] {
        let route = Route {
            name: String::from("dashboard"),
            hosts: vec![String::from("traefik.example.com")],
            container_port: None,
            service: Some(service.to_string()),
            ..Route::default()
        };
        assert!(route.validate().is_err(), "{service} 应当被拒绝");
    }
}

#[test]
fn routes_round_trip_through_labels() -> anyhow::Result<()> {
    let mut service = Service {
        image: String::from("example/app:1"),
        ..Service::default()
    };
    let expected = Route {
        name: String::from("default"),
        hosts: vec![String::from("app.example.com")],
        path_prefix: Some(String::from("/api")),
        container_port: Some(8080),
        middlewares: vec![String::from("gzip")],
        protocol: RouteProtocol::H2c,
        entrypoint: String::from("https"),
        sticky_cookie: true,
        pass_host_header: Some(false),
        priority: Some(100),
        ..Route::default()
    };
    service.set_routes("demo", "web", std::slice::from_ref(&expected))?;
    assert!(service.labels.iter().any(|label| {
        label == "traefik.http.routers.nsetup-demo-web-default.tls.certresolver=cloudflare"
    }));
    assert!(service.labels.iter().any(|label| {
        label == "traefik.http.routers.nsetup-demo-web-default.entrypoints=https"
    }));
    assert_eq!(service.routes("demo", "web")?, vec![expected]);
    let spec = StackSpec {
        name: String::from("demo"),
        document: Document {
            services: BTreeMap::from([(String::from("web"), service)]),
            networks: BTreeMap::new(),
        },
        environment: BTreeMap::new(),
    };
    spec.validate()?;
    Ok(())
}

/// 镜像钉版本同时拒绝缺少标签和可变的 `latest` 标签。
#[test]
fn rejects_unpinned_images() {
    for image in ["example/app", "example/app:latest"] {
        let compose = format!("services:\n  app:\n    image: {image}\n");
        assert!(StackSpec::parse("demo", &compose, "").is_err());
    }
}

/// 容器环境文件不能越出受管项目目录。
#[test]
fn rejects_escaping_env_file() {
    let compose = "services:\n  app:\n    image: example/app:1\n    env_file: [../secret]\n";
    assert!(StackSpec::parse("demo", compose, "").is_err());
}

/// 受管 `.env` 转义应保留空格、引号、换行符和字面量美元符号。
#[test]
fn env_file_round_trip() -> anyhow::Result<()> {
    let parsed = parse_env_file(
        "A=plain\nB=\"two words\"\nC=\"line\\nnext\"\nD='$argon2id$v=19'\nE='owner\\'s $value'\n",
    )?;
    let normalized = serialize_env_file(&parsed);
    assert!(normalized.contains("D='$argon2id$v=19'"));
    assert!(normalized.contains("E='owner\\'s $value'"));
    assert_eq!(parse_env_file(&normalized)?, parsed);
    Ok(())
}

/// 由变量支撑的模板镜像不能按字面镜像标签编辑。
#[test]
fn rejects_version_edit_for_variable_image() {
    let mut service = Service {
        image: String::from("nginx:${NGINX_VERSION}"),
        ..Service::default()
    };
    assert!(service.set_image_version(None, Some("1.28")).is_err());
}

/// 同一 host 下不同 `path_prefix` 必须被判定为不同路由（A1）。
#[test]
fn same_host_different_paths_are_distinct() -> anyhow::Result<()> {
    let plain = RouteIdentity::new("media.example.com", None, "https", RouteProtocol::Http);
    let api = RouteIdentity::new(
        "media.example.com",
        Some("/api"),
        "https",
        RouteProtocol::Http,
    );
    let api_trailing = RouteIdentity::new(
        "media.example.com",
        Some("/api/"),
        "https",
        RouteProtocol::Http,
    );
    let api_h2c = RouteIdentity::new(
        "media.example.com",
        Some("/api"),
        "https",
        RouteProtocol::H2c,
    );
    let api_other_entry = RouteIdentity::new(
        "media.example.com",
        Some("/api"),
        "internal",
        RouteProtocol::Http,
    );
    assert_ne!(plain, api);
    assert_eq!(api, api_trailing, "尾部 / 必须归一化");
    assert_ne!(api, api_h2c, "协议不同即不同路由");
    assert_ne!(api, api_other_entry, "entrypoint 不同即不同路由");
    assert_eq!(
        RouteIdentity::new("media.example.com", None, "", RouteProtocol::Http).entrypoint,
        super::DEFAULT_ENTRYPOINT,
        "空 entrypoint 归一为 Traefik 默认入口"
    );
    Ok(())
}

/// 路由展开为按主机名拆分的绑定，报错信息包含归属方（C2）。
#[test]
fn route_bindings_carry_ownership() -> anyhow::Result<()> {
    let route = Route {
        name: String::from("grpc"),
        hosts: vec![
            String::from("media.example.com"),
            String::from("grpc.example.com"),
        ],
        path_prefix: None,
        container_port: Some(10000),
        middlewares: Vec::new(),
        protocol: RouteProtocol::H2c,
        entrypoint: String::from("https"),
        sticky_cookie: false,
        pass_host_header: None,
        priority: None,
        service: None,
        tls_domains: Vec::new(),
    };
    let bindings = route.bindings("netbird", "dashboard");
    assert_eq!(bindings.len(), 2);
    assert_eq!(bindings[0].router, "nsetup-netbird-dashboard-grpc");
    assert_eq!(
        bindings[0].owner(),
        "项目 netbird 的服务 dashboard 路由 nsetup-netbird-dashboard-grpc"
    );
    assert_eq!(bindings[0].identity.protocol.as_str(), "h2c");
    Ok(())
}

/// 用户手写 router label 参与路由身份提取；`traefik.enable=false` 时跳过（A2/A3）。
#[test]
fn user_router_labels_are_recognized() -> anyhow::Result<()> {
    let service = Service {
        image: String::from("example/app:1"),
        labels: vec![
            String::from(
                "traefik.http.routers.custom.rule=Host(`custom.example.com`) && PathPrefix(`/x`)",
            ),
            String::from("traefik.http.routers.custom.entrypoints=web"),
            String::from("traefik.http.services.custom.loadbalancer.server.scheme=h2c"),
            String::from("traefik.http.routers.regex.rule=HostRegexp(`{sub:[a-z]+}.example.com`)"),
        ],
        ..Service::default()
    };
    let identities = service.user_route_identities()?;
    assert_eq!(
        identities.len(),
        1,
        "HostRegexp 不参与冲突判定: {identities:?}"
    );
    assert_eq!(identities[0].host, "custom.example.com");
    assert_eq!(identities[0].path_prefix, "/x");
    assert_eq!(identities[0].entrypoint, "web");
    assert_eq!(identities[0].protocol.as_str(), "h2c");

    let disabled = Service {
        labels: vec![
            String::from("traefik.enable=false"),
            String::from("traefik.http.routers.custom.rule=Host(`custom.example.com`)"),
        ],
        ..service
    };
    assert!(disabled.user_route_identities()?.is_empty());
    Ok(())
}

/// 用户显式声明的 `traefik.enable` 必须保留（A3）。
#[test]
fn explicit_traefik_enable_is_preserved() -> anyhow::Result<()> {
    let mut enabled = Service {
        image: String::from("example/app:1"),
        labels: vec![String::from("traefik.enable=true")],
        ..Service::default()
    };
    enabled.set_routes("demo", "web", &[])?;
    assert!(
        enabled
            .labels
            .iter()
            .any(|label| label == "traefik.enable=true")
    );

    let mut disabled = Service {
        image: String::from("example/app:1"),
        labels: vec![String::from("traefik.enable=false")],
        ..Service::default()
    };
    disabled.set_routes("demo", "web", &[])?;
    assert!(
        disabled
            .labels
            .iter()
            .any(|label| label == "traefik.enable=false")
    );

    let mut plain = Service {
        image: String::from("example/app:1"),
        ..Service::default()
    };
    plain.set_routes("demo", "web", &[])?;
    assert!(
        !plain
            .labels
            .iter()
            .any(|label| label.starts_with("traefik.")),
        "没有路由时不生成任何 Traefik label: {:?}",
        plain.labels
    );
    let mut invalid = Service {
        image: String::from("example/app:1"),
        labels: vec![String::from("traefik.enable=maybe")],
        ..Service::default()
    };
    assert!(
        invalid
            .set_routes("demo", "web", &[generated_route()])
            .is_err(),
        "非法 traefik.enable 必须报错而不是被忽略"
    );
    Ok(())
}

/// 构造一条 nsetup 生成的路由，用于复用其默认字段。
fn generated_route() -> Route {
    Route {
        name: String::from("default"),
        hosts: vec![String::from("demo.example.com")],
        path_prefix: None,
        container_port: Some(80),
        middlewares: Vec::new(),
        protocol: RouteProtocol::Http,
        entrypoint: String::from(HTTPS_ENTRYPOINT),
        sticky_cookie: false,
        pass_host_header: None,
        priority: None,
        service: None,
        tls_domains: Vec::new(),
    }
}

/// 健康检查支持 `CMD-SHELL` 与 argv 两种形式，并拒绝其它写法（B4）。
#[test]
fn healthcheck_supports_shell_and_exec_forms() -> anyhow::Result<()> {
    let shell = super::Healthcheck::command(String::from("/app/healthcheck.sh"));
    assert_eq!(shell.shell_command()?, "/app/healthcheck.sh");
    assert!(shell.exec_arguments().is_err());
    shell.validate()?;

    let exec = super::Healthcheck::exec(&[
        String::from("/usr/bin/curl"),
        String::from("-f"),
        String::from("http://127.0.0.1:8008/health"),
    ])?;
    assert_eq!(exec.test[0], "CMD");
    assert_eq!(exec.exec_arguments()?.len(), 3);
    assert!(exec.shell_command().is_err());
    exec.validate()?;

    assert!(super::Healthcheck::exec(&[]).is_err());
    let unsupported = super::Healthcheck {
        test: vec![String::from("NONE"), String::from("x")],
        ..exec.clone()
    };
    assert!(unsupported.validate().is_err());
    let mut zero_retries = exec;
    zero_retries.retries = Some(0);
    assert!(zero_retries.validate().is_err());
    Ok(())
}
