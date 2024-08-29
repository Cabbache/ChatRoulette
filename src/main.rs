use axum::{
	extract::State,
	http::{header::SET_COOKIE, StatusCode},
	response::{IntoResponse},
	routing::{get, post},
	Router,
};

use url::form_urlencoded;

use serde::Serialize;

use base64::{engine::general_purpose::URL_SAFE, Engine as _};

use tera::{Context, Tera};

use axum_extra::TypedHeader;

use headers::Cookie;

use axum::http::HeaderMap;

use rand_core::{OsRng, RngCore};
use std::{
	collections::{HashMap, HashSet},
	sync::{Arc, Mutex},
	time::{SystemTime, UNIX_EPOCH},
};

type UserId = String;
type ChatId = String;

impl UserState {
	fn new(id: UserId) -> Self {
		let tstamp = get_timestamp();
		Self {
			first_seen: tstamp,
			last_seen: tstamp,
			chat_ctr: 0,
			room_id: None,
			id: id,
		}
	}
}

#[derive(Clone, Debug)]
struct Message {
	sender: Option<UserId>,
	time: u64,
	msg: String,
}

#[derive(Clone, Debug, Serialize)]
enum SenderKind {
	YOU,
	THEM,
	SYSTEM,
}

#[derive(Clone, Debug, Serialize)]
struct MessageView {
	senderkind: SenderKind,
	time: u64,
	msg: String,
}

impl Message {
	fn new(from: Option<UserId>, msg: String) -> Self {
		Self {
			sender: from,
			msg: msg,
			time: get_timestamp(),
		}
	}

	fn pov(&self, userid: &Option<UserId>) -> MessageView {
		MessageView {
			senderkind: match userid {
				Some(user) => match &self.sender {
					Some(sender) => if *sender == *user {
						SenderKind::YOU
					} else {
						SenderKind::THEM
					},
					None => SenderKind::SYSTEM
				}
				None => SenderKind::SYSTEM
			},
			time: self.time,
			msg: self.msg.clone()
		}
	}
}

#[derive(Clone, Debug)]
struct UserState {
	first_seen: u64,
	last_seen: u64,
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
	created: u64,
}

impl ChatRoom {
	fn new(id: ChatId, initiator: UserId) -> Self {
		let mut users = HashSet::new();
		users.insert(initiator);
		let timenow = get_timestamp();
		Self {
			id: id,
			users: users,
			terminator: None,
			messages: vec![
				Message {
					sender: None,
					time: timenow,
					msg: format!("Chat initiated {}", timenow)
				}
			],
			created: timenow,
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
	response_headers.insert("Content-Type", "text/html".parse().expect("weird"));

	let template = include_str!("template/index.tera");
	let mut tera = Tera::default();
	tera.add_raw_template("index", template).unwrap();
	let mut context = Context::new();

	let user = {
		let user = cookie
			.get("uid")
			.and_then(|uid| stateguard.users.get_mut(uid));
		match user {
			Some(usr) => {
				usr.last_seen = get_timestamp();
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
				1 => context.insert("waiting", &true),
				2 => {
					context.insert("messages", &roomguard.messages.iter().map(|msg| msg.pov(&Some(user.id.clone()))).collect::<Vec<MessageView>>())
				},
				x => {panic!("Room can't have {} users", x)},
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
					context.insert("messages", &roomguard.messages.iter().map(|msg| msg.pov(&Some(user.id.clone()))).collect::<Vec<MessageView>>());
				} else {
					context.insert("waiting", &true);
				}
			}
			None => {
				let room_id = gen_rand_id_safe(&stateguard.chats);
				let newroom = Arc::new(Mutex::new(ChatRoom::new(room_id.clone(), user.id.clone())));
				stateguard.next_room = Some(newroom.clone());
				stateguard.chats.insert(room_id.clone(), newroom);
				let muser = stateguard.users.get_mut(&user.id).unwrap();
				muser.room_id = Some(room_id);
				context.insert("waiting", &true);
			}
		},
	};

	//context.insert("messages", &vec![Message::new("123".to_string(), "hello1".to_string()), Message::new("123".to_string(), "hello2".to_string())]);
	let response_html = tera.render("index", &context).unwrap();
	(response_headers, response_html)
}

async fn send_message(
	State(state): State<Arc<Mutex<AppState>>>,
	TypedHeader(cookie): TypedHeader<Cookie>,
	body: String,
) -> impl IntoResponse {
	let stateguard = state.lock().unwrap();
	let mut response_headers = HeaderMap::new();
	response_headers.insert("Location", "/".parse().expect("weird"));
	match cookie
		.get("uid")
		.and_then(|uid| stateguard.users.get(uid).map(|user| (uid, user)))
		.and_then(|(uid, user)| user.room_id.as_ref().map(|room_id| (uid, room_id)))
		.and_then(|(uid, room_id)| stateguard.chats.get(room_id).map(|room| (uid, room)))
	{
		Some((uid, room)) => {
			let parsed: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes())
				.into_owned()
				.collect();
			if parsed.len() == 1 {
				let (param, value) = &parsed[0];
				if param == "message" {
					room.lock()
					.unwrap()
					.messages
					.push(Message::new(Some(uid.to_string()), value.to_string()));
				} else {
					eprintln!("param not named message");
				}
			} else {
				eprintln!("got weird params");
			}
		}
		None => {}
	};

	(StatusCode::SEE_OTHER, response_headers)
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

fn get_timestamp() -> u64 {
	let start = SystemTime::now();
	start
		.duration_since(UNIX_EPOCH)
		.expect("Time went backwards")
		.as_millis().try_into().expect("500 million years?")
}
