use actix_files as fs;
use actix_web::{middleware, web, App, HttpResponse, HttpServer, Responder};
mod markdown;

async fn index() -> impl Responder {
    HttpResponse::Ok().body("Markdown to HTML demo")
}

#[actix_rt::main]
async fn main() -> std::io::Result<()> {
    std::env::set_var("RUST_LOG", "actix_server=info,actix_web=info");
    env_logger::init();
    HttpServer::new(|| {
        App::new()
            .wrap(markdown::Transformer)
            .wrap(middleware::Compress::default())
            .wrap(middleware::Logger::default())
            .route("/", web::get().to(index))
            .service(fs::Files::new("/files", ".").show_files_listing())
    })
    .bind("127.0.0.1:8088")?
    .run()
    .await
}
