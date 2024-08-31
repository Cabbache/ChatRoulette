use axum::{
	extract::State,
	http::{header::SET_COOKIE, StatusCode},
	response::IntoResponse,
	routing::{get, post},
	Router,
};

use env_logger;

use log::{info, trace, debug};

use axum::http::HeaderMap;
use axum_extra::TypedHeader;
use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use headers::Cookie;
use serde::Serialize;
use tera::{Context, Tera};
use url::form_urlencoded;

use tokio::time;

use rand_core::{OsRng, RngCore};
use std::{
	collections::{HashMap, HashSet},
	sync::{Arc, Mutex},
	time::{Duration, SystemTime, UNIX_EPOCH},
};

type UserId = String;
type ChatId = String;
type TimeStamp = u64;

trait Elapsed {
	fn elapsed(&self) -> u64;
}

impl Elapsed for TimeStamp {
	fn elapsed(&self) -> u64 {
		get_timestamp() - self
	}
}

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
	time: String,
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
		let elapsed = get_timestamp() - self.time;
		MessageView {
			senderkind: match userid {
				Some(user) => match &self.sender {
					Some(sender) => {
						if *sender == *user {
							SenderKind::YOU
						} else {
							SenderKind::THEM
						}
					}
					None => SenderKind::SYSTEM,
				},
				None => SenderKind::SYSTEM,
			},
			time: format_duration(elapsed),
			msg: self.msg.clone(),
		}
	}
}

#[derive(Clone, Debug)]
struct UserState {
	first_seen: TimeStamp,
	last_seen: TimeStamp,
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
	created: TimeStamp,
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
			messages: vec![Message {
				sender: None,
				time: timenow,
				msg: String::from("Chat initiated"),
			}],
			created: timenow,
		}
	}

	fn dump(&self) -> String {
		self
			.messages
			.iter()
			.map(|msg| format!("{}|{}|{}", msg.time, msg.sender.clone().unwrap_or("system".to_string()), msg.msg))
			.collect::<Vec<String>>()
			.join("\n")
	}
}

#[derive(Clone, Debug)]
struct AppState {
	users: HashMap<UserId, UserState>,
	chats: HashMap<ChatId, Arc<Mutex<ChatRoom>>>,
	next_room: Option<Arc<Mutex<ChatRoom>>>,
}

fn format_duration(milliseconds: u64) -> String {
	let seconds_in_minute = 60;
	let seconds_in_hour = 3600;

	if milliseconds >= seconds_in_hour * 1000 {
		format!("{}h", milliseconds / (seconds_in_hour * 1000))
	} else if milliseconds >= seconds_in_minute * 1000 {
		format!("{}m", milliseconds / (seconds_in_minute * 1000))
	} else if milliseconds < 5000 {
		String::from("now")
	} else {
		format!("{}s", milliseconds / 1000)
	}
}

async fn cleanup(millis: u64, state: Arc<Mutex<AppState>>) {
	let mut interval = time::interval(Duration::from_millis(millis));
	loop {
		interval.tick().await;
		let stateguard = state.lock().unwrap();
		for (id, user) in &stateguard.users {
			if user.last_seen.elapsed() > 20000 {
				debug!("User {} left", user.id);
			}
		}
	}
}


#[tokio::main]
async fn main() {
	// initialize tracing
	//tracing_subscriber::fmt::init();

	env_logger::init();

	let state = Arc::new(Mutex::new(AppState {
		users: HashMap::new(),
		chats: HashMap::new(),
		next_room: None,
	}));

	let cleanupclone = state.clone();
	tokio::spawn(async move {
		cleanup(2000, cleanupclone).await;
	});

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
	response_headers.insert("Location", "/".parse().expect("weird"));
	if let Some((uid, room)) = cookie
		.get("uid")
		.and_then(|uid| stateguard.users.get(uid).map(|user| (uid, user)))
		.and_then(|(uid, user)| user.room_id.as_ref().map(|room_id| (uid, room_id)))
		.and_then(|(uid, room_id)| {
			stateguard
				.chats
				.get(room_id)
				.map(|room| (uid, room.clone()))
	}) {
		let mut roomguard = room.lock().unwrap();
		let muser = stateguard.users.get_mut(uid).unwrap();
		muser.room_id = None;
		if let Some(terminator) = &roomguard.terminator { //remaining user is leaving
			stateguard.chats.remove(&roomguard.id);
		} else {
			roomguard.terminator = Some(uid.to_string());
			roomguard.messages.push(Message::new(None, String::from("User left the room")));
		}
	}

	(StatusCode::SEE_OTHER, response_headers)
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
				debug!("New user {}", newid);
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

	match &user.room_id {
		Some(room_id) => {
			let roomguard = stateguard.chats.get(room_id).unwrap().lock().unwrap();

			if roomguard.terminator.is_some() {
				context.insert("terminated", &true);
			}
			match roomguard.users.len() {
				1 => context.insert("waiting", &true),
				2 => context.insert(
					"messages",
					&roomguard
						.messages
						.iter()
						.rev()
						.map(|msg| msg.pov(&Some(user.id.clone())))
						.collect::<Vec<MessageView>>(),
				),
				x => {
					panic!("Room can't have {} users", x)
				}
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
					if roomguard.terminator.is_some() {
						context.insert("terminated", &true);
					}
					context.insert(
						"messages",
						&roomguard
							.messages
							.iter()
							.rev()
							.map(|msg| msg.pov(&Some(user.id.clone())))
							.collect::<Vec<MessageView>>(),
					);
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
					let mut roomguard = room.lock().unwrap();
					if !roomguard.terminator.is_some() {
						roomguard.messages
						.push(Message::new(Some(uid.to_string()), value.to_string()));
					} else {
						eprintln!("cannot send to terminated room");
					}
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
		.as_millis()
		.try_into()
		.expect("500 million years?")
}
