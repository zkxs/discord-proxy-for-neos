use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Buf as _;
use hyper::{Client, Request};
use hyper_tls::HttpsConnector;
use tokio::sync::{RwLock, Semaphore};
use warp::Filter;
use warp::http::{Response, StatusCode};

use crate::user::User;

mod user;

const DISCORD_USER_URI: &str = "https://discord.com/api/v9/users/";
const DEFAULT_CONFIG_FILE_NAME: &str = "discord_token.config";
const CACHE_EXPIRY_DURATION: Duration = Duration::from_secs(60 * 60); // 1 hour

type UserStateContainer = Arc<UserState>;

struct UserState {
    discord_bot_token: String,
    mutable_state: RwLock<MutableUserState>,
}

struct MutableUserState {
    cache: HashMap<String, CachedUser>,
    semaphore: Semaphore,
}

struct CachedUser {
    user_string: String,
    cache_time: Instant,
}

#[tokio::main]
async fn main() {
    println!("Initializing {} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

    let proxy_server_address: SocketAddr = ([127, 0, 0, 1], 3032).into();


    let discord_bot_token = get_default_config_path()
        .and_then(|path|
            fs::read_to_string(path)
                .map_err(|e| format!("{:?}", e))
        )
        .map(|file_contents| file_contents.trim().to_string())
        .unwrap_or_else(|e| {
            eprintln!("Could not read discord bot token for reason: {}", e);
            std::process::exit(1);
        });

    let user_state = Arc::new(UserState {
        discord_bot_token,
        mutable_state: RwLock::new(MutableUserState {
            cache: Default::default(),
            semaphore: Semaphore::new(30),
        }),
    });

    let info = warp::path::end()
        .and(warp::get())
        .map(|| format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")));

    let discord_user = warp::path!("users" / String)
        .and(warp::get())
        .and(with_state(user_state))
        .and_then(discord_user_handler);

    let routes = info
        .or(discord_user);

    println!("Starting web server...");
    warp::serve(routes)
        .run(proxy_server_address)
        .await;
}

fn get_default_config_path() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map_err(|e| format!("{:?}", e))
        .and_then(|path|
            path.parent()
                .map(|p| p.to_path_buf().join(DEFAULT_CONFIG_FILE_NAME))
                .ok_or("Could not find parent directory of this executable".to_string())
        )
}

fn with_state<T: Clone + Send>(db: T) -> impl Filter<Extract=(T, ), Error=std::convert::Infallible> + Clone {
    warp::any().map(move || db.clone())
}

async fn discord_user_handler(user_id: String, state: UserStateContainer) -> Result<impl warp::Reply, warp::Rejection> {
    let mutable_state_mutex = state.mutable_state.read().await;

    let cache_result = match (*mutable_state_mutex).cache.get(&user_id) {
        Some(cached_user) => {
            if cached_user.cache_time.elapsed() > CACHE_EXPIRY_DURATION {
                println!("Expiring cached user: {}", cached_user.user_string);
                None
            } else {
                Some(cached_user)
            }
        }
        None => None
    };

    if let Some(user_string) = cache_result {
        Ok(Response::builder().status(StatusCode::OK).body(user_string.user_string.to_string()))
    } else {
        (*mutable_state_mutex).semaphore.acquire().await.expect("semaphore error").forget();
        drop(mutable_state_mutex);

        let request = Request::get(DISCORD_USER_URI.to_owned() + &user_id)
            .header("Authorization", "Bot ".to_string() + &state.discord_bot_token)
            .body(hyper::Body::default());
        let request = match request {
            Ok(request) => request,
            Err(e) => {
                restore_permit(&state).await;
                return Ok(Response::builder().status(StatusCode::BAD_REQUEST).body(format!("Error building request: {:?}", e)));
            }
        };

        let https = HttpsConnector::new();
        let client = Client::builder().build::<_, hyper::Body>(https);

        match client.request(request).await {
            Ok(response) => {
                if &response.status() == &StatusCode::OK {
                    let delay = response.headers().get("x-ratelimit-reset-after")
                        .and_then(|d| String::from_utf8(d.as_bytes().into()).ok())
                        .and_then(|d| d.parse().ok())
                        .unwrap_or(30.0);

                    println!("permit will return after {}s", delay);

                    // schedule permit restoration
                    let state_clone = state.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                        restore_permit(&state_clone).await;
                    });

                    match deserialize_user(response).await {
                        Ok(user) => {
                            let user_string = user.into_pretty_string();
                            let mut mutable_state_mutex = state.mutable_state.write().await;
                            (*mutable_state_mutex).cache.insert(user_id, CachedUser {
                                user_string: user_string.clone(),
                                cache_time: Instant::now(),
                            });
                            println!("Cached new user: {}", user_string);
                            Ok(Response::builder().status(StatusCode::OK).body(user_string))
                        }
                        Err(e) => {
                            let text = format!("Error deserializing 200 response: {}", e);
                            Ok(Response::builder().status(StatusCode::OK).body(text))
                        }
                    }
                } else {
                    // don't even worry about header parsing
                    let state_clone = state.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        restore_permit(&state_clone).await;
                    });

                    let status = response.status().clone();

                    match deserialize_string(response).await {
                        Ok(error_body) => Ok(Response::builder().status(status).body(error_body)),
                        Err(error) => {
                            let text = format!("Error reading {} response: {}", status.as_str(), error);
                            Ok(Response::builder().status(StatusCode::INTERNAL_SERVER_ERROR).body(text))
                        }
                    }
                }
            }
            Err(e) => {
                restore_permit(&state).await;
                Ok(Response::builder().status(StatusCode::INTERNAL_SERVER_ERROR).body(format!("Error building performing request: {:?}", e)))
            }
        }
    }
}

async fn deserialize_user(response: Response<hyper::Body>) -> Result<User, String> {
    let body = hyper::body::aggregate(response).await
        .map_err(|e| format!("error aggregating user response body: {:?}", e))?;
    serde_json::from_reader(body.reader())
        .map_err(|e| format!("error parsing user response body: {:?}", e))
}

async fn deserialize_string(response: Response<hyper::Body>) -> Result<String, String> {
    hyper::body::to_bytes(response).await
        .map_err(|e| format!("{:?}", e).to_string())
        .and_then(|bytes|
            String::from_utf8(bytes.to_vec())
                .map_err(|e| format!("{:?}", e).to_string())
        )
}

async fn restore_permit(state: &UserStateContainer) {
    let mutable_state_mutex = state.mutable_state.read().await;
    (*mutable_state_mutex).semaphore.add_permits(1);
    println!("permit has returned");
}
