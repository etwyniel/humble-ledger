use std::collections::HashMap;

use rspotify::model::TrackId;
use serenity::{model::prelude::{UserId, Presence, ActivityType}, async_trait, prelude::RwLock};
use serenity_command_handler::{Module, ModuleMap};


pub struct NowPlaying {
    pub track_id: TrackId<'static>,
    pub end: u64,
}

pub struct SpotifyActivity {
    user_activities: RwLock<HashMap<UserId, NowPlaying>>,
}

fn get_now_playing(presence: &Presence) -> Option<NowPlaying> {
    let act = presence.activities.iter().find(|act| act.kind == ActivityType::Listening && act.name == "Spotify")?;
    let track_id = TrackId::from_id(act.sync_id.as_deref()?).ok()?.into_static();
    let end = act.timestamps.as_ref()?.end?;
    Some(NowPlaying { track_id, end })
}

impl SpotifyActivity {
    pub async fn presence_update(&self, presence: &Presence) {
        if let Some(np) = get_now_playing(presence) {
            self.user_activities.write().await.insert(presence.user.id, np);
        } else {
            self.user_activities.write().await.remove(&presence.user.id);
        }
    }

    pub async fn user_now_playing(&self, user_id: UserId) -> Option<TrackId<'static>> {
        self.user_activities.read().await.get(&user_id).map(|np| np.track_id.clone_static())
    }
}

#[async_trait]
impl Module for SpotifyActivity {
    async fn init(_: &ModuleMap) ->  anyhow::Result<Self>{
        Ok(SpotifyActivity { user_activities: Default::default() })
    }
}
