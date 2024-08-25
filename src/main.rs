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
use std::{
	collections::{HashMap, HashSet},
	sync::{Arc, Mutex},
	time::Instant,
};

type UserId = String;
type ChatId = String;

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

#[derive(Clone, Debug)]
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
			time: Instant::now(),
		}
	}
}

#[derive(Clone, Debug)]
struct UserState {
	first_seen: Instant,
	last_seen: Instant,
	chat_ctr: u64,
	room_id: Option<ChatId>,
	id: UserId,
}

#[derive(Clone, Debug)]
struct ChatRoom {
	id: ChatId,
	users: HashSet<UserId>,
	messages: Vec<Message>,
	terminator: Option<UserId>,
	created: Instant,
}

impl ChatRoom {
	fn new(id: ChatId, initiator: UserId) -> Self {
		let mut users = HashSet::new();
		users.insert(initiator);
		Self {
			id: id,
			users: users,
			terminator: None,
			messages: Vec::new(),
			created: Instant::now(),
		}
	}
}

#[derive(Clone, Debug)]
struct AppState {
	users: HashMap<UserId, UserState>,
	chats: HashMap<ChatId, Arc<Mutex<ChatRoom>>>,
	next_room: Option<Arc<Mutex<ChatRoom>>>,
}

#[tokio::main]
async fn main() {
	// initialize tracing
	//tracing_subscriber::fmt::init();

	let state = Arc::new(Mutex::new(AppState {
		users: HashMap::new(),
		chats: HashMap::new(),
		next_room: None,
	}));

	// build our application with a route
	let app = Router::new()
		.route("/", get(read_messages))
		.route("/dump", get(dump_states))
		.route("/exit", get(exit_room))
		.route("/", post(send_message))
		.with_state(state);

	// run our app with hyper, listening globally on port 3000
	let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
	axum::serve(listener, app).await.unwrap();
}

async fn exit_room(
	State(state): State<Arc<Mutex<AppState>>>,
	TypedHeader(cookie): TypedHeader<Cookie>,
) -> impl IntoResponse {
	let mut stateguard = state.lock().unwrap();
	let mut response_headers = HeaderMap::new();
	match cookie
		.get("uid")
		.and_then(|uid| stateguard.users.get(uid).map(|user| (uid, user)))
		.and_then(|(uid, user)| user.room_id.as_ref().map(|room_id| (uid, room_id)))
		.and_then(|(uid, room_id)| stateguard.chats.get(room_id).map(|room| (uid, room.clone())))
	{
		Some((uid, room)) => {
			let mut roomguard = room.lock().unwrap();
			roomguard.terminator = Some(uid.to_string());
			let muser = stateguard.users.get_mut(uid).unwrap();
			muser.room_id = None;
		}
		None => {
			response_headers.insert("Location", "/".parse().expect("weird"));
		}
	};

	response_headers
}

async fn read_messages(
	State(state): State<Arc<Mutex<AppState>>>,
	TypedHeader(cookie): TypedHeader<Cookie>,
) -> impl IntoResponse {
	let mut stateguard = state.lock().unwrap();
	let mut response_headers = HeaderMap::new();

	let user = {
		let user = cookie
			.get("uid")
			.and_then(|uid| stateguard.users.get_mut(uid));
		match user {
			Some(usr) => {
				usr.last_seen = Instant::now();
				usr.clone()
			}
			None => {
				let newid = gen_rand_id_safe(&stateguard.users);
				response_headers.insert(
					SET_COOKIE,
					format!("uid={};", newid.clone()).parse().unwrap(),
				);
				let newuser = UserState::new(newid.clone());
				stateguard.users.insert(newid, newuser.clone());
				newuser
			}
		}
	};

	let resp = match &user.room_id {
		Some(room_id) => {
			let roomguard = stateguard.chats.get(room_id).unwrap().lock().unwrap();

			match roomguard.users.len() {
				1 => String::from("Waiting for interlocutor"),
				2 => roomguard
					.messages
					.iter()
					.cloned()
					.map(|m| m.msg)
					.collect::<Vec<String>>()
					.join("\n"),
				_ => String::from("Room in unknown state"),
			}
		}
		None => match &stateguard.next_room.clone() {
			Some(room) => {
				let mut roomguard = room.lock().unwrap();
				roomguard.users.insert(user.id.clone());
				let muser = stateguard.users.get_mut(&user.id).unwrap();
				muser.room_id = Some(roomguard.id.clone());
				if roomguard.users.len() >= 2 {
					stateguard.next_room = None;
					String::from("Joined room")
				} else {
					String::from("Waiting for interlocutor")
				}
			}
			None => {
				let room_id = gen_rand_id_safe(&stateguard.chats);
				let newroom = Arc::new(Mutex::new(ChatRoom::new(room_id.clone(), user.id.clone())));
				stateguard.next_room = Some(newroom.clone());
				stateguard.chats.insert(room_id.clone(), newroom);
				let muser = stateguard.users.get_mut(&user.id).unwrap();
				muser.room_id = Some(room_id);
				String::from("Waiting for interlocutor")
			}
		},
	};

	(response_headers, resp)
}

async fn send_message(
	State(state): State<Arc<Mutex<AppState>>>,
	TypedHeader(cookie): TypedHeader<Cookie>,
	body: String,
) -> impl IntoResponse {
	let stateguard = state.lock().unwrap();
	let mut response_headers = HeaderMap::new();
	match cookie
		.get("uid")
		.and_then(|uid| stateguard.users.get(uid).map(|user| (uid, user)))
		.and_then(|(uid, user)| user.room_id.as_ref().map(|room_id| (uid, room_id)))
		.and_then(|(uid, room_id)| stateguard.chats.get(room_id).map(|room| (uid, room)))
	{
		Some((uid, room)) => {
			room.lock()
				.unwrap()
				.messages
				.push(Message::new(uid.to_string(), body));
		}
		None => {
			response_headers.insert("Location", "/".parse().expect("weird"));
		}
	};

	response_headers
}

async fn dump_states(State(state): State<Arc<Mutex<AppState>>>) -> impl IntoResponse {
	let stateguard = state.lock().unwrap();
	let mut dump = String::new();
	for (id, user) in &stateguard.users {
		dump = format!(
			"{}\n{}: {}",
			dump,
			id,
			user.room_id.clone().unwrap_or("none".to_string())
		);
	}
	for (roomid, room) in &stateguard.chats {
		dump = format!("{}\n{} -> {:?}", dump, roomid, room.lock().unwrap().users);
	}
	dump
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
