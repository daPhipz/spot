use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::future::{FutureExt, TryFutureExt};
use librespot::core::spotify_id::SpotifyId;
use std::cell::RefCell;
use std::rc::Rc;
use std::thread;
use tokio_core::reactor::Core;

use crate::app::state::{LoginAction, PlaybackAction};
use crate::app::{credentials, AppAction};

mod player;
pub use player::*;

#[derive(Debug, Clone)]
pub enum Command {
    Login(String, String),
    Logout,
    PlayerLoad(SpotifyId),
    PlayerResume,
    PlayerPause,
    PlayerStop,
    PlayerSeek(u32),
    RefreshToken,
}

struct AppPlayerDelegate {
    sender: RefCell<UnboundedSender<AppAction>>,
}

impl AppPlayerDelegate {
    fn new(sender: UnboundedSender<AppAction>) -> Self {
        let sender = RefCell::new(sender);
        Self { sender }
    }
}

impl SpotifyPlayerDelegate for AppPlayerDelegate {
    fn end_of_track_reached(&self) {
        self.sender
            .borrow_mut()
            .unbounded_send(PlaybackAction::Next.into())
            .unwrap();
    }

    fn login_successful(&self, credentials: credentials::Credentials) {
        self.sender
            .borrow_mut()
            .unbounded_send(LoginAction::SetLoginSuccess(credentials).into())
            .unwrap();
    }

    fn refresh_successful(&self, token: String) {
        self.sender
            .borrow_mut()
            .unbounded_send(LoginAction::SetRefreshedToken(token).into())
            .unwrap();
    }

    fn report_error(&self, error: SpotifyError) {
        self.sender
            .borrow_mut()
            .unbounded_send(AppAction::ShowNotification(format!("{}", error)))
            .unwrap();
    }

    fn notify_playback_state(&self, position: u32) {
        self.sender
            .borrow_mut()
            .unbounded_send(PlaybackAction::SyncSeek(position).into())
            .unwrap();
    }
}

pub fn start_player_service(
    player_settings: SpotifyPlayerSettings,
    appaction_sender: UnboundedSender<AppAction>,
) -> UnboundedSender<Command> {
    let (sender, receiver) = unbounded::<Command>();
    thread::spawn(move || {
        let mut core = Core::new().unwrap();
        let delegate = Rc::new(AppPlayerDelegate::new(appaction_sender.clone()));
        core.run(
            SpotifyPlayer::new(player_settings, delegate)
                .start(core.handle(), receiver)
                .boxed_local()
                .compat(),
        )
        .unwrap()
    });
    sender
}
