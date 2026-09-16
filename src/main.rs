use actix_web::{
    App, Error, HttpRequest, HttpResponse, HttpResponseBuilder, HttpServer, Responder, delete,
    dev::ServiceRequest, error::ErrorUnauthorized, get, rt, web,
};
use actix_web_httpauth::{extractors::basic::BasicAuth, middleware::HttpAuthentication};
use actix_ws::Message;
use futures_util::StreamExt as _;
use maud::{DOCTYPE, Markup, PreEscaped, html};
use sqlx::SqlitePool;

fn index_html(title: &str) -> Markup {
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
                header #status {
                    h1 { (title) }
                }
                main {
                    input id="terms" name="terms" autofocus="" autocomplete="off" spellcheck="false"
                            _=r#"
                            on input or refresh
                                send terms(terms: my value) to Socket
                            end
                            on keydown[key is 'Enter']
                                set terms to encodeURIComponent(my value)
                                go to `/${terms}`
                            "#;
                    output id="matches" {}
                }
            }
        }
    }
}

fn make_default_response(title: &str, mut resp: HttpResponseBuilder) -> HttpResponse {
    resp.content_type("text/html charset=utf-8")
        .body(index_html(title))
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

    return match minx::dispatch(command, pool).await {
        minx::CommandResult::Deleted => HttpResponse::Ok().body("deleted"),
        _ => HttpResponse::InternalServerError().body("delete failed"),
    };
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

async fn login(
    service_req: ServiceRequest,
    auth: BasicAuth,
) -> Result<ServiceRequest, (actix_web::Error, ServiceRequest)> {
    if auth.user_id() == "user" && auth.password() == Some("pass") {
        Ok(service_req)
    } else {
        Err((ErrorUnauthorized("login failed"), service_req))
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let db = SqlitePool::connect("sqlite:minx.db")
        .await
        .expect("failed to connect to minx.db");

    HttpServer::new(move || {
        App::new()
            .wrap(HttpAuthentication::basic(login))
            .app_data(web::Data::new(db.clone()))
            .service(index)
            .service(actix_files::Files::new("/static", "./static"))
            .route("/ws", web::get().to(websocket))
            .service(delete)
            .service(search)
    })
    .bind(("127.0.0.1", 3484))?
    .run()
    .await
}
