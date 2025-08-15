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
    env, fs,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Instant,
    usize,
};
use tokio::{sync::Semaphore, task::JoinSet};

const URL: &str = "https://freshporno.net";
const MAX_CONCURRENT_DRIVERS: usize = 8;
const MAX_CONCURRENT_PAGES: usize = 16;

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

async fn save_json_locally_fast(data: &PageData, page: u32) -> Result<(), std::io::Error> {
    println!("💾 [SAVE] Rozpoczynam zapisywanie strony {}", page);
    let start = Instant::now();

    static FOLDER_CREATED: std::sync::Once = std::sync::Once::new();
    FOLDER_CREATED.call_once(|| {
        println!("📁 [SAVE] Tworzę folder 'json'");
        let _ = fs::create_dir_all("json");
    });

    let filename = format!("json/json_page_{}.json", page);
    println!(
        "📝 [SAVE] Serializuję dane strony {} ({} elementów)",
        page,
        data.videos.len()
    );

    let json_content = serde_json::to_string(data)?;
    println!(
        "📄 [SAVE] JSON ma {} bajtów dla strony {}",
        json_content.len(),
        page
    );

    tokio::fs::write(&filename, json_content).await?;
    println!("✅ [SAVE] Strona {} zapisana w {:?}", page, start.elapsed());

    Ok(())
}

async fn get_json_handler(
    Query(params): Query<GetParams>,
    State(_state): State<Arc<AppState>>,
) -> Result<Json<PageData>, StatusCode> {
    let page = params.page.unwrap_or(1);
    println!("🌐 [API] Żądanie pobrania danych strony {}", page);

    let filename = format!("json/json_page_{}.json", page);

    match tokio::fs::read_to_string(&filename).await {
        Ok(content) => {
            println!("📖 [API] Znaleziono plik dla strony {}", page);
            match serde_json::from_str::<PageData>(&content) {
                Ok(data) => {
                    println!(
                        "✅ [API] Zwracam dane strony {} ({} elementów)",
                        page,
                        data.videos.len()
                    );
                    Ok(Json(data))
                }
                Err(e) => {
                    println!("❌ [API] Błąd deserializacji strony {}: {}", page, e);
                    Err(StatusCode::INTERNAL_SERVER_ERROR)
                }
            }
        }
        Err(e) => {
            println!("❌ [API] Nie znaleziono pliku strony {}: {}", page, e);
            Err(StatusCode::NOT_FOUND)
        }
    }
}

async fn start_web_server(state: Arc<AppState>) -> Result<(), std::io::Error> {
    println!("🌐 [SERVER] Uruchamiam serwer web...");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    println!("🌐 [SERVER] Listener utworzony na porcie 3000");

    let app = Router::new()
        .route(
            "/",
            get(|| async {
                println!("🌐 [SERVER] Żądanie GET /");
                "🚀 Ultra Fast Scraper API"
            }),
        )
        .route("/get", get(get_json_handler))
        .route(
            "/stats",
            get({
                let state = state.clone();
                move || async move {
                    let processed = state.processed_pages.load(Ordering::Relaxed);
                    println!(
                        "📊 [SERVER] Żądanie stats - przetworzono {} stron",
                        processed
                    );
                    Json(serde_json::json!({
                        "processed_pages": processed,
                        "status": "running"
                    }))
                }
            }),
        )
        .with_state(state);

    println!("🌐 [SERVER] Serwer uruchomiony na http://0.0.0.0:3000");
    println!("📊 [SERVER] /stats - statystyki");
    println!("📄 [SERVER] /get?page=X - pobierz dane");

    axum::serve(listener, app).await
}

async fn create_optimized_client() -> Result<fantoccini::Client, CmdError> {
    println!("🔧 [CLIENT] Tworzę nowego klienta Chrome...");
    let start = Instant::now();

    let chrome_url = env::var("CHROME_URL").unwrap_or_else(|_| {
        println!("🔧 [CLIENT] Używam domyślnego URL Chrome: http://localhost:9515");
        "http://localhost:9515".to_string()
    });

    println!("🔧 [CLIENT] Łączę z Chrome WebDriver na: {}", chrome_url);

    let mut caps = serde_json::map::Map::new();
    let chrome_opts = serde_json::json!({
        "args": [
              "headless=new", // Nowy headless mode - najszybszy
              "--disable-images", // Nie ładuje obrazków - MEGA przyspieszenie! //
             "--disable-javascript", // Wyłączamy JS jeśli niepotrzebny //
              // "--disable-css", // Wyłączamy CSS
            "--no-sandbox",
            "--disable-dev-shm-usage",
            "--disable-gpu",
            "--disable-web-security",
            "--disable-extensions",
            "--disable-plugins",
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
            "--blink-settings=imagesEnabled=false",
            "--user-agent=Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
            format!("--window-size=1920,1080"),
            "--disable-dev-tools",
            "--remote-debugging-port=0"
        ],
        "prefs": {
       //     "profile.managed_default_content_settings.images": 2,
                 "profile.managed_default_content_settings.images": 1,
        //    "profile.default_content_setting_values.notifications": 2,

        "profile.default_content_setting_values.notifications": 1,
            //  "profile.managed_default_content_settings.media_stream": 2,
                    "profile.managed_default_content_settings.media_stream": 1,

        }
    });

    caps.insert("goog:chromeOptions".to_string(), chrome_opts);
    caps.insert(
        "pageLoadStrategy".to_string(),
        serde_json::Value::String("eager".to_string()),
    );

    println!("🔧 [CLIENT] Konfiguracja Chrome gotowa, łączę...");

    let client = ClientBuilder::native()
        .capabilities(caps.into())
        .connect(&chrome_url)
        .await
        .map_err(|e| {
            CmdError::NotW3C(serde_json::Value::String(format!(
                "Błąd połączenia z Chrome: {}",
                e
            )))
        })?;

    println!(
        "✅ [CLIENT] Klient Chrome utworzony w {:?}",
        start.elapsed()
    );
    Ok(client)
}

struct ClientPool {
    clients: Vec<Arc<tokio::sync::Mutex<fantoccini::Client>>>,
    semaphore: Arc<Semaphore>,
}

impl ClientPool {
    async fn new(size: usize) -> Result<Self, CmdError> {
        println!("🚀 [POOL] Tworzę pulę {} instancji Chrome...", size);
        let start = Instant::now();
        let mut clients = Vec::with_capacity(size);

        for i in 0..size {
            println!("🔧 [POOL] Tworzę klienta {}/{}", i + 1, size);
            let client_start = Instant::now();

            match create_optimized_client().await {
                Ok(client) => {
                    clients.push(Arc::new(tokio::sync::Mutex::new(client)));
                    println!(
                        "✅ [POOL] Klient {}/{} utworzony w {:?}",
                        i + 1,
                        size,
                        client_start.elapsed()
                    );
                }
                Err(e) => {
                    println!("❌ [POOL] Błąd tworzenia klienta {}: {}", i, e);
                    return Err(e);
                }
            }
        }

        println!(
            "🎯 [POOL] Wszystkie {} instancji Chrome gotowe w {:?}!",
            size,
            start.elapsed()
        );

        Ok(ClientPool {
            clients,
            semaphore: Arc::new(Semaphore::new(size)),
        })
    }

    async fn get_client(&self) -> Option<Arc<tokio::sync::Mutex<fantoccini::Client>>> {
        println!("🔄 [POOL] Próbuję pobrać klienta z puli...");
        let _permit = self.semaphore.acquire().await.ok()?;

        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let index = COUNTER.fetch_add(1, Ordering::Relaxed) as usize % self.clients.len();

        println!("✅ [POOL] Przydzielono klienta #{}", index);
        Some(self.clients[index].clone())
    }
}

async fn connect_fan() -> Result<(), CmdError> {
    println!("🚀 [MAIN] Rozpoczynam główny proces scrapowania...");

    let cpu_count = num_cpus::get();
    let optimal_clients = std::cmp::min(cpu_count * 2, MAX_CONCURRENT_DRIVERS);

    println!("💻 [MAIN] Wykryto {} rdzeni CPU", cpu_count);
    println!(
        "🚀 [MAIN] Uruchamiam {} równoległych instancji Chrome",
        optimal_clients
    );

    println!("🔧 [MAIN] Tworzę pulę klientów...");
    let client_pool = Arc::new(ClientPool::new(optimal_clients).await?);
    println!("✅ [MAIN] Pula klientów gotowa!");

    println!("🎯 [MAIN] Rozpoczynam scrapowanie od URL: {}", URL);
    scrape_all_pages_parallel(client_pool, URL, 1, optimal_clients.try_into().unwrap()).await?;

    println!("🏁 [MAIN] Proces scrapowania zakończony!");
    Ok(())
}

async fn scrape_all_pages_parallel(
    client_pool: Arc<ClientPool>,
    base_url: &str,
    start_page: u32,
    size: u32,
) -> Result<(), CmdError> {
    println!("📋 [SCRAPER] Rozpoczynam równoległe scrapowanie...");
    println!("📋 [SCRAPER] URL bazowy: {}", base_url);
    println!("📋 [SCRAPER] Strona startowa: {}", start_page);
    println!("📋 [SCRAPER] Liczba początkowych stron: {}", size);
    println!(
        "📋 [SCRAPER] Max równoległych stron: {}",
        MAX_CONCURRENT_PAGES
    );

    let mut join_set = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_PAGES));
    let base_url = base_url.to_string();

    println!("🚀 [SCRAPER] Dodaję pierwsze {} stron do kolejki...", size);
    for page in start_page..start_page + size {
        println!("📝 [SCRAPER] Dodaję stronę {} do kolejki", page);
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let pool = client_pool.clone();
        let url = base_url.clone();

        join_set.spawn(async move {
            println!("▶️ [TASK] Rozpoczynam zadanie dla strony {}", page);
            let _permit = permit;
            let result = process_single_page_fast(&pool, &url, page).await;
            println!(
                "🏁 [TASK] Zadanie strony {} zakończone: {:?}",
                page,
                result.is_ok()
            );
            (page, result)
        });
    }

    let mut current_page = start_page + size;
    let mut consecutive_empty = 0;
    let mut completed_pages = 0;

    println!("🔄 [SCRAPER] Czekam na wyniki zadań...");

    while let Some(result) = join_set.join_next().await {
        println!("📨 [SCRAPER] Otrzymano wynik zadania");

        match result {
            Ok((page, Ok(has_content))) => {
                completed_pages += 1;
                println!(
                    "✅ [SCRAPER] Strona {} zakończona pomyślnie (zawiera treść: {})",
                    page, has_content
                );
                println!(
                    "📊 [SCRAPER] Ukończono {} stron, aktywnych zadań: {}",
                    completed_pages,
                    join_set.len()
                );

                if has_content {
                    consecutive_empty = 0;
                    println!(
                        "🎯 [SCRAPER] Strona {} miała treść - resetuję licznik pustych stron",
                        page
                    );

                    if join_set.len() < MAX_CONCURRENT_PAGES / 2 {
                        println!(
                            "📈 [SCRAPER] Mało aktywnych zadań ({}), dodaję więcej...",
                            join_set.len()
                        );
                        for i in 0..3 {
                            let new_page = current_page + i;
                            println!("📝 [SCRAPER] Dodaję nową stronę {} do kolejki", new_page);
                            let permit = semaphore.clone().acquire_owned().await.unwrap();
                            let pool = client_pool.clone();
                            let url = base_url.clone();

                            join_set.spawn(async move {
                                println!(
                                    "▶️ [TASK] Rozpoczynam zadanie dla nowej strony {}",
                                    new_page
                                );
                                let _permit = permit;
                                let result = process_single_page_fast(&pool, &url, new_page).await;
                                println!(
                                    "🏁 [TASK] Zadanie nowej strony {} zakończone: {:?}",
                                    new_page,
                                    result.is_ok()
                                );
                                (new_page, result)
                            });
                        }
                        current_page += 3;
                        println!("📊 [SCRAPER] Następna strona do dodania: {}", current_page);
                    }
                } else {
                    consecutive_empty += 1;
                    println!(
                        "⚠️ [SCRAPER] Strona {} była pusta (puste z rzędu: {})",
                        page, consecutive_empty
                    );

                    if consecutive_empty >= 3 {
                        println!("🛑 [SCRAPER] Znaleziono 3 puste strony z rzędu. Kończę dodawanie nowych stron.");
                        break;
                    }
                }
            }
            Ok((page, Err(e))) => {
                completed_pages += 1;
                println!("❌ [SCRAPER] Błąd na stronie {}: {}", page, e);
                println!(
                    "📊 [SCRAPER] Ukończono {} stron (z błędem), aktywnych zadań: {}",
                    completed_pages,
                    join_set.len()
                );
            }
            Err(e) => {
                completed_pages += 1;
                println!("❌ [SCRAPER] Błąd zadania: {}", e);
                println!(
                    "📊 [SCRAPER] Ukończono {} stron (błąd zadania), aktywnych zadań: {}",
                    completed_pages,
                    join_set.len()
                );
            }
        }
    }

    println!(
        "⏳ [SCRAPER] Czekam na zakończenie pozostałych {} zadań...",
        join_set.len()
    );
    let mut remaining = 0;
    while let Some(_) = join_set.join_next().await {
        remaining += 1;
        println!("🏁 [SCRAPER] Zakończono pozostałe zadanie #{}", remaining);
    }

    println!(
        "🎉 [SCRAPER] Wszystkie strony zakończone! Łącznie przetworzono {} stron",
        completed_pages
    );
    Ok(())
}

async fn process_single_page_fast(
    client_pool: &ClientPool,
    base_url: &str,
    page: u32,
) -> Result<bool, CmdError> {
    println!("🔍 [PAGE-{}] Rozpoczynam przetwarzanie strony", page);
    let page_start = Instant::now();

    println!("🔄 [PAGE-{}] Pobieram klienta z puli...", page);
    let client_arc = client_pool.get_client().await.ok_or_else(|| {
        println!("❌ [PAGE-{}] Nie można pobrać klienta z puli", page);
        CmdError::NotW3C(serde_json::Value::String(
            "Nie można pobrać klienta z puli".to_string(),
        ))
    })?;

    println!("🔒 [PAGE-{}] Blokuję klienta...", page);
    let client = client_arc.lock().await;
    println!("✅ [PAGE-{}] Klient zablokowany", page);

    let page_url = if page == 1 {
        base_url.to_string()
    } else {
        format!("{}/{}/", base_url.trim_end_matches('/'), page)
    };

    println!("🌐 [PAGE-{}] Nawiguję do URL: {}", page, page_url);
    let nav_start = Instant::now();
    client.goto(&page_url).await?;
    println!(
        "✅ [PAGE-{}] Nawigacja ukończona w {:?}",
        page,
        nav_start.elapsed()
    );

    println!("🔍 [PAGE-{}] Szukam elementów na stronie...", page);
    let find_start = Instant::now();
    let elements = match client
        .find_all(Locator::Css(
            "div.main-wrapper > div:nth-child(2) div.page-content",
        ))
        .await
    {
        Ok(elements) => {
            println!(
                "✅ [PAGE-{}] Znaleziono {} elementów w {:?}",
                page,
                elements.len(),
                find_start.elapsed()
            );
            elements
        }
        Err(e) => {
            println!("❌ [PAGE-{}] Nie znaleziono elementów: {}", page, e);
            return Ok(false);
        }
    };

    if elements.is_empty() {
        println!("⚠️ [PAGE-{}] Strona pusta - brak elementów", page);
        return Ok(false);
    }

    let mut videos: Vec<VideoData> = Vec::with_capacity(elements.len());
    let mut video_tasks = Vec::new();

    println!(
        "📝 [PAGE-{}] Przetwarzam {} elementów...",
        page,
        elements.len()
    );

    for (y, element) in elements.iter().enumerate() {
        println!("🔍 [PAGE-{}] Element {}/{}", page, y + 1, elements.len());

        let thumbnail = element
            .find(Locator::Css("img"))
            .await
            .ok()
            .and_then(|img| {
                println!("🖼️ [PAGE-{}] Szukam thumbnail dla elementu {}", page, y + 1);
                futures::executor::block_on(img.attr("data-original")).ok()
            })
            .flatten();
        println!(
            "📷 [PAGE-{}] Thumbnail elementu {}: {:?}",
            page,
            y + 1,
            thumbnail.is_some()
        );

        let link = match element.find(Locator::Css("a")).await {
            Ok(a) => match a.attr("href").await {
                Ok(Some(href)) => {
                    println!("🔗 [PAGE-{}] Link elementu {}: {}", page, y + 1, href);
                    href
                }
                Ok(None) => {
                    println!("❌ [PAGE-{}] Element {} nie ma atrybutu href", page, y + 1);
                    continue;
                }
                Err(e) => {
                    println!(
                        "❌ [PAGE-{}] Błąd pobierania href elementu {}: {}",
                        page,
                        y + 1,
                        e
                    );
                    continue;
                }
            },
            Err(e) => {
                println!(
                    "❌ [PAGE-{}] Nie znaleziono linka w elemencie {}: {}",
                    page,
                    y + 1,
                    e
                );
                continue;
            }
        };

        let preview = element
            .find(Locator::Css("div.page-content"))
            .await
            .ok()
            .and_then(|div| {
                println!("🎬 [PAGE-{}] Szukam preview dla elementu {}", page, y + 1);
                futures::executor::block_on(div.attr("data-preview")).ok()
            })
            .flatten();
        println!(
            "🎥 [PAGE-{}] Preview elementu {}: {:?}",
            page,
            y + 1,
            preview.is_some()
        );

        println!(
            "📹 [PAGE-{}] Szukam video URL dla elementu {}...",
            page,
            y + 1
        );
        let video_url = find_video_fast(&client, &link).await.unwrap_or(None);
        println!(
            "🎦 [PAGE-{}] Video URL elementu {}: {:?}",
            page,
            y + 1,
            video_url.is_some()
        );

        videos.push(VideoData {
            id: y,
            thumbnail,
            link: link.clone(),
            preview,
            video_url,
        });

        let client_for_video = client_arc.clone();
        video_tasks.push(async move {
            println!(
                "🔄 [VIDEO-TASK] Rozpoczynam zadanie video dla elementu {}",
                y + 1
            );
            let client = client_for_video.lock().await;
            let result = find_video_fast(&*client, &link).await.ok().flatten();
            println!(
                "🏁 [VIDEO-TASK] Zadanie video elementu {} zakończone: {:?}",
                y + 1,
                result.is_some()
            );
            result
        });
    }

    println!("🔓 [PAGE-{}] Zwalnianie klienta...", page);
    drop(client);
    println!("✅ [PAGE-{}] Klient zwolniony", page);

    println!(
        "🚀 [PAGE-{}] Uruchamiam {} równoległych zadań video...",
        page,
        video_tasks.len()
    );
    let video_start = Instant::now();
    let video_urls: Vec<Option<String>> = join_all(video_tasks).await;
    println!(
        "✅ [PAGE-{}] Wszystkie zadania video zakończone w {:?}",
        page,
        video_start.elapsed()
    );

    println!("🔄 [PAGE-{}] Aktualizuję URLs video...", page);
    for (video, url) in videos.iter_mut().zip(video_urls.iter()) {
        video.video_url = url.clone();
    }

    println!(
        "📋 [PAGE-{}] Tworzę dane strony ({} elementów)...",
        page,
        videos.len()
    );
    let page_data = PageData {
        page,
        total_items: videos.len(),
        videos,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    println!("💾 [PAGE-{}] Uruchamiam zapis asynchroniczny...", page);
    tokio::spawn(async move {
        if let Err(e) = save_json_locally_fast(&page_data, page).await {
            println!("❌ [SAVE] Błąd zapisu strony {}: {}", page, e);
        }
    });

    println!(
        "🎉 [PAGE-{}] Strona ukończona w {:?}",
        page,
        page_start.elapsed()
    );
    Ok(true)
}

async fn find_video_fast(
    client: &fantoccini::Client,
    href: &str,
) -> Result<Option<String>, CmdError> {
    println!("🔍 [VIDEO] Szukam video na: {}", href);
    let video_start = Instant::now();

    println!("🌐 [VIDEO] Nawigacja do strony video...");
    client.goto(href).await?;
    println!("✅ [VIDEO] Nawigacja ukończona");

    println!("🎬 [VIDEO] Szukam elementu video...");
    let result = client
        .find(Locator::Css("div.video-block div.fp-player video"))
        .await?
        .attr("src")
        .await?;

    println!("🔙 [VIDEO] Powrót do poprzedniej strony...");
    client.back().await?;

    println!(
        "🎥 [VIDEO] Wyszukiwanie video zakończone w {:?} - znaleziono: {:?}",
        video_start.elapsed(),
        result.is_some()
    );
    Ok(result)
}

#[tokio::main]
async fn main() {
    println!("🚀 ULTRA FAST WEB SCRAPER 3000 - DETAILED LOGGING");
    println!("================================================");

    println!("🔧 [INIT] Ładuję zmienne środowiskowe...");
    dotenv().ok();

    println!("🔧 [INIT] Sprawdzam połączenie z Chrome WebDriver...");

    let state = Arc::new(AppState {
        processed_pages: AtomicU32::new(0),
    });

    println!("💻 [INIT] Rdzenie CPU: {}", num_cpus::get());
    println!("🔥 [INIT] Max równoległych stron: {}", MAX_CONCURRENT_PAGES);
    println!("⚡ [INIT] Max instancji Chrome: {}", MAX_CONCURRENT_DRIVERS);
    println!("🎯 [INIT] Headless: ✅");
    println!("🚫 [INIT] Obrazki: Wyłączone");
    println!("📱 [INIT] User Agent: Mac OS X");

    println!("🚀 [INIT] Uruchamiam równolegle serwer i scraper...");
    let (web_result, scraper_result) = tokio::join!(start_web_server(state), connect_fan());

    if let Err(e) = web_result {
        println!("❌ [MAIN] Web server error: {}", e);
    }

    if let Err(e) = scraper_result {
        println!("❌ [MAIN] Scraper error: {}", e);
    }

    println!("🏁 [MAIN] Aplikacja zakończona");
}
