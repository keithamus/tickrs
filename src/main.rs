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
use askama::Template;
use chrono::{DateTime, Utc};
use nanoid::nanoid;
use prometheus::{default_registry, IntGauge, Registry};
use sqlx::{sqlite::SqlitePool, Pool, Sqlite};
use std::{env, fmt::Display, net::Ipv4Addr, sync::LazyLock, time::Duration, time::SystemTime};

static REF: LazyLock<&'static str> = LazyLock::new(|| include_str!("../.git/HEAD"));
static REF_MAIN: LazyLock<&'static str> = LazyLock::new(|| include_str!("../.git/refs/heads/main"));
static HASH: LazyLock<&'static str> = LazyLock::new(|| {
    if REF.contains("refs/heads/main") {
        &REF_MAIN
    } else {
        &REF
    }
});

#[derive(Clone)]
struct DbMetrics {
    counters_total: IntGauge,
    gauges_total: IntGauge,
    db_size_bytes: IntGauge,
    db_wal_size_bytes: IntGauge,
    db_page_count: IntGauge,
    db_freelist_count: IntGauge,
}

impl DbMetrics {
    fn register(registry: &Registry) -> Self {
        let counters_total =
            IntGauge::new("tickrs_db_counters_total", "Total number of counters").unwrap();
        let gauges_total =
            IntGauge::new("tickrs_db_gauges_total", "Total number of gauges").unwrap();
        let db_size_bytes = IntGauge::new(
            "tickrs_db_size_bytes",
            "Size of the main database file in bytes",
        )
        .unwrap();
        let db_wal_size_bytes =
            IntGauge::new("tickrs_db_wal_size_bytes", "Size of the WAL file in bytes").unwrap();
        let db_page_count =
            IntGauge::new("tickrs_db_page_count", "Total pages in the database").unwrap();
        let db_freelist_count =
            IntGauge::new("tickrs_db_freelist_count", "Free pages available for reuse").unwrap();

        registry.register(Box::new(counters_total.clone())).unwrap();
        registry.register(Box::new(gauges_total.clone())).unwrap();
        registry.register(Box::new(db_size_bytes.clone())).unwrap();
        registry
            .register(Box::new(db_wal_size_bytes.clone()))
            .unwrap();
        registry.register(Box::new(db_page_count.clone())).unwrap();
        registry
            .register(Box::new(db_freelist_count.clone()))
            .unwrap();

        Self {
            counters_total,
            gauges_total,
            db_size_bytes,
            db_wal_size_bytes,
            db_page_count,
            db_freelist_count,
        }
    }

    async fn refresh(&self, pool: &Pool<Sqlite>, db_path: &str) {
        // Row counts
        if let Ok(n) = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM c")
            .fetch_one(pool)
            .await
        {
            self.counters_total.set(n);
        }
        if let Ok(n) = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM g")
            .fetch_one(pool)
            .await
        {
            self.gauges_total.set(n);
        }

        // SQLite page stats
        if let Ok(n) = sqlx::query_scalar::<_, i64>("PRAGMA page_count")
            .fetch_one(pool)
            .await
        {
            self.db_page_count.set(n);
        }
        if let Ok(n) = sqlx::query_scalar::<_, i64>("PRAGMA freelist_count")
            .fetch_one(pool)
            .await
        {
            self.db_freelist_count.set(n);
        }

        // File sizes from the filesystem
        let path = db_path.strip_prefix("sqlite://").unwrap_or(db_path);
        if let Ok(meta) = std::fs::metadata(path) {
            self.db_size_bytes.set(meta.len() as i64);
        }
        let wal_path = format!("{}-wal", path);
        match std::fs::metadata(&wal_path) {
            Ok(meta) => self.db_wal_size_bytes.set(meta.len() as i64),
            Err(_) => self.db_wal_size_bytes.set(0),
        }
    }
}

#[actix_web::main]
async fn main() -> Result<(), Error> {
    dotenvy::dotenv().unwrap();

    let registry = default_registry().clone();
    let prometheus = PrometheusMetricsBuilder::new("api")
        .endpoint("/metrics")
        .registry(registry.clone())
        .build()
        .unwrap();

    let db_url = env::var("DATABASE_URL").expect("DATABASE_URL not configured");
    let pool = SqlitePool::connect(&db_url)
        .await
        .expect("Could not connect to database");

    sqlx::query("PRAGMA journal_mode=WAL")
        .execute(&pool)
        .await
        .expect("Could not enable WAL mode");

    // Register and start periodic DB metrics refresh
    let db_metrics = DbMetrics::register(&registry);
    let db_metrics_bg = db_metrics.clone();
    let pool_bg = pool.clone();
    let db_url_bg = db_url.clone();
    actix_web::rt::spawn(async move {
        let mut ticker = actix_web::rt::time::interval(Duration::from_secs(30));
        loop {
            ticker.tick().await;
            db_metrics_bg.refresh(&pool_bg, &db_url_bg).await;
        }
    });

    let host: Ipv4Addr = env::var("HOST")
        .ok()
        .and_then(|h| h.parse().ok())
        .unwrap_or(Ipv4Addr::new(127, 0, 0, 1));
    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8126);

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
    .shutdown_timeout(30)
    .bind((host, port))?
    .run()
    .await?)
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate;

#[get("/")]
async fn index() -> impl Responder {
    match IndexTemplate.render() {
        Ok(body) => HttpResponse::Ok()
            .insert_header(header::ContentType::html())
            .body(body),
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
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
                .insert_header((header::CONTENT_TYPE, "image/jpeg"))
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
        let rec = sqlx::query!(
            r#"INSERT INTO c (nano_id, value) VALUES (?1, 1)
               ON CONFLICT(nano_id) DO UPDATE SET
                 value = value + 1,
                 updated_at = datetime('now', 'utc')
               RETURNING value"#,
            id
        )
        .fetch_one(&mut *conn)
        .await?;
        Ok(rec.value)
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
        write!(f, "{}", self.value)
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
        let rec = sqlx::query!(
            r#"INSERT INTO g (nano_id, value) VALUES (?1, -1)
               ON CONFLICT(nano_id) DO UPDATE SET
                 value = value - 1,
                 updated_at = datetime('now', 'utc')
               RETURNING value"#,
            id
        )
        .fetch_one(&mut *conn)
        .await?;
        Ok(rec.value)
    }

    async fn increment_or_create(id: &str, pool: &Pool<Sqlite>) -> Result<i64> {
        let mut conn = pool.acquire().await?;
        let rec = sqlx::query!(
            r#"INSERT INTO g (nano_id, value) VALUES (?1, 1)
               ON CONFLICT(nano_id) DO UPDATE SET
                 value = value + 1,
                 updated_at = datetime('now', 'utc')
               RETURNING value"#,
            id
        )
        .fetch_one(&mut *conn)
        .await?;
        Ok(rec.value)
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
        write!(f, "{}", self.value)
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
            .body(format!("{}", i))
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
            .body(format!("{}", i))
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
            .body(format!("{}", i))
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
            .body(format!("{}", i))
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
            .body(format!("{}", i))
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
            .body(format!("{}", i))
    } else {
        HttpResponse::NotFound().body("")
    }
}
