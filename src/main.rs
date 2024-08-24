use axum::{
	extract::{State},
	http::{header::SET_COOKIE, StatusCode},
	response::{IntoResponse, Response},
	routing::{get, post},
	Router,
};

use axum_extra::TypedHeader;

use headers::Cookie;

use axum::http::HeaderMap;

use rand_core::{OsRng, RngCore};
use std::{collections::HashMap, hash::Hash, sync::Arc, time::Instant};

type UserId = String;
type ChatId = String;

#[derive(Clone)]
struct UserState {
	first_seen: Instant,
	last_seen: Instant,
	chat_ctr: u64,
	chat: Option<ChatId>,
}

impl UserState {
	fn new() -> Self {
		UserState {
			first_seen: Instant::now(),
			last_seen: Instant::now(),
			chat_ctr: 0,
			chat: None
		}
	}
}

#[derive(Clone)]
struct Message {
	sender: UserId,
	time: Instant,
	msg: String,
}

#[derive(Clone)]
struct AppState {
	users: HashMap<UserId, UserState>,
	chats: HashMap<ChatId, Vec<Message>>,
}

#[tokio::main]
async fn main() {
	// initialize tracing
	//tracing_subscriber::fmt::init();

	let state = AppState {
		users: HashMap::new(),
		chats: HashMap::new(),
	};

	// build our application with a route
	let app = Router::new()
		.route("/", get(handle_index))
		.route("/msg", get(read_messages))
		.route("/msg", post(send_message))
		.with_state(state);

	// run our app with hyper, listening globally on port 3000
	let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
	axum::serve(listener, app).await.unwrap();
}

async fn handle_index(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
	format!("hola {}", gen_rand_id())
}

async fn read_messages(
	State(mut state): State<AppState>,
	TypedHeader(cookie): TypedHeader<Cookie>,
) -> impl IntoResponse {
	let uid = cookie
		.get("uid").map(|s| s.to_string())
		.unwrap_or_else(|| {
			let id = gen_rand_id();
			state.users.insert(id.clone(), UserState::new());
			id
		});
	let room = state.users.get(&uid);
	println!("uid: {}", uid);
	format!("hola {}", gen_rand_id())
}

async fn send_message(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
	format!("hola {}", gen_rand_id())
}

fn gen_rand_id() -> String {
	let mut key = [0u8; 20];
	OsRng.fill_bytes(&mut key);
	base64::encode(key)
}

//fn set_uid_cookie(uid: String) -> impl IntoResponse {
//	Response::builder()
//		.status(http::StatusCode::SEE_OTHER)
//		.header("Location", "/")
//		.header(
//			"Set-Cookie",
//			format!("uid={}; Max-Age=999999", uid),
//		)
//		.body(http_body::Empty::new())
//		.unwrap()
//}
