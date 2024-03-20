use crate::{
    api::{self},
    middleware::{model_route, ThreadRequest, ThreadState},
};
use crate::{load_config, load_plugin, load_web, Args};
use clap::Parser;
use salvo::affix;
use salvo::cors::AllowOrigin;
use salvo::cors::Cors;
use salvo::http::Method;
use salvo::logging::Logger;
use salvo::prelude::*;
use salvo::serve_static::StaticDir;
use salvo::Router;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
};

#[allow(clippy::collapsible_else_if)]
pub async fn salvo_main() {
    use clap::CommandFactory;
    use salvo::conn::rustls::{Keycert, RustlsConfig};

    simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Warn)
        .with_module_level("ai00_server", log::LevelFilter::Info)
        .with_module_level("web_rwkv", log::LevelFilter::Info)
        .init()
        .unwrap();

    let args = Args::parse();
    let (sender, receiver) = flume::unbounded::<ThreadRequest>();

    let request: crate::middleware::ReloadRequest = {
        let path = args
            .config
            .clone()
            .unwrap_or("assets/configs/Config.toml".into());
        log::info!("reading config {}...", path.to_string_lossy());
        load_config(path).expect("load config failed").into()
    };

    let listen = request.listen.clone();

    tokio::task::spawn_blocking(move || model_route(receiver));
    let _ = sender.send(ThreadRequest::Reload {
        request: Box::new(request),
        sender: None,
    });

    let serve_path = {
        let path = tempfile::tempdir()
            .expect("create temp dir failed")
            .into_path();
        load_web("assets/www/index.zip", &path).expect("load frontend failed");
        path
    };

    // create `assets/www/plugins` if it doesn't exist
    if !Path::new("assets/www/plugins").exists() {
        std::fs::create_dir("assets/www/plugins").expect("create plugins dir failed");
    }

    // extract and load all plugins under `assets/www/plugins`
    match std::fs::read_dir("assets/www/plugins") {
        Ok(dir) => dir
            .filter_map(|x| x.ok())
            .filter(|x| x.path().is_file())
            .filter(|x| x.path().extension().is_some_and(|ext| ext == "zip"))
            .filter(|x| x.path().file_stem().is_some_and(|stem| stem != "api"))
            .for_each(|x| {
                let name = x
                    .path()
                    .file_stem()
                    .expect("this cannot happen")
                    .to_string_lossy()
                    .into();
                match load_plugin(x.path(), &serve_path, &name) {
                    Ok(_) => log::info!("loaded plugin {}", name),
                    Err(err) => log::error!("failed to load plugin {}, {}", name, err),
                }
            }),
        Err(err) => {
            log::error!("failed to read plugin directory: {}", err);
        }
    };

    let cors = Cors::new()
        .allow_origin(AllowOrigin::any())
        .allow_methods(vec![Method::GET, Method::POST, Method::DELETE])
        .allow_headers("authorization")
        .into_handler();

    let app = Router::new()
        //.hoop(CorsLayer::permissive())
        .hoop(Logger::new())
        .hoop(affix::inject(ThreadState(sender)))
        .hoop(cors)
        .push(Router::with_path("/api/adapters").get(api::adapters))
        .push(Router::with_path("/api/models/info").get(api::info))
        .push(Router::with_path("/api/models/load").post(api::load))
        .push(Router::with_path("/api/models/unload").get(api::unload))
        .push(Router::with_path("/api/models/state").get(api::state))
        .push(Router::with_path("/api/models/list").get(api::models))
        .push(Router::with_path("/api/files/unzip").post(api::unzip))
        .push(Router::with_path("/api/files/dir").post(api::dir))
        .push(Router::with_path("/api/files/ls").post(api::dir))
        .push(Router::with_path("/api/files/config/load").post(api::load_config))
        .push(Router::with_path("/api/files/config/save").post(api::save_config))
        .push(Router::with_path("/api/oai/models").get(api::oai::models))
        .push(Router::with_path("/api/oai/v1/models").get(api::oai::models))
        .push(Router::with_path("/api/oai/completions").post(api::oai::completions))
        .push(Router::with_path("/api/oai/v1/completions").post(api::oai::completions))
        .push(Router::with_path("/api/oai/chat/completions").post(api::oai::chat_completions))
        .push(Router::with_path("/api/oai/v1/chat/completions").post(api::oai::chat_completions))
        .push(Router::with_path("/api/oai/embeddings").post(api::oai::embeddings))
        .push(Router::with_path("/api/oai/v1/embeddings").post(api::oai::embeddings));
    // .push(
    //     Router::with_path("<**path>").get(StaticDir::new(serve_path).defaults(["index.html"])),
    // )
    // .fallback_service(ServeDir::new(serve_path))
    // .layer(CorsLayer::permissive());

    let cmd = Args::command();
    let version = cmd.get_version().unwrap_or("0.0.1");
    let bin_name = cmd.get_bin_name().unwrap_or("ai00_server");

    let doc = OpenApi::new(bin_name, version).merge_router(&app);

    let app = app
        .push(doc.into_router("/api-doc/openapi.json"))
        .push(SwaggerUi::new("/api-doc/openapi.json").into_router("swagger-ui"))
        .push(
            Router::with_path("<**path>").get(StaticDir::new(serve_path).defaults(["index.html"])),
        ); // this static serve should after the swagger.

    let (ipaddr, ipv6addr) = if args.ip.is_some() {
        (args.ip.unwrap(), None)
    } else if listen.is_some() {
        let v4_addr = listen
            .clone()
            .unwrap()
            .ip
            .map(|f| f.parse().unwrap_or(Ipv4Addr::UNSPECIFIED))
            .unwrap_or(Ipv4Addr::UNSPECIFIED);
        let v6_addr = listen
            .clone()
            .unwrap()
            .ipv6
            .map(|f| f.parse().unwrap_or(Ipv6Addr::UNSPECIFIED));
        (IpAddr::from(v4_addr), v6_addr)
    } else {
        (IpAddr::from(Ipv4Addr::UNSPECIFIED), None)
    };

    let bind_port = if args.port > 0 && args.port != 65530u16 {
        args.port
    } else if listen.clone().is_some() {
        listen.clone().unwrap().port.unwrap_or(65530u16)
    } else {
        65530u16
    };

    let (bind_domain, use_acme, use_tls) = if listen.clone().is_some() {
        let clone_listen = listen.clone().unwrap();
        let domain = clone_listen.domain.unwrap_or("local".to_string());
        let acme = match domain.as_str() {
            "local" => false,
            _ => clone_listen.acme.unwrap_or_default(),
        };
        let tls = match acme {
            true => true,
            false => clone_listen.tls.unwrap_or_default(),
        };

        (domain, acme, tls)
    } else {
        ("local".to_string(), false, false)
    };

    let addr = SocketAddr::new(ipaddr, bind_port);

    if use_acme {
        let acmelistener = TcpListener::new(addr)
            .acme()
            .cache_path("assets/certs")
            .add_domain(bind_domain)
            .quinn(addr);
        if ipv6addr.is_some() {
            let v6addr = SocketAddr::new(IpAddr::V6(ipv6addr.unwrap()), bind_port);
            let acceptor = acmelistener.join(TcpListener::new(v6addr)).bind().await;
            log::info!("server started at {addr} with acme and tls.");
            log::info!("server started at {v6addr} with acme and tls.");
            salvo::server::Server::new(acceptor).serve(app).await;
        } else {
            let acceptor = acmelistener.bind().await;
            log::info!("server started at {addr} with acme and tls.");
            salvo::server::Server::new(acceptor).serve(app).await;
        };
    } else if use_tls {
        let config = RustlsConfig::new(
            Keycert::new()
                .cert_from_path("assets/certs/cert.pem")
                .unwrap()
                .key_from_path("assets/certs/key.pem")
                .unwrap(),
        );
        let listener = TcpListener::new(addr).rustls(config.clone());
        if ipv6addr.is_some() {
            let v6addr = SocketAddr::new(IpAddr::V6(ipv6addr.unwrap()), bind_port);
            let ipv6listener = TcpListener::new(v6addr).rustls(config.clone());
            let acceptor = QuinnListener::new(config.clone(), addr)
                .join(QuinnListener::new(config, v6addr))
                .join(ipv6listener)
                .join(listener)
                .bind()
                .await;
            log::info!("server started at {addr} with tls.");
            log::info!("server started at {v6addr} with tls.");
            salvo::server::Server::new(acceptor).serve(app).await;
        } else {
            let acceptor = QuinnListener::new(config.clone(), addr)
                .join(listener)
                .bind()
                .await;
            log::info!("server started at {addr} with tls.");
            salvo::server::Server::new(acceptor).serve(app).await;
        };
    } else {
        if ipv6addr.is_some() {
            let v6addr = SocketAddr::new(IpAddr::V6(ipv6addr.unwrap()), bind_port);
            let ipv6listener = TcpListener::new(v6addr);
            let acceptor = TcpListener::new(addr).join(ipv6listener).bind().await;
            log::info!("server started at {addr} without tls.");
            log::info!("server started at {v6addr} without tls.");
            salvo::server::Server::new(acceptor).serve(app).await;
        } else {
            log::info!("server started at {addr} without tls.");
            let acceptor = TcpListener::new(addr).bind().await;
            salvo::server::Server::new(acceptor).serve(app).await;
        };
    };
}
