use actix_web::{
    App, Error, HttpRequest, HttpResponse, HttpResponseBuilder, HttpServer, Responder,
    body::BoxBody,
    cookie::{Cookie, SameSite, time::Duration},
    delete,
    dev::{ServiceRequest, ServiceResponse},
    get,
    middleware::{Next, from_fn},
    put, rt, web,
};
use actix_ws::Message;
use clap::Parser;
use futures_util::StreamExt as _;
use maud::{DOCTYPE, Markup, PreEscaped, html};
use serde::Deserialize;
use sqlx::SqlitePool;

fn layout_html(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                meta name="color-scheme" content="light";
                base rel="noopener noreferrer";
                title { (title) }
                meta name="author" content="C Wiebe <ctwiebe23@gmail.com>";
                meta name="description" content="Searchable bookmarks";
                link rel="stylesheet" href="/static/styles.css";
                script src="/static/hyperscript.min.js" {}
                script src="/static/socket.min.js" {}
            }
            body {
                header #status {
                    h1 { (title) }
                }
                main {
                    (body)
                }
            }
        }
    }
}

fn index_body() -> Markup {
    html! {
        script type="text/hyperscript" {
            (PreEscaped(r#"
            socket Socket /ws
                on message put message into #matches
            end
            on delete(locator)
                set path to encodeURIComponent(locator)
                fetch `/${path}` with method: 'DELETE'
                put "<h1>" + the result + "</h1>" into #status
                send refresh to #terms
            end
            on keydown(key, ctrlKey)
                if key is 'x' and ctrlKey is true and some <h2/>
                    set locator to the innerText of the first <h2/>
                    then trigger delete(locator: locator)
                else if key is 'e' and ctrlKey is true and some <h2/>
                    set #terms's value to the first <h2/>'s innerText + " " + the first <p/>'s innerText
            "#))
        }
        input id="terms" name="terms" autofocus="" autocomplete="off" spellcheck="false"
                _=r#"
                on input or refresh
                    send terms(terms: my value) to Socket
                catch e
                    put "<h1>disconnected</h1>" into #status
                end
                on keydown[key is 'Enter']
                    set terms to encodeURIComponent(my value)
                    go to `/${terms}`
                "#;
        output id="matches" {}
    }
}

fn make_default_response(title: &str, mut resp: HttpResponseBuilder) -> HttpResponse {
    resp.content_type("text/html charset=utf-8")
        .body(layout_html(title, index_body()))
}

fn login_body() -> Markup {
    html! {
        input id="decade" name="decade" autofocus="" autocomplete="off" spellcheck="false" type="password"
                _=r#"
                on keydown[key is 'Enter']
                    set decade to encodeURIComponent(my value)
                    fetch `/login?decade=${decade}` then reload() the location of the window
                catch e
                    put "<h1>login failed</h1>" into #status
                "#;
    }
}

fn make_login_response(mut resp: HttpResponseBuilder) -> HttpResponse {
    resp.content_type("text/html charset=utf-8")
        .body(layout_html("login", login_body()))
}

#[get("/")]
async fn index() -> impl Responder {
    make_default_response("minx", HttpResponse::Ok())
}

#[get("/{terms:.+}")]
async fn search(path: web::Path<String>, pool_data: web::Data<SqlitePool>) -> impl Responder {
    let terms = path.into_inner();
    let command = minx::terms_to_command(&terms);
    let pool = pool_data.get_ref();

    return match minx::dispatch(command, pool).await {
        minx::CommandResult::NavigateTo(locator) => HttpResponse::Found()
            .append_header(("LOCATION", locator))
            .finish(),
        minx::CommandResult::SearchFailed => {
            make_default_response("not found", HttpResponse::NotFound())
        }
        minx::CommandResult::NothingHappened => {
            make_default_response("but nothing happened", HttpResponse::Ok())
        }
        _ => make_default_response("internal error", HttpResponse::InternalServerError()),
    };
}

#[delete("/{locator}")]
async fn delete(path: web::Path<String>, pool_data: web::Data<SqlitePool>) -> impl Responder {
    let locator = path.into_inner();
    let command = minx::Command::Delete(locator);
    let pool = pool_data.get_ref();

    match minx::dispatch(command, pool).await {
        minx::CommandResult::Deleted => HttpResponse::Ok().body("deleted"),
        _ => HttpResponse::InternalServerError().body("delete failed"),
    }
}

#[put("/{locator}")]
async fn increment(path: web::Path<String>, pool_data: web::Data<SqlitePool>) -> impl Responder {
    let locator = path.into_inner();
    let command = minx::Command::Increment(locator);
    let pool = pool_data.get_ref().clone();

    match minx::dispatch(command, &pool).await {
        minx::CommandResult::Incremented => HttpResponse::Ok().body("increment successful"),
        _ => HttpResponse::InternalServerError().body("increment failed"),
    }
}

async fn websocket(
    req: HttpRequest,
    stream: web::Payload,
    pool_data: web::Data<SqlitePool>,
) -> Result<HttpResponse, Error> {
    let (resp, mut session, mut stream) = actix_ws::handle(&req, stream)?;
    let pool = pool_data.get_ref().clone();

    rt::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Text(text) => {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        if json["type"].as_str().unwrap_or_default() != "terms" {
                            continue;
                        }
                        let terms = json["terms"].as_str().unwrap_or_default();
                        let keywords = minx::terms_to_keywords(&terms);
                        let matches = minx::make_matches(&keywords, &pool).await.into_string();
                        if session.text(matches).await.is_err() {
                            break;
                        }
                    } else {
                        continue;
                    }
                }
                Message::Ping(bytes) => {
                    if session.pong(&bytes).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }

        let _ = session.close(None).await;
    });

    Ok(resp)
}

#[derive(Deserialize)]
struct WebCredentials {
    decade: Option<String>,
}

#[get("/login")]
async fn login(info: web::Query<WebCredentials>, creds: web::Data<Credentials>) -> impl Responder {
    match &info.decade {
        Some(decade) if decade == &creds.openphrase => HttpResponse::Ok()
            .cookie(
                Cookie::build("DECADE", &creds.cookie)
                    .max_age(Duration::days(30))
                    .http_only(true)
                    .secure(true)
                    .same_site(SameSite::Lax)
                    .finish(),
            )
            .body("login successful"),
        _ => HttpResponse::Unauthorized().body("invalid credentials"),
    }
}

async fn login_middleware(
    creds: web::Data<Credentials>,
    service_req: ServiceRequest,
    next: Next<BoxBody>,
) -> Result<ServiceResponse<BoxBody>, actix_web::Error> {
    let decade = service_req
        .cookie("DECADE")
        .and_then(|c| Some(c.value().to_owned()));

    match decade {
        Some(decade) if decade == creds.cookie => {
            Ok(next.call(service_req).await?.map_into_boxed_body())
        }
        _ => Ok(service_req.into_response(make_login_response(HttpResponse::Unauthorized()))),
    }
}

#[derive(clap::Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value_t = "127.0.0.1".to_string())]
    ip: String,

    #[arg(short, long, default_value_t = 3484)]
    port: u16,

    #[arg(short, long, default_value_t = "uv-5r+plus".to_string())]
    openphrase: String,

    #[arg(short, long, default_value_t = "./minx.db".to_string())]
    sqlite_db: String,
}

#[derive(Clone)]
struct Credentials {
    openphrase: String,
    cookie: String,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let creds = Credentials {
        openphrase: args.openphrase,
        cookie: uuid::Uuid::new_v4().to_string(),
    };

    let db = SqlitePool::connect(&format!("sqlite:{}", args.sqlite_db))
        .await
        .expect(&format!("failed to connect to {}", args.sqlite_db));

    println!("Starting minx on http://{}:{} . . .", args.ip, args.port);

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(creds.clone()))
            .service(login)
            .service(actix_files::Files::new("/static", "./static"))
            .service(
                web::scope("")
                    .wrap(from_fn(login_middleware))
                    .app_data(web::Data::new(db.clone()))
                    .service(index)
                    .route("/ws", web::get().to(websocket))
                    .service(delete)
                    .service(increment)
                    .service(search),
            )
    })
    .bind((args.ip, args.port))?
    .run()
    .await
}
