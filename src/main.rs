use actix_cors::Cors;
use actix_http::header::HttpDate;
use actix_web::{
    get,
    http::header,
    middleware, post,
    web::{Data, Path},
    App, Error, HttpResponse, HttpServer, Responder,
};
use actix_web_prom::PrometheusMetricsBuilder;
use anyhow::Result;
use askama_actix::Template;
use chrono::{DateTime, Utc};
use lazy_static::lazy_static;
use nanoid::nanoid;
use prometheus::default_registry;
use sqlx::{sqlite::SqlitePool, Pool, Sqlite};
use std::{env, fmt::Display, net::Ipv4Addr, time::SystemTime};

lazy_static! {
    static ref REF: &'static str = include_str!("../.git/HEAD");
    static ref REF_MAIN: &'static str = include_str!("../.git/refs/heads/main");
    static ref HASH: &'static str = if REF.contains("refs/heads/main") {
        &REF_MAIN
    } else {
        &REF
    };
}

#[actix_web::main]
async fn main() -> Result<(), Error> {
    dotenvy::dotenv().unwrap();

    let prometheus = PrometheusMetricsBuilder::new("api")
        .endpoint("/metrics")
        .registry(default_registry().clone())
        .build()
        .unwrap();

    let pool = SqlitePool::connect(&env::var("DATABASE_URL").expect("DATABASE_URL not configured"))
        .await
        .expect("Could not connect to database");

    Ok(HttpServer::new(move || {
        App::new()
            .wrap(middleware::Compress::default())
            .wrap(middleware::NormalizePath::trim())
            .wrap(middleware::Logger::default())
            .wrap(prometheus.clone())
            .wrap(
                Cors::default()
                    .allow_any_origin()
                    .allowed_methods(vec!["GET", "POST"])
                    .allowed_header(header::CONTENT_TYPE)
                    .max_age(3600),
            )
            .app_data(Data::new(pool.clone()))
            .service(index)
            .service(favicon)
            .service(health)
            .service(get_total)
            .service(get_highest)
            .service(new_counter)
            .service(get_counter_ext)
            .service(get_counter_metrics)
            .service(get_plus_counter_ext)
            .service(get_plus_counter)
            .service(get_counter)
            .service(post_counter)
            .service(new_gauge)
            .service(get_gauge_ext)
            .service(get_gauge_metrics)
            .service(get_minus_gauge_ext)
            .service(get_plus_gauge_ext)
            .service(get_minus_gauge)
            .service(get_plus_gauge)
            .service(get_gauge)
            .service(post_gauge)
            .service(post_minus_gauge)
    })
    .bind((
        Ipv4Addr::new(127, 0, 0, 1),
        env::var("PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(8126),
    ))?
    .run()
    .await?)
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate;

#[get("/")]
async fn index() -> impl Responder {
    IndexTemplate
}

#[get("/favicon.ico")]
async fn favicon() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((header::CACHE_CONTROL, "immutable"))
        .insert_header((header::CONTENT_TYPE, "image/x-icon"))
        .body(&include_bytes!("../favicon.ico")[..])
}

#[get("/_h")]
async fn health() -> impl Responder {
    HttpResponse::Ok().body(HASH.as_bytes())
}

trait CounterLike: Sized + Display
where
    HttpDate: for<'a> std::convert::From<&'a Self>,
{
    #[inline(always)]
    fn valid_id(id: &str) -> bool {
        !id.is_empty() && id.len() < 255 && id.is_ascii()
    }

    #[inline(always)]
    async fn create(pool: &Pool<Sqlite>) -> Result<Self> {
        Self::create_with_id_and_value(&nanoid!(12, &nanoid::alphabet::SAFE), pool, 0).await
    }

    fn as_format(&self, ext: &str) -> HttpResponse {
        match ext {
            "png" => HttpResponse::Ok()
                .insert_header(header::LastModified(self.into()))
                .insert_header(header::ContentType::png())
                .body(&include_bytes!("../out.png")[..]),
            "jpg" => HttpResponse::Ok()
                .insert_header(header::LastModified(self.into()))
                .insert_header(header::ContentType::png())
                .body(&include_bytes!("../out.jpg")[..]),
            "gif" => HttpResponse::Ok()
                .insert_header(header::LastModified(self.into()))
                .insert_header((header::CONTENT_TYPE, "image/gif"))
                .body(&include_bytes!("../out.gif")[..]),
            "svg" => HttpResponse::Ok()
                .insert_header(header::LastModified(self.into()))
                .insert_header((header::CONTENT_TYPE, "image/svg+xml; charset=utf-8"))
                .body("<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            "json" => HttpResponse::Ok()
                .insert_header(header::LastModified(self.into()))
                .insert_header(header::ContentType::json())
                .json(self.value()),
            "txt" => HttpResponse::Ok()
                .insert_header(header::LastModified(self.into()))
                .insert_header(header::ContentType::plaintext())
                .body(self.to_string()),
            _ => HttpResponse::NotFound().body(""),
        }
    }

    async fn create_with_id_and_value(id: &str, pool: &Pool<Sqlite>, value: i64) -> Result<Self>;
    async fn get(id: &str, pool: &Pool<Sqlite>) -> Option<Self>;
    fn as_openmetrics(&self) -> HttpResponse;
    fn new(id: &str, value: i64) -> Self;
    fn id(&self) -> &str;
    fn value(&self) -> i64;
}

#[derive(Clone)]
pub struct Counter {
    pub id: String,
    value: i64,
    updated_at: DateTime<Utc>,
}

impl CounterLike for Counter {
    #[inline(always)]
    fn new(id: &str, value: i64) -> Self {
        Self {
            id: id.to_owned(),
            value,
            updated_at: SystemTime::now().into(),
        }
    }

    #[inline(always)]
    fn id(&self) -> &str {
        &self.id
    }

    #[inline(always)]
    fn value(&self) -> i64 {
        self.value
    }

    async fn create_with_id_and_value(id: &str, pool: &Pool<Sqlite>, value: i64) -> Result<Self> {
        let mut conn = pool.acquire().await?;
        sqlx::query!(
            r#"INSERT INTO c ( nano_id, value ) VALUES ( ?1, ?2 )"#,
            id,
            value
        )
        .execute(&mut *conn)
        .await?;

        Ok(Self {
            id: id.to_owned(),
            value,
            updated_at: SystemTime::now().into(),
        })
    }

    async fn get(id: &str, pool: &Pool<Sqlite>) -> Option<Self> {
        if let Ok(mut conn) = pool.acquire().await {
            sqlx::query!(r#"SELECT value, updated_at FROM c WHERE nano_id = ?1"#, id)
                .fetch_one(&mut *conn)
                .await
                .map(|res| {
                    Some(Self {
                        id: id.to_owned(),
                        value: res.value,
                        updated_at: res.updated_at.and_utc(),
                    })
                })
                .unwrap_or(None)
        } else {
            None
        }
    }

    fn as_openmetrics(&self) -> HttpResponse {
        HttpResponse::Ok()
            .insert_header(header::LastModified(self.into()))
            .insert_header((
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            ))
            .body(format!(
                "# TYPE {} counter\n{}_count {}",
                self.id(),
                self.id(),
                self
            ))
    }
}

impl Counter {
    async fn increment_or_create(id: &str, pool: &Pool<Sqlite>) -> Result<i64> {
        let mut conn = pool.acquire().await?;
        let res = sqlx::query!(
            r#"UPDATE c SET value = value + 1 WHERE nano_id = ?1 RETURNING value"#,
            id
        )
        .fetch_optional(&mut *conn)
        .await?;

        if let Some(rec) = res {
            Ok(rec.value)
        } else {
            Ok(Self::create_with_id_and_value(id, pool, 1).await?.value)
        }
    }
}

impl From<&Counter> for HttpDate {
    fn from(val: &Counter) -> Self {
        let time: SystemTime = val.updated_at.into();
        time.into()
    }
}

impl Display for Counter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.value)
    }
}

#[derive(Clone)]
pub struct Gauge {
    pub id: String,
    value: i64,
    updated_at: DateTime<Utc>,
}

impl CounterLike for Gauge {
    #[inline(always)]
    fn new(id: &str, value: i64) -> Self {
        Self {
            id: id.to_owned(),
            value,
            updated_at: SystemTime::now().into(),
        }
    }

    #[inline(always)]
    fn id(&self) -> &str {
        &self.id
    }

    #[inline(always)]
    fn value(&self) -> i64 {
        self.value
    }

    async fn create_with_id_and_value(id: &str, pool: &Pool<Sqlite>, value: i64) -> Result<Self> {
        let mut conn = pool.acquire().await?;
        sqlx::query!(
            r#"INSERT INTO g ( nano_id, value ) VALUES ( ?1, ?2 )"#,
            id,
            value
        )
        .execute(&mut *conn)
        .await?;

        Ok(Self {
            id: id.to_owned(),
            value,
            updated_at: SystemTime::now().into(),
        })
    }

    async fn get(id: &str, pool: &Pool<Sqlite>) -> Option<Self> {
        if let Ok(mut conn) = pool.acquire().await {
            sqlx::query!(r#"SELECT value, updated_at FROM g WHERE nano_id = ?1"#, id)
                .fetch_one(&mut *conn)
                .await
                .map(|res| {
                    Some(Self {
                        id: id.to_owned(),
                        value: res.value,
                        updated_at: res.updated_at.and_utc(),
                    })
                })
                .unwrap_or(None)
        } else {
            None
        }
    }

    fn as_openmetrics(&self) -> HttpResponse {
        HttpResponse::Ok()
            .insert_header(header::LastModified(self.into()))
            .insert_header((
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            ))
            .body(format!(
                "# TYPE {} gauge\n{}_count {}",
                self.id(),
                self.id(),
                self
            ))
    }
}

impl Gauge {
    async fn decrement_or_create(id: &str, pool: &Pool<Sqlite>) -> Result<i64> {
        let mut conn = pool.acquire().await?;
        let res = sqlx::query!(
            r#"UPDATE g SET value = value - 1 WHERE nano_id = ?1 RETURNING value"#,
            id
        )
        .fetch_optional(&mut *conn)
        .await?;

        if let Some(rec) = res {
            Ok(rec.value)
        } else {
            Ok(Self::create_with_id_and_value(id, pool, 1).await?.value)
        }
    }
    async fn increment_or_create(id: &str, pool: &Pool<Sqlite>) -> Result<i64> {
        let mut conn = pool.acquire().await?;
        let res = sqlx::query!(
            r#"UPDATE g SET value = value + 1 WHERE nano_id = ?1 RETURNING value"#,
            id
        )
        .fetch_optional(&mut *conn)
        .await?;

        if let Some(rec) = res {
            Ok(rec.value)
        } else {
            Ok(Self::create_with_id_and_value(id, pool, 1).await?.value)
        }
    }
}

impl From<&Gauge> for HttpDate {
    fn from(val: &Gauge) -> Self {
        let time: SystemTime = val.updated_at.into();
        time.into()
    }
}

impl Display for Gauge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.value)
    }
}

#[get("/_total")]
async fn get_total(pool: Data<Pool<Sqlite>>) -> impl Responder {
    let value =
        sqlx::query!(r#"SELECT (SELECT count(id) FROM c) + (SELECT count(id) FROM g) as value"#)
            .fetch_one(pool.get_ref())
            .await
            .map(|res| res.value);
    if let Ok(value) = value {
        HttpResponse::Ok().body(format!("{}", value))
    } else {
        HttpResponse::InternalServerError().body("")
    }
}

#[get("/_highest")]
async fn get_highest(pool: Data<Pool<Sqlite>>) -> impl Responder {
    let value = sqlx::query!(
        r#"SELECT value FROM c UNION SELECT value from g ORDER BY value DESC LIMIT 1"#
    )
    .fetch_one(pool.get_ref())
    .await
    .map(|res| res.value);
    if let Ok(value) = value {
        HttpResponse::Ok().body(format!("{}", value))
    } else {
        HttpResponse::InternalServerError().body("")
    }
}

#[post("/c")]
async fn new_counter(pool: Data<Pool<Sqlite>>) -> impl Responder {
    if let Ok(counter) = Counter::create(pool.get_ref()).await {
        HttpResponse::SeeOther()
            .insert_header((header::LOCATION, format!("/c/{}", counter.id)))
            .insert_header(header::ContentType::plaintext())
            .body(counter.id)
    } else {
        HttpResponse::InternalServerError().body("")
    }
}

#[get("/c/{id}")]
async fn get_counter(path: Path<(String,)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Counter::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Some(counter) = Counter::get(&path.0, pool.get_ref()).await {
        counter.as_format("txt")
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[get("/c+/{id}")]
async fn get_plus_counter(path: Path<(String,)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Counter::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Ok(i) = Counter::increment_or_create(&path.0, pool.get_ref()).await {
        HttpResponse::SeeOther()
            .insert_header((header::LOCATION, format!("/c/{}", path.0)))
            .insert_header(header::ContentType::plaintext())
            .body(format!("{:?}", i))
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[get("/c+/{id}.{ext}")]
async fn get_plus_counter_ext(
    path: Path<(String, String)>,
    pool: Data<Pool<Sqlite>>,
) -> impl Responder {
    if !Counter::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Ok(i) = Counter::increment_or_create(&path.0, pool.get_ref()).await {
        Counter::new(&path.0, i).as_format(&path.1)
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[get("/c/{id}.{ext}")]
async fn get_counter_ext(path: Path<(String, String)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Counter::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Some(counter) = Counter::get(&path.0, pool.get_ref()).await {
        counter.as_format(&path.1)
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[get("/c/{id}/metrics")]
async fn get_counter_metrics(path: Path<(String,)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Counter::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Some(counter) = Counter::get(&path.0, pool.get_ref()).await {
        counter.as_openmetrics()
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[post("/c/{id}")]
async fn post_counter(path: Path<(String,)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Counter::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Ok(i) = Counter::increment_or_create(&path.0, pool.get_ref()).await {
        HttpResponse::SeeOther()
            .insert_header((header::LOCATION, format!("/c/{}", path.0)))
            .insert_header(header::ContentType::plaintext())
            .body(format!("{:?}", i))
    } else {
        HttpResponse::InternalServerError().body("")
    }
}

#[post("/g")]
async fn new_gauge(pool: Data<Pool<Sqlite>>) -> impl Responder {
    if let Ok(gauge) = Gauge::create(pool.get_ref()).await {
        HttpResponse::SeeOther()
            .insert_header((header::LOCATION, format!("/g/{}", gauge.id)))
            .insert_header(header::ContentType::plaintext())
            .body(gauge.id)
    } else {
        HttpResponse::InternalServerError().body("")
    }
}

#[get("/g/{id}")]
async fn get_gauge(path: Path<(String,)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Gauge::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Some(gauge) = Gauge::get(&path.0, pool.get_ref()).await {
        gauge.as_format("txt")
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[get("/g-/{id}")]
async fn get_minus_gauge(path: Path<(String,)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Gauge::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Ok(i) = Gauge::decrement_or_create(&path.0, pool.get_ref()).await {
        HttpResponse::SeeOther()
            .insert_header((header::LOCATION, format!("/g/{}", path.0)))
            .insert_header(header::ContentType::plaintext())
            .body(format!("{:?}", i))
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[get("/g+/{id}")]
async fn get_plus_gauge(path: Path<(String,)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Gauge::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Ok(i) = Gauge::increment_or_create(&path.0, pool.get_ref()).await {
        HttpResponse::SeeOther()
            .insert_header((header::LOCATION, format!("/g/{}", path.0)))
            .insert_header(header::ContentType::plaintext())
            .body(format!("{:?}", i))
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[get("/g-/{id}.{ext}")]
async fn get_minus_gauge_ext(
    path: Path<(String, String)>,
    pool: Data<Pool<Sqlite>>,
) -> impl Responder {
    if !Gauge::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Ok(i) = Gauge::decrement_or_create(&path.0, pool.get_ref()).await {
        Gauge::new(&path.0, i).as_format(&path.1)
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[get("/g+/{id}.{ext}")]
async fn get_plus_gauge_ext(
    path: Path<(String, String)>,
    pool: Data<Pool<Sqlite>>,
) -> impl Responder {
    if !Gauge::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Ok(i) = Gauge::increment_or_create(&path.0, pool.get_ref()).await {
        Gauge::new(&path.0, i).as_format(&path.1)
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[get("/g/{id}.{ext}")]
async fn get_gauge_ext(path: Path<(String, String)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Gauge::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Some(gauge) = Gauge::get(&path.0, pool.get_ref()).await {
        gauge.as_format(&path.1)
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[get("/g/{id}/metrics")]
async fn get_gauge_metrics(path: Path<(String,)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Gauge::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Some(gauge) = Gauge::get(&path.0, pool.get_ref()).await {
        gauge.as_openmetrics()
    } else {
        HttpResponse::NotFound().body("")
    }
}

#[post("/g/{id}")]
async fn post_gauge(path: Path<(String,)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Gauge::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Ok(i) = Gauge::increment_or_create(&path.0, pool.get_ref()).await {
        HttpResponse::SeeOther()
            .insert_header((header::LOCATION, format!("/g/{}", path.0)))
            .insert_header(header::ContentType::plaintext())
            .body(format!("{:?}", i))
    } else {
        HttpResponse::InternalServerError().body("")
    }
}

#[post("/g-/{id}")]
async fn post_minus_gauge(path: Path<(String,)>, pool: Data<Pool<Sqlite>>) -> impl Responder {
    if !Gauge::valid_id(&path.0) {
        return HttpResponse::BadRequest().body("");
    }
    if let Ok(i) = Gauge::decrement_or_create(&path.0, pool.get_ref()).await {
        HttpResponse::SeeOther()
            .insert_header((header::LOCATION, format!("/g/{}", path.0)))
            .insert_header(header::ContentType::plaintext())
            .body(format!("{:?}", i))
    } else {
        HttpResponse::NotFound().body("")
    }
}
