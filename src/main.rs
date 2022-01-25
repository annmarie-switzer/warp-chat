use futures_util::{SinkExt, StreamExt, TryFutureExt};
use std::collections::HashMap;
use std::env;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::ws::{Message, WebSocket};
use warp::Filter;

static NEXT_USER_ID: AtomicUsize = AtomicUsize::new(1);

struct UserInfo {
    room: String,
    tx: mpsc::UnboundedSender<Message>,
}

type Users = Arc<RwLock<HashMap<usize, UserInfo>>>;

#[tokio::main]
async fn main() {
    if env::var_os("RUST_LOG").is_none() {
        env::set_var("RUST_LOG", "info");
    };

    pretty_env_logger::init();

    let users = Users::default();

    let users = warp::any().map(move || users.clone());

    // Instantiates a websocket connection.
    // Called from the browser when chat.html is loaded.
    let websocket = warp::path!("websocket" / String)
        .and(users)
        .and(warp::ws())
        .map(|room_id: String, users, ws: warp::ws::Ws| {
            ws.on_upgrade(move |socket| user_connected(socket, room_id.clone(), users))
        });

    // GET `/chat/:room_id`
    let chat = warp::path!("chat" / String)
        .map(|_| ())
        .untuple_one()
        .and(warp::fs::file("client/chat.html"));

    // GET `/`
    let index = warp::path::end().and(warp::fs::file("client/index.html"));

    let api = index.or(chat).or(websocket);

    warp::serve(api).run(([127, 0, 0, 1], 3030)).await;
}

async fn user_connected(ws: WebSocket, room_id: String, users: Users) {
    // "An unbounded channel handles buffering and flushing of messages to the websocket."
    let (tx, rx) = mpsc::unbounded_channel();
    let mut rx = UnboundedReceiverStream::new(rx);

    let user_id = NEXT_USER_ID.fetch_add(1, Ordering::Relaxed);

    let user_info = UserInfo {
        room: room_id.clone(),
        tx,
    };

    users.write().await.insert(user_id, user_info);

    eprintln!("New chat user: {user_id} added to room: {room_id}");

    let (mut ws_sender, mut ws_receiver) = ws.split();

    tokio::task::spawn(async move {
        while let Some(message) = rx.next().await {
            ws_sender
                .send(message)
                .unwrap_or_else(|e| {
                    eprintln!("websocket send error: {e}");
                })
                .await;
        }
    });

    while let Some(result) = ws_receiver.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("websocket error(uid={user_id}): {e}");
                break;
            }
        };

        user_message(user_id, msg, &users, &room_id).await;
    }

    user_disconnected(user_id, &users).await;
}

async fn user_message(user_id: usize, msg: Message, users: &Users, room_id: &str) {
    let msg = if let Ok(s) = msg.to_str() {
        s
    } else {
        return;
    };

    let new_msg = format!("<User#{user_id}>: {msg}");

    for (&id, user_info) in users.read().await.iter() {
        if id != user_id && user_info.room == room_id.to_string() {
            if let Err(_disconnected) = user_info.tx.send(Message::text(new_msg.clone())) {
                // The tx is disconnected, our `user_disconnected` code
                // should be happening in another task, nothing more to
                // do here.
            }
        }
    }
}

async fn user_disconnected(user_id: usize, users: &Users) {
    eprintln!("good bye user: {user_id}");
    users.write().await.remove(&user_id);
}
