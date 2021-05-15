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

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use crate::dto::{User, ExtraUserIds};

mod dto;

const DISCORD_USER_URI: &str = "https://discord.com/api/v9/users/";
const NQUERY_URI: &str = "http://127.0.0.1:3029/ExtraUserIds?m=";
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

    let discord_user_by_snowflake = warp::path!("users" / String)
        .and(warp::get())
        .and(with_state(user_state.clone()))
        .and_then(discord_user_handler);

    let discord_user_by_machine_id = warp::path!("machineid" / String)
        .and(warp::get())
        .and(with_state(user_state))
        .and_then(neos_machine_id_handler);

    let routes = info
        .or(discord_user_by_snowflake)
        .or(discord_user_by_machine_id);

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

async fn neos_machine_id_handler(machine_id: String, state: UserStateContainer) -> Result<impl warp::Reply, warp::Rejection> {

    match extra_user_id_lookup(machine_id).await {
        Ok(extra_user_ids) => {
            match extra_user_ids.discord {
                Some(discord_id) => {
                    match discord_cached_lookup(discord_id, &state).await {
                        Ok(user) => Ok(Response::builder().status(StatusCode::OK).body(user)),
                        Err(e) => Ok(Response::builder().status(StatusCode::NOT_FOUND).body(e)),
                    }
                }
                None => {
                    Ok(Response::builder().status(StatusCode::NOT_FOUND).body("N/A".to_string()))
                }
            }
        }
        Err(e) => {
            Ok(Response::builder().status(StatusCode::NOT_FOUND).body(e))
        }
    }
}

async fn discord_user_handler(user_id: String, state: UserStateContainer) -> Result<impl warp::Reply, warp::Rejection> {
    match discord_cached_lookup(user_id, &state).await {
        Ok(user) => Ok(Response::builder().status(StatusCode::OK).body(user)),
        Err(e) => Ok(Response::builder().status(StatusCode::NOT_FOUND).body(e)),
    }
}

async fn discord_cached_lookup(user_id: String, state: &UserStateContainer) -> Result<String, String> {
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
        Ok(user_string.user_string.to_owned())
    } else {
        (*mutable_state_mutex).semaphore.acquire().await.expect("semaphore error").forget();
        drop(mutable_state_mutex);

        discord_lookup(user_id, state).await
    }
}

async fn discord_lookup(user_id: String, state: &UserStateContainer) -> Result<String, String> {
    let request = Request::get(DISCORD_USER_URI.to_owned() + &user_id)
        .header("Authorization", "Bot ".to_string() + &state.discord_bot_token)
        .body(hyper::Body::default());
    let request = match request {
        Ok(request) => request,
        Err(e) => {
            restore_permit(state).await;
            return Err(format!("Error building request: {:?}", e));
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
                        let user_string = user.as_pretty_string();
                        let mut mutable_state_mutex = state.mutable_state.write().await;
                        (*mutable_state_mutex).cache.insert(user_id, CachedUser {
                            user_string: user_string.clone(),
                            cache_time: Instant::now(),
                        });
                        println!("Cached new user: {}", user_string);
                        Ok(user_string)
                    }
                    Err(e) => {
                        Err(format!("Error deserializing 200 response: {}", e))
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
                    Ok(error_body) => Err(error_body),
                    Err(error) => {
                        Err(format!("Error reading {} response: {}", status.as_str(), error))
                    }
                }
            }
        }
        Err(e) => {
            restore_permit(&state).await;
            Err(format!("Error performing request: {:?}", e))
        }
    }
}

async fn extra_user_id_lookup(machine_id: String) -> Result<ExtraUserIds, String> {
    let encoded_machine_id = utf8_percent_encode(&machine_id, NON_ALPHANUMERIC).to_string();
    let uri = (NQUERY_URI.to_owned() + &encoded_machine_id).parse()
        .map_err(|e| format!("invalid URI: {:?}", e))?;
    let client = Client::new();
    let response = client.get(uri).await
        .map_err(|e| format!("Error hitting ExtraUserIds endpoint: {:?}", e))?;
    deserialize_extra_user_ids(response).await
}

async fn deserialize_extra_user_ids(response: Response<hyper::Body>) -> Result<ExtraUserIds, String> {
    let body = hyper::body::aggregate(response).await
        .map_err(|e| format!("error aggregating ExtraUserIds response body: {:?}", e))?;
    serde_json::from_reader(body.reader())
        .map_err(|e| format!("error parsing ExtraUserIds response body: {:?}", e))
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
