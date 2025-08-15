use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use dotenv::dotenv;
use fantoccini::{error::CmdError, ClientBuilder, Locator};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env, fs,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    usize,
};
use tokio::{sync::Semaphore, task::JoinSet};

const URL: &str = "https://freshporno.net";
const MAX_CONCURRENT_DRIVERS: usize = 8; // Zwiększamy liczbę równoległych instancji
const MAX_CONCURRENT_PAGES: usize = 16; // Maksymalna liczba stron przetwarzanych równolegle

#[derive(Serialize, Deserialize, Debug, Clone)]
struct VideoData {
    id: usize,
    thumbnail: Option<String>,
    link: String,
    preview: Option<String>,
    video_url: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct PageData {
    page: u32,
    total_items: usize,
    videos: Vec<VideoData>,
    timestamp: String,
}

#[derive(Deserialize)]
struct GetParams {
    page: Option<u32>,
}

struct AppState {
    processed_pages: AtomicU32,
}

// Ultra szybki zapis JSON z cache'owaniem
async fn save_json_locally_fast(data: &PageData, page: u32) -> Result<(), std::io::Error> {
    // Sprawdzamy czy folder istnieje tylko raz
    static FOLDER_CREATED: std::sync::Once = std::sync::Once::new();
    FOLDER_CREATED.call_once(|| {
        let _ = fs::create_dir_all("json");
    });

    let filename = format!("json/json_page_{}.json", page);

    // Używamy fastest JSON serialization
    let json_content = serde_json::to_string(data)?; // Bez pretty - szybsze

    // Asynchroniczny zapis
    tokio::fs::write(&filename, json_content).await?;
    println!("✅ Strona {} zapisana", page);

    Ok(())
}

async fn get_json_handler(
    Query(params): Query<GetParams>,
    State(_state): State<Arc<AppState>>,
) -> Result<Json<PageData>, StatusCode> {
    let page = params.page.unwrap_or(1);
    let filename = format!("json/json_page_{}.json", page);

    match tokio::fs::read_to_string(&filename).await {
        Ok(content) => match serde_json::from_str::<PageData>(&content) {
            Ok(data) => Ok(Json(data)),
            Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        },
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}

async fn start_web_server(state: Arc<AppState>) -> Result<(), std::io::Error> {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    let app = Router::new()
        .route("/", get(|| async { "🚀 Ultra Fast Scraper API" }))
        .route("/get", get(get_json_handler))
        .route(
            "/stats",
            get({
                let state = state.clone();
                move || async move {
                    let processed = state.processed_pages.load(Ordering::Relaxed);
                    Json(serde_json::json!({
                        "processed_pages": processed,
                        "status": "running"
                    }))
                }
            }),
        )
        .with_state(state);

    println!("🌐 Serwer uruchomiony na http://0.0.0.0:3000");
    println!("📊 /stats - statystyki");
    println!("📄 /get?page=X - pobierz dane");

    axum::serve(listener, app).await
}

// Ultra szybki headless Chrome z maksymalnymi optymalizacjami
async fn create_optimized_client() -> Result<fantoccini::Client, CmdError> {
    let chrome_url = env::var("CHROME_URL").unwrap_or_else(|_| "http://localhost:9515".to_string());

    // Ultra optymalizowane ustawienia Chrome
    let mut caps = serde_json::map::Map::new();
    let chrome_opts = serde_json::json!({
           "args": [
          //   "--headless=new", // Nowy headless mode - najszybszy
               "--no-sandbox",
               "--disable-dev-shm-usage",
               "--disable-gpu",
               "--disable-web-security",
               "--disable-extensions",
               "--disable-plugins",
        //       "--disable-images", // Nie ładuje obrazków - MEGA przyspieszenie!
      //         "--disable-javascript", // Wyłączamy JS jeśli niepotrzebny
    //           "--disable-css", // Wyłączamy CSS
               "--disable-plugins-discovery",
               "--disable-preconnect",
               "--disable-prefetch",
               "--disable-background-timer-throttling",
               "--disable-renderer-backgrounding",
               "--disable-backgrounding-occluded-windows",
               "--disable-background-networking",
               "--disable-sync",
               "--disable-translate",
               "--disable-ipc-flooding-protection",
               "--disable-default-apps",
               "--disable-component-extensions-with-background-pages",
               "--disable-client-side-phishing-detection",
               "--disable-hang-monitor",
               "--disable-popup-blocking",
               "--disable-prompt-on-repost",
               "--disable-domain-reliability",
               "--disable-features=TranslateUI,VizDisplayCompositor",
               "--aggressive-cache-discard",
               "--memory-pressure-off",
               "--max_old_space_size=4096",
               "--no-first-run",
               "--no-default-browser-check",
               "--disable-logging",
               "--disable-gpu-logging",
               "--silent",
               "--blink-settings=imagesEnabled=false", // Dodatkowe wyłączenie obrazków
               "--user-agent=Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36", // Mac user agent
               format!("--window-size=1920,1080"),
               "--disable-dev-tools",
               "--remote-debugging-port=0"
           ],
           "prefs": {
               "profile.managed_default_content_settings.images": 2, // Blokada obrazków
               "profile.default_content_setting_values.notifications": 2,
               "profile.managed_default_content_settings.media_stream": 2,
           }
       });

    caps.insert("goog:chromeOptions".to_string(), chrome_opts);
    caps.insert(
        "pageLoadStrategy".to_string(),
        serde_json::Value::String("eager".to_string()),
    ); // Nie czeka na pełne załadowanie

    Ok(ClientBuilder::native()
        .capabilities(caps.into())
        .connect(&chrome_url)
        .await
        .unwrap())
}

// Pool klientów dla maksymalnej wydajności
struct ClientPool {
    clients: Vec<Arc<tokio::sync::Mutex<fantoccini::Client>>>,
    semaphore: Arc<Semaphore>,
}

impl ClientPool {
    async fn new(size: usize) -> Result<Self, CmdError> {
        let mut clients = Vec::with_capacity(size);

        println!("🚀 Tworzę {} instancji Chrome...", size);
        for i in 0..size {
            match create_optimized_client().await {
                Ok(client) => {
                    clients.push(Arc::new(tokio::sync::Mutex::new(client)));
                    print!("✅ ");
                    if (i + 1) % 10 == 0 {
                        println!(" {}/{}", i + 1, size);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Błąd tworzenia klienta {}: {}", i, e);
                    return Err(e);
                }
            }
        }
        println!("\n🎯 Wszystkie instancje Chrome gotowe!");

        Ok(ClientPool {
            clients,
            semaphore: Arc::new(Semaphore::new(size)),
        })
    }

    async fn get_client(&self) -> Option<Arc<tokio::sync::Mutex<fantoccini::Client>>> {
        let _permit = self.semaphore.acquire().await.ok()?;

        // Round-robin selection
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let index = COUNTER.fetch_add(1, Ordering::Relaxed) as usize % self.clients.len();

        Some(self.clients[index].clone())
    }
}

// Ultra szybkie równoległe przetwarzanie
async fn connect_fan() -> Result<(), CmdError> {
    let cpu_count = num_cpus::get();
    let optimal_clients = std::cmp::min(cpu_count * 2, MAX_CONCURRENT_DRIVERS);

    println!("💻 Wykryto {} rdzeni CPU", cpu_count);
    println!(
        "🚀 Uruchamiam {} równoległych instancji Chrome",
        optimal_clients
    );

    let client_pool = Arc::new(ClientPool::new(optimal_clients).await?);

    scrape_all_pages_parallel(client_pool, URL, 1, optimal_clients.try_into().unwrap()).await?;

    Ok(())
}

// Mega szybkie równoległe scrapowanie
async fn scrape_all_pages_parallel(
    client_pool: Arc<ClientPool>,
    base_url: &str,
    start_page: u32,
    size: u32,
) -> Result<(), CmdError> {
    let mut join_set = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_PAGES));
    let base_url = base_url.to_string();
    // Rozpoczynamy od kilku stron równolegle, aby szybko sprawdzić zasięg
    for page in start_page..start_page + size {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let pool = client_pool.clone();
        let url = base_url.clone();

        join_set.spawn(async move {
            let _permit = permit;
            let result = process_single_page_fast(&pool, &url, page).await;
            (page, result)
        });
    }

    let mut current_page = start_page + 5;
    let mut consecutive_empty = 0;

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok((page, Ok(has_content))) => {
                if has_content {
                    consecutive_empty = 0;
                    println!("✅ Strona {} zakończona", page);

                    // Dodajemy kolejne strony dynamicznie
                    if join_set.len() < MAX_CONCURRENT_PAGES / 2 {
                        for _ in 0..3 {
                            let permit = semaphore.clone().acquire_owned().await.unwrap();
                            let pool = client_pool.clone();
                            let url = base_url.clone();
                            let next_page = current_page;
                            current_page += 1;

                            join_set.spawn(async move {
                                let _permit = permit;
                                let result = process_single_page_fast(&pool, &url, next_page).await;
                                (next_page, result)
                            });
                        }
                    }
                } else {
                    consecutive_empty += 1;
                    println!("⚠️ Strona {} pusta", page);

                    if consecutive_empty >= 3 {
                        println!("🏁 Znaleziono 3 puste strony z rzędu. Kończę.");
                        break;
                    }
                }
            }
            Ok((page, Err(e))) => {
                eprintln!("❌ Błąd strony {}: {}", page, e);
            }
            Err(e) => {
                eprintln!("❌ Błąd zadania: {}", e);
            }
        }
    }

    // Czekamy na zakończenie pozostałych zadań
    while let Some(_) = join_set.join_next().await {}

    println!("🎉 Wszystkie strony zakończone!");
    Ok(())
}

// Ultra zoptymalizowane przetwarzanie pojedynczej strony
async fn process_single_page_fast(
    client_pool: &ClientPool,
    base_url: &str,
    page: u32,
) -> Result<bool, CmdError> {
    let client_arc = client_pool.get_client().await.ok_or_else(|| {
        CmdError::NotW3C(serde_json::Value::String(
            "Nie można pobrać klienta z puli".to_string(),
        ))
    })?;

    let client = client_arc.lock().await;

    let page_url = if page == 1 {
        base_url.to_string()
    } else {
        format!("{}/{}/", base_url.trim_end_matches('/'), page)
    };

    println!("🔍 Strona {}: {}", page, page_url);

    // Ustawiamy timeout dla szybszego przetwarzania
    client.goto(&page_url).await?;

    // Ultra szybkie znajdowanie elementów
    let elements = match client
        .find_all(Locator::Css(
            "div.main-wrapper > div:nth-child(2) div.page-content",
        ))
        .await
    {
        Ok(elements) => elements,
        Err(_) => return Ok(false),
    };

    if elements.is_empty() {
        return Ok(false);
    }

    let mut videos: Vec<VideoData> = Vec::with_capacity(elements.len());
    let mut video_tasks = Vec::new();

    // Zbieramy podstawowe dane szybko
    for (y, element) in elements.iter().enumerate() {
        let thumbnail = element
            .find(Locator::Css("img"))
            .await
            .ok()
            .and_then(|img| futures::executor::block_on(img.attr("data-original")).ok())
            .flatten();

        let link = match element.find(Locator::Css("a")).await {
            Ok(a) => match a.attr("href").await {
                Ok(Some(href)) => href,
                _ => continue,
            },
            Err(_) => continue,
        };

        let preview = element
            .find(Locator::Css("div.page-content "))
            .await
            .ok()
            .and_then(|div| futures::executor::block_on(div.attr("data-preview")).ok())
            .flatten();
        let video_url = find_video_fast(&client, &link).await.unwrap_or(None);
        videos.push(VideoData {
            id: y,
            thumbnail,
            link: link.clone(),
            preview,
            video_url: video_url,
        });

        // Dodajemy zadanie pobrania URL video
        let client_for_video = client_arc.clone();
        video_tasks.push(async move {
            let client = client_for_video.lock().await;
            find_video_fast(&*client, &link).await.ok().flatten()
        });
    }

    drop(client); // Zwalniamy klienta wcześnie

    // Równolegle pobieramy wszystkie video URLs
    let video_urls: Vec<Option<String>> = join_all(video_tasks).await;

    // Aktualizujemy video URLs
    for (video, url) in videos.iter_mut().zip(video_urls.iter()) {
        video.video_url = url.clone();
    }

    // Tworzymy dane strony
    let page_data = PageData {
        page,
        total_items: videos.len(),
        videos,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    // Szybki zapis
    tokio::spawn(async move {
        if let Err(e) = save_json_locally_fast(&page_data, page).await {
            eprintln!("❌ Błąd zapisu strony {}: {}", page, e);
        }
    });

    Ok(true)
}

// Ultra szybkie znajdowanie video
async fn find_video_fast(
    client: &fantoccini::Client,
    href: &str,
) -> Result<Option<String>, CmdError> {
    client.goto(href).await?;

    let result = client
        .find(Locator::Css("div.video-block div.fp-player video"))
        .await?
        .attr("src")
        .await?;

    client.back().await?;
    Ok(result)
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    // Sprawdzamy dostępność Chrome
    println!("🔧 Sprawdzam połączenie z Chrome WebDriver...");

    let state = Arc::new(AppState {
        processed_pages: AtomicU32::new(0),
    });

    println!("🚀 ULTRA FAST WEB SCRAPER 3000");
    println!("💻 Rdzenie CPU: {}", num_cpus::get());
    println!("🔥 Max równoległych stron: {}", MAX_CONCURRENT_PAGES);
    println!("⚡ Max instancji Chrome: {}", MAX_CONCURRENT_DRIVERS);
    println!("🎯 Headless: ✅");
    println!("🚫 Obrazki: Wyłączone");
    println!("📱 User Agent: Mac OS X");

    // Uruchamiamy równolegle serwer i scraper
    let (web_result, scraper_result) = tokio::join!(start_web_server(state), connect_fan());

    if let Err(e) = web_result {
        eprintln!("❌ Web server error: {}", e);
    }

    if let Err(e) = scraper_result {
        eprintln!("❌ Scraper error: {}", e);
    }

    println!("🏁 Aplikacja zakończona");
}
