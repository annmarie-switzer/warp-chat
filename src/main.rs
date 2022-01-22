use futures_util::{SinkExt, StreamExt, TryFutureExt};
use std::env;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::ws::{Message, WebSocket};
use warp::Filter;

struct User {
    id: usize,
    room: usize,
    tx: mpsc::UnboundedSender<Message>,
}

static NEXT_USER_ID: AtomicUsize = AtomicUsize::new(1);

type Users = Arc<RwLock<Vec<User>>>;

#[tokio::main]
async fn main() {
    if env::var_os("RUST_LOG").is_none() {
        env::set_var("RUST_LOG", "info");
    };

    pretty_env_logger::init();

    let users = Users::default();

    // custom filter
    let users = warp::any().map(move || users.clone());

    // GET `/chat/:room_id`
    // Called from the browser when the page loads
    let chat = warp::path("chat")
        .and(warp::ws())
        .and(warp::path::param::<usize>())
        .and(users)
        .map(|ws: warp::ws::Ws, room_id, users| {
            ws.on_upgrade(move |socket| user_connected(socket, room_id, users))
        });

    // GET `/:room_id`
    let index = warp::path!(usize).map(|_| warp::reply::html(INDEX_HTML));

    let routes = index.or(chat);

    warp::serve(routes).run(([127, 0, 0, 1], 3030)).await;
}

async fn user_connected(ws: WebSocket, room_id: usize, users: Users) {
    let user_id = NEXT_USER_ID.fetch_add(1, Ordering::Relaxed);

    eprintln!("New chat user: {user_id} added to room: {room_id}");

    let (mut user_ws_tx, mut user_ws_rx) = ws.split();

    let (tx, rx) = mpsc::unbounded_channel();
    let mut rx = UnboundedReceiverStream::new(rx);

    tokio::task::spawn(async move {
        while let Some(message) = rx.next().await {
            user_ws_tx
                .send(message)
                .unwrap_or_else(|e| {
                    eprintln!("websocket send error: {e}");
                })
                .await;
        }
    });

    let new_user = User {
        id: user_id,
        room: room_id,
        tx,
    };

    users.write().await.push(new_user);

    // Every time the user sends a message,
    // broadcast it to all other users in the same room.
    while let Some(result) = user_ws_rx.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("websocket error(uid={user_id}): {e}");
                break;
            }
        };

        user_message(user_id, msg, &users, room_id).await;
    }

    user_disconnected(user_id, &users).await;
}

async fn user_disconnected(user_id: usize, users: &Users) {
    eprintln!("good bye user: {user_id}");
    users.write().await.retain(|u| u.id != user_id);
}

async fn user_message(user_id: usize, msg: Message, users: &Users, room_id: usize) {
    let msg = if let Ok(s) = msg.to_str() {
        s
    } else {
        return;
    };

    let new_msg = format!("<User#{user_id}>: {msg}");

    // New message from this user, send it to everyone else (except same uid)...
    for user in users.read().await.iter() {
        if user.id != user_id && user.room == room_id {
            if let Err(_disconnected) = user.tx.send(Message::text(new_msg.clone())) {
                // The tx is disconnected, our `user_disconnected` code
                // should be happening in another task, nothing more to
                // do here.
            }
        }
    }
}

static INDEX_HTML: &str = r#"
<!DOCTYPE html>
<html lang="en">
    <head>
        <title>Warp Chat</title>
        <link rel="icon" href="data:,">
    </head>
    <body>
        <h1>Warp chat</h1>
        
        <div id="chat">
            <p><em>Connecting...</em></p>
        </div>
        
        <input type="text" id="text" />
        
        <button type="button" id="send">Send</button>
        
        <script type="text/javascript">
            const chat = document.getElementById('chat');
            const text = document.getElementById('text');
            const uri = `ws://${location.host}/chat${location.pathname}`;
            const ws = new WebSocket(uri);
            
            function message(data) {
                const line = document.createElement('p');
                line.innerText = data;
                chat.appendChild(line);
            }
            
            ws.onopen = function() {
                chat.innerHTML = '<p><em>Connected!</em></p>';
            };
            
            ws.onmessage = function(msg) {
                message(msg.data);
            };
            
            ws.onclose = function() {
                chat.getElementsByTagName('em')[0].innerText = 'Disconnected!';
            };
            
            send.onclick = function() {
                const msg = text.value;
                ws.send(msg);
                text.value = '';
                message('<You>: ' + msg);
            };
        </script>
    </body>
</html>
"#;
