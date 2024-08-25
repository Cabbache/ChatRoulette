use axum::{
	extract::State,
	http::{header::SET_COOKIE, StatusCode},
	response::{IntoResponse, Response},
	routing::{get, post},
	Router,
};

use base64::{engine::general_purpose::URL_SAFE, Engine as _};

use axum_extra::TypedHeader;

use headers::Cookie;

use axum::http::HeaderMap;

use rand_core::{OsRng, RngCore};
use std::{collections::{HashMap, HashSet}, hash::Hash, sync::{Arc, Mutex}, time::Instant};

type UserId = String;
type ChatId = String;

#[derive(Clone, Debug)]
struct UserState {
	first_seen: Instant,
	last_seen: Instant,
	chat_ctr: u64,
	room_id: Option<ChatId>,
	id: UserId,
}

impl UserState {
	fn new(id: UserId) -> Self {
		Self {
			first_seen: Instant::now(),
			last_seen: Instant::now(),
			chat_ctr: 0,
			room_id: None,
			id: id,
		}
	}
}

#[derive(Clone)]
struct Message {
	sender: UserId,
	time: Instant,
	msg: String,
}

impl Message {
	fn new(from: UserId, msg: String) -> Self {
		Self {
			sender: from,
			msg: msg,
			time: Instant::now()
		}
	}
}

#[derive(Clone)]
struct ChatRoom {
	users: HashSet<UserId>,
	messages: Vec<Message>,
	created: Instant,
}

impl ChatRoom {
	fn new(initiator: UserId) -> Self {
		let mut users = HashSet::new();
		users.insert(initiator);
		Self {
			users: users,
			messages: Vec::new(),
			created: Instant::now(),
		}
	}
}

#[derive(Clone)]
struct AppState {
	users: HashMap<UserId, UserState>,
	chats: HashMap<ChatId, Arc<Mutex<ChatRoom>>>,
	next_room: Option<Arc<Mutex<ChatRoom>>>,
}

#[tokio::main]
async fn main() {
	// initialize tracing
	//tracing_subscriber::fmt::init();

	let state = AppState {
		users: HashMap::new(),
		chats: HashMap::new(),
		next_room: None,
	};

	// build our application with a route
	let app = Router::new()
		.route("/", get(read_messages))
		.route("/exit", get(exit_room))
		.route("/", post(send_message))
		.with_state(state);

	// run our app with hyper, listening globally on port 3000
	let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
	axum::serve(listener, app).await.unwrap();
}

async fn exit_room(
	State(mut state): State<AppState>,
	TypedHeader(cookie): TypedHeader<Cookie>,
) -> impl IntoResponse {
	
}

async fn read_messages(
	State(mut state): State<AppState>,
	TypedHeader(cookie): TypedHeader<Cookie>,
) -> impl IntoResponse {
	let mut response_headers = HeaderMap::new();
	let user = cookie.get("uid").and_then(|uid| state.users.get_mut(uid));
	let user = match user {
		Some(usr) => {
			usr.last_seen = Instant::now();
			usr
		}
		None => {
			let newid = gen_rand_id_safe(&state.users);
			response_headers.insert(
				"Set-Cookie",
				format!("uid={};", newid.clone()).parse().unwrap(),
			);
			state
				.users
				.entry(newid.clone())
				.or_insert(UserState::new(newid))
		}
	};

	let resp = match &user.room_id {
		Some(room_id) => state
			.chats
			.get(room_id)
			.unwrap()
			.lock().unwrap()
			.messages
			.iter()
			.cloned()
			.map(|m| m.msg)
			.collect::<Vec<String>>()
			.join("\n"),
		None => match state.next_room {
			Some(room) => {
				let mut roomguard = room.lock().unwrap();
				roomguard.users.insert(user.id.clone());
				if roomguard.users.len() >= 2 {
					state.next_room = None;
					String::from("Joined room")
				} else {
					String::from("Waiting for interlocutor")
				}
			},
			None => {
				let room_id = gen_rand_id_safe(&state.chats);
				let newroom = Arc::new(Mutex::new(ChatRoom::new(user.id.clone())));
				state.next_room = Some(newroom.clone());
				state.chats.insert(room_id, newroom);
				String::from("Waiting for interlocutor")
			}
		}
	};

	(response_headers, resp)
}

async fn send_message(
	State(mut state): State<AppState>,
	TypedHeader(cookie): TypedHeader<Cookie>,
	body: String,
) -> impl IntoResponse {
	let mut response_headers = HeaderMap::new();
	match cookie.get("uid")
		.and_then(|uid| state.users.get(uid).map(|user| (uid, user)))
		.and_then(|(uid, user)| user.room_id.as_ref().map(|room_id| (uid, room_id)))
		.and_then(|(uid, room_id)| state.chats.get_mut(room_id).map(|room| (uid, room)))
	{
		Some((uid, room)) => {
			room.lock().unwrap().messages.push(Message::new(uid.to_string(), body));
		},
		None => {
			response_headers.insert("Location", "/".parse().expect("weird"));
		}
	};

	response_headers
}

fn gen_rand_id_safe<T>(map: &HashMap<String, T>) -> String {
	loop {
		let id = gen_rand_id();
		if !map.contains_key(&id) {
			break id;
		}
	}
}

fn gen_rand_id() -> String {
	let mut key = [0u8; 21];
	OsRng.fill_bytes(&mut key);
	URL_SAFE.encode(key)
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
