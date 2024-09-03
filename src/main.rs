use axum::{
	extract::State,
	http::{header::SET_COOKIE, StatusCode},
	response::IntoResponse,
	routing::{get, post},
	Router,
};

use clap::Parser;

use env_logger;

use log::{debug, info, trace};

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

/// Chat application configuration parameters
#[derive(Parser, Clone, Debug)]
#[command(author = "Your Name <your.email@example.com>", version = "1.0", about = "Chat application with configurable parameters")]
struct Args {
	/// Cleanup poll frequency in milliseconds (default: 5000)
	#[arg(long, default_value_t = 5000)]
	cleanup_poll_frequency: u64,

	/// Max number of messages in chat room (default: 100)
	#[arg(long, default_value_t = 100)]
	max_messages: usize,

	/// Max idle time outside of chat in seconds (default: 20)
	#[arg(long, default_value_t = 20)]
	max_idle_outside: u64,

	/// Max idle time inside chat in seconds (default: 300)
	#[arg(long, default_value_t = 300)]
	max_idle_inside: u64,
}

#[derive(Clone, Debug)]
struct Message {
	sender: Option<UserId>,
	seen: bool,
	time: u64,
	msg: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
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
	seen: bool,
}

impl Message {
	fn new(from: Option<UserId>, msg: String) -> Self {
		Self {
			seen: false,
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
			seen: self.seen, //true if seen by the other user (not self.sender)
		}
	}

	fn pov_mut(&mut self, userid: &Option<UserId>) -> MessageView {
		let view = self.pov(userid);
		if view.senderkind == SenderKind::THEM {
			self.seen = true;
		}
		view
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
				seen: false,
				sender: None,
				time: timenow,
				msg: String::from("Chat initiated"),
			}],
			created: timenow,
		}
	}

	fn terminate(&mut self, user: UserId) {
		self.terminator = Some(user);
		self
			.messages
			.push(Message::new(None, String::from("User left the room")));
	}

	fn dump(&self) -> String {
		self.messages
			.iter()
			.map(|msg| {
				format!(
					"{}|{}|{}",
					msg.time,
					msg.sender.clone().unwrap_or("system".to_string()),
					msg.msg
				)
			})
			.collect::<Vec<String>>()
			.join("\n")
	}
}

#[derive(Debug)]
struct AppConfigState {
	state: AppState,
	config: Args,
}

impl Clone for AppConfigState {
	fn clone(&self) -> Self {
		Self {
			state: &self.state.clone(),
			config: &self.config.clone(),
		}
	}
}

#[derive(Clone, Debug)]
struct AppState {
	users: HashMap<UserId, UserState>,
	chats: HashMap<ChatId, Arc<Mutex<ChatRoom>>>,
	next_room: Option<Arc<Mutex<ChatRoom>>>,
}

impl AppState {

	fn cleanup(&mut self) {
		let users_to_remove: Vec<(UserState, u64)> = self
			.users
			.iter()
			.map(|(_, v)| (v.clone(), v.last_seen.elapsed()))
			.filter(|(_, t)| *t > 20000)
			.collect();
		for (user, t) in users_to_remove {
			match user.room_id {
				Some(id) => {
					let room = self
						.chats
						.get(&id)
						.expect("Couldn't find room user points to")
						.clone(); //clones the arc mutex reference
					let mut roomguard = room.lock().unwrap();
					match &roomguard.terminator {
						Some(terminator) => if *terminator != user.id {
							//other user left, and now this one too
							assert!(self.chats.remove(&id).is_some()); //so we remove the room
							assert!(self.users.remove(&user.id).is_some()); //and the this user
							debug!("Removed user {}. Age: {}, rooms: {}", user.id, user.first_seen.elapsed(), user.chat_ctr);
							debug!("Removed room {}. Age: {}", id, roomguard.created.elapsed());
						}
						None => match roomguard.users.len() { //User is in non terminated room
							1 => { //Room conversation is not initiated, looking for other user
								assert!(self.chats.remove(&id).is_some());
								assert!(self.users.remove(&user.id).is_some());
								assert!(Arc::ptr_eq(&room, &self.next_room.clone().expect("shouldnt be none")));
								self.next_room = None;
								debug!("Removed user {}. Age: {}, rooms: {}", user.id, user.first_seen.elapsed(), user.chat_ctr);
								debug!("Removed room {}. Age: {}", id, roomguard.created.elapsed());
							},
							2 => if t > 300000 { //Room conversation is initiated and running
								roomguard.terminate(user.id.clone());
								assert!(self.users.remove(&user.id).is_some());
								debug!("Removed user {}. Age: {}, rooms: {}", user.id, user.first_seen.elapsed(), user.chat_ctr);
							}
							x => eprintln!("weird, room has {} users", x),
						}
					}
				}
				None => {
					assert!(self.users.remove(&user.id).is_some()); //There may exist rooms which they have already exit
					debug!("Removed user {}. Age: {}", user.id, user.first_seen.elapsed());
				}
			}
		}
	}
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

#[tokio::main]
async fn main() {
	env_logger::init();

	let args = Args::parse();

	let configstate = AppConfigState {
		state: Arc::new(Mutex::new(AppState {
			users: HashMap::new(),
			chats: HashMap::new(),
			next_room: None,
		})),
		config: args,
	};

	let millis = 2000;
	let cleanupclone = configstate.clone();
	tokio::spawn(async move {
		let mut interval = time::interval(Duration::from_millis(millis));
		loop {
			interval.tick().await;
			let mut stateguard = cleanupclone.lock().unwrap();
			stateguard.cleanup();
		}
	});

	// build our application with a route
	let app = Router::new()
		.route("/", get(get_index))
		.route("/messages", get(read_messages))
		.route("/dump", get(dump_states))
		.route("/exit", get(exit_room))
		.route("/", post(send_message))
		.with_state(configstate);

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
		if roomguard.terminator.is_some() {
			//remaining user is leaving
			assert!(stateguard.chats.remove(&roomguard.id).is_some());
		} else {
			roomguard.terminate(String::from(uid));
		}
	}

	(StatusCode::SEE_OTHER, response_headers)
}

async fn get_index(
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
	context.insert("user_ctr", &stateguard.users.len());

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
			if roomguard.users.len() == 1{
				context.insert("waiting", &true);
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

	let response_html = tera.render("index", &context).unwrap();
	(response_headers, response_html)
}

async fn read_messages(
	State(state): State<Arc<Mutex<AppState>>>,
	TypedHeader(cookie): TypedHeader<Cookie>,
) -> impl IntoResponse {
	let mut stateguard = state.lock().unwrap();
	let mut response_headers = HeaderMap::new();

	let user = cookie
		.get("uid")
		.and_then(|uid| stateguard.users.get_mut(uid))
		.map(|usr| {
			usr.last_seen = get_timestamp();
			usr.clone()
		});

	let response_html = match user.and_then(|usr| {
		usr.room_id
		.and_then(|roomid| stateguard.chats.get(&roomid))
		.map(|roomref| {
			let mut roomguard = roomref.lock().unwrap();
			roomguard
				.messages
				.iter_mut()
				.rev()
				.map(|msg| msg.pov_mut(&Some(usr.id.clone())))
				.collect::<Vec<MessageView>>()
		})
	}) {
		Some(messages) => {
			let template = include_str!("template/messages.tera");
			let mut tera = Tera::default();
			tera.add_raw_template("messages", template).unwrap();
			let mut context = Context::new();
			response_headers.insert("Content-Type", "text/html".parse().expect("weird"));
			context.insert("messages", &messages);
			tera.render("messages", &context).unwrap()
		},
		None => String::new()
	};

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
						//TODO delete old messages to keep max len 1000
						roomguard
							.messages
							.push(Message::new(Some(uid.to_string()), value.to_string()));
						if roomguard.messages.len() > 5 {
							roomguard.messages.remove(0);
						}
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
