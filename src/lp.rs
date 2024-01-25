use anyhow::Context as _;
use futures_util::stream::{StreamExt, TryStreamExt};
use itertools::Itertools;
use once_cell::sync::Lazy;
use regex::Regex;
use rspotify::clients::BaseClient;
use rspotify::model::{FullEpisode, FullTrack, PlayableItem, PlaylistItem};
use serenity::builder::CreateEmbed;
use serenity::model::prelude::CommandInteraction;
use serenity::model::prelude::{ChannelId, Message};
use serenity::prelude::RwLock;
use serenity::{async_trait, prelude::Context};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;
use std::collections::HashMap;
use std::sync::Arc;

use serenity_command_handler::modules::polls; // serenity-command-handler, for hooking

use serenity_command_handler::{
    CommandStore, CompletionStore, Handler, HandlerBuilder, Module, ModuleMap,
};

#[derive(Debug)]
pub struct TrackInfo {
    pub number: usize,
    pub name: String,
    pub uri: Option<String>,
    pub duration: chrono::Duration,
}

#[derive(Debug)]
enum PlaylistInfo {
    AlbumInfo {
        id: String,
        artist: String,
        name: String,
        uri: Option<String>,
    },
    PlaylistInfo {
        id: String,
        name: String,
        uri: Option<String>,
    },
}

#[derive(Debug)]
pub struct LPInfo {
    playlist: PlaylistInfo,
    tracks: Vec<TrackInfo>,
    started: Option<chrono::DateTime<chrono::Utc>>,
}

impl LPInfo {
    async fn from_spotify_album_id<C: BaseClient>(
        client: &C,
        album_id_str: &str,
    ) -> anyhow::Result<Self> {
        let album_id = rspotify::model::AlbumId::from_id(album_id_str)
            .context("trying to parse album ID")?;

        let album = client
            .album(album_id.clone(), None)
            .await
            .context("fetching album")?;
        let artists = album
            .artists
            .iter()
            .map(|a| a.name.as_ref())
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!("Album pinged: {} - {} ", &artists, &album.name); // Debug
        let tracks = client
            .album_track(album_id, None)
            .enumerate()
            .map(|(count, mb)| mb.map(|x| (count, x)))
            .map_ok(|(count, track)| TrackInfo {
                number: count,
                name: track.name.to_string(),
                duration: track.duration.clone(),
                uri: track.external_urls.get("spotify").map(|s| s.to_owned()),
            })
            .try_collect::<Vec<TrackInfo>>()
            .await?;

        Ok(LPInfo {
            playlist: PlaylistInfo::AlbumInfo {
                id: album.id.to_string(),
                artist: artists.clone(),
                name: album.name.to_string(),
                uri: album.external_urls.get("spotify").map(|s| s.to_owned()),
            },
            tracks,
            started: None,
        })
    }
    async fn from_spotify_playlist_id<C: BaseClient>(
        client: &C,
        album_id_str: &str,
    ) -> anyhow::Result<Self> {
        let playlist_id = rspotify::model::PlaylistId::from_id(album_id_str)
            .context("trying to parse playlist ID")?;

        let playlist = client
            .playlist(playlist_id.clone(), None, None)
            .await
            .context("fetching playlist")?;

        eprintln!("Playlist pinged: {}", &playlist.name); // Debug
        let items = client
            .playlist_items(playlist_id, None, None)
            .try_collect::<Vec<PlaylistItem>>()
            .await?;
        let tracks = items
            .iter()
            .enumerate()
            .filter_map(|(count, item)| {
                item.track.as_ref().map(|track| match track {
                    PlayableItem::Track(FullTrack {
                        name,
                        duration,
                        external_urls,
                        ..
                    })
                    | PlayableItem::Episode(FullEpisode {
                        name,
                        duration,
                        external_urls,
                        ..
                    }) => TrackInfo {
                        number: count + 1,
                        name: name.to_string(),
                        duration: duration.clone(),
                        uri: external_urls.get("spotify").map(|s| s.to_owned()),
                    },
                })
            })
            .collect::<Vec<_>>();

        Ok(LPInfo {
            playlist: PlaylistInfo::PlaylistInfo {
                id: playlist.id.to_string(),
                name: playlist.name.to_string(),
                uri: playlist
                    .external_urls
                    .get("spotify")
                    .map(|s| s.to_owned()),
            },
            tracks,
            started: None,
        })
    }
}

enum PlayState<'a> {
    NotStarted,
    Finished(chrono::Duration), // how long ago
    Playing {
        track: &'a TrackInfo,
        position: chrono::Duration,
    },
}

// Turn a string into a link if an URI is available.
fn maybe_uri<S: AsRef<str>, T: AsRef<str>>(
    text: T,
    mb_uri: Option<S>,
) -> String {
    match mb_uri.as_ref() {
        None => text.as_ref().to_string(),
        Some(uri) => format!("[{}]({})", text.as_ref(), uri.as_ref()),
    }
}

impl LPInfo {
    fn now_playing(&self) -> PlayState {
        let started = match self.started {
            None => {
                return PlayState::NotStarted;
            }
            Some(started) => started,
        };
        let now = chrono::offset::Utc::now();
        if started > now {
            eprintln!(
                "LPInfo: Started timestamp in the future! started={} > now={}",
                started, now
            );
            return PlayState::NotStarted;
        }
        let mut remain = now - started;
        for track in self.tracks.iter() {
            if track.duration > remain {
                return PlayState::Playing {
                    track: &track,
                    position: remain,
                };
            } else {
                remain = remain - track.duration;
            }
        }
        // We passed all the tracks
        // remain = now - started - sum(track duration)
        // How long ago the playlist finished
        PlayState::Finished(remain)
    }

    fn build_embed(&self) -> Box<CreateEmbed> {
        let (lp_name, lp_id) = match &self.playlist {
            PlaylistInfo::AlbumInfo {
                id,
                artist,
                name,
                uri,
            } => {
                let album_name =
                    maybe_uri(format!("{artist} - {name}"), uri.as_ref());
                (format!("**Album**:\n {album_name}"), id.clone())
            }
            PlaylistInfo::PlaylistInfo { id, name, uri } => {
                let playlist_name = maybe_uri(name, uri.as_ref());
                (format!("**Playlist**:\n {playlist_name}"), id.clone())
            }
        };

        let mut embed = CreateEmbed::new().description(format!(
            "{} - \\[{}\\]",
            lp_name,
            display_duration(&self.tracks.iter().map(|t| t.duration).sum())
        ));
        match self.now_playing() {
            PlayState::NotStarted => {
                embed = embed.title("Listening Party has not started yet.");
            }
            PlayState::Finished(_) => {
                embed = embed.title("Listening Party has finished.");
            }
            PlayState::Playing { track, position } => {
                let track_uri_ctx = track
                    .uri
                    .as_ref()
                    .map(|uri| format!("{}?context={}", uri, &lp_id));
                embed = embed
                    .title("Listening Party in full swing! Join in!")
                    .field(
                        "Now playing",
                        format!(
                            "Track {} - {} - {} / {}",
                            track.number,
                            maybe_uri(&track.name, track_uri_ctx.as_ref()),
                            display_duration(&position),
                            display_duration(&track.duration)
                        ),
                        true,
                    );
            }
        }

        Box::new(embed)
    }
}

// Format Duration as hh:mm:ss
fn display_duration(duration: &chrono::Duration) -> String {
    let allsecs = duration.num_seconds();
    let seconds = allsecs % 60;
    let minutes = allsecs / 60 % 60;
    let hours = allsecs / 3600;
    if hours > 0 {
        format!("{}:{:0>2}:{:0>2}", hours, minutes, seconds)
    } else {
        format!("{:0>2}:{:0>2}", minutes, seconds)
    }
}

// Regex to identity spotify album URIs and extract album id
static SPOTIFY_ALBUM_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        "\\bhttps://open.spotify.com(?:/intl-[a-z]+)?\
            /album/([a-zA-Z0-9]+)(?:\\?[a-zA-Z?=&]*)?\\b",
    )
    .unwrap()
});

// Find spotify playlist URI and extract the album ID
fn match_spotify_album(string: &str) -> Option<&str> {
    SPOTIFY_ALBUM_RE
        .captures(string.as_ref())
        .map(|caps| caps.get(1).unwrap().as_str())
}

// Regex to identity spotify playlist URIs and extract album id
const SPOTIFY_PLAYLIST_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        "\\bhttps://open.spotify.com(?:/intl-[a-z]+)?\
           /playlist/([a-zA-Z0-9]+)(?:\\?[a-zA-Z?=&]*)?\\b",
    )
    .unwrap()
});

// Find spotify playlist URI and extract the album ID
fn match_spotify_playlist(string: &str) -> Option<&str> {
    SPOTIFY_PLAYLIST_RE
        .captures(string.as_ref())
        .map(|caps| caps.get(1).unwrap().as_str())
}

#[derive(Command, Debug)]
#[cmd(name = "lp", desc = "Check if listening party is going")]
pub struct CurrentLP {}

#[async_trait]
impl BotCommand for CurrentLP {
    type Data = Handler;
    async fn run(
        self,
        data: &Handler,
        _ctx: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let channel = interaction.channel_id;
        let lpmod = data.module::<LP>().unwrap();
        let lps = lpmod.last_pinged.read().await;
        let lp = lps.get(&channel);
        let response = match lp {
            None => CommandResponse::Public(
                "There is no listening party at the moment.".to_string(),
            ),
            Some(lpinfo) => CommandResponse::Embed(lpinfo.build_embed()),
        };
        Ok(response)
    }
}

pub type PingedMap = Arc<RwLock<HashMap<ChannelId, LPInfo>>>;

pub struct LP {
    last_pinged: PingedMap,
}

impl Clone for LP {
    fn clone(&self) -> Self {
        LP {
            last_pinged: self.last_pinged.clone(),
        }
    }
}

#[async_trait]
impl polls::ModPollReadyHandler for LP {
    async fn ready(&self, channelid: &ChannelId) {
        self.start_lp(channelid).await;
    }
}

// Roles used for pinging listening parties
const LP_ROLES: &'static [u64] = &[
    1198354637137391709, // `@Listening Party`` in test guild
                         // TODO: Make this configurable? Fetch via name?
];

impl LP {
    pub fn new() -> Self {
        LP {
            last_pinged: Default::default(),
        }
    }

    // Handle messages to remember the last pinged album
    pub async fn handle_message<C: BaseClient>(
        &self,
        client: &C,
        msg: &Message,
    ) {
        let msg_txt: &str = &msg.content;

        // Check if the specified roles were mentioned
        if msg
            .mention_roles
            .iter()
            .any(|&role| LP_ROLES.iter().contains(&role.get()))
        {
            let mb_pl = 'regex: {
                // Check for spotify playlist URL
                if let Some(aid) = match_spotify_album(&msg_txt) {
                    break 'regex (LPInfo::from_spotify_album_id(client, aid)
                        .await);
                }
                // Check for spotify playlist URL
                if let Some(pid) = match_spotify_playlist(&msg_txt) {
                    break 'regex LPInfo::from_spotify_playlist_id(client, pid)
                        .await;
                }
                // No regexes match
                return;
            };
            let pl = match mb_pl {
                Err(e) => {
                    eprintln!("Error resolving ping: {}", e);
                    return;
                }
                Ok(pl) => pl,
            };

            let mut channels = self.last_pinged.write().await;

            (*channels).insert(msg.channel_id, pl);
            eprintln!("Found pinged LP!");
            ()
        };
    }

    pub async fn start_lp(&self, channel: &ChannelId) {
        let now = chrono::offset::Utc::now();
        let mut channels = self.last_pinged.write().await;
        channels
            .entry(*channel)
            .and_modify(|lp_info| lp_info.started = Some(now));
        ()
    }
}

#[async_trait]
impl Module for LP {
    async fn add_dependencies(
        builder: HandlerBuilder,
    ) -> anyhow::Result<HandlerBuilder> {
        Ok(builder)
    }
    fn register_commands(
        &self,
        store: &mut CommandStore,
        _completions: &mut CompletionStore,
    ) {
        eprintln!("Created LP module");
        store.register::<CurrentLP>();
    }

    async fn init(_m: &ModuleMap) -> anyhow::Result<Self> {
        Ok(LP {
            last_pinged: Default::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_spotify_album_parses() {
        let aid = "4ddRx20FxcGU2ZJhateVym";

        let uris: &'static [&'static str] = &[
            "https://open.spotify.com/album/4ddRx20FxcGU2ZJhateVym", // regular
            "https://open.spotify.com/album/4ddRx20FxcGU2ZJhateVym\
                ?si=RQNX_vP_SN6Ct4haVZeHDA", // ?si=
            "https://open.spotify.com/intl-de/album\
             /4ddRx20FxcGU2ZJhateVym", // intl
            "https://open.spotify.com/intl-de/album\
             /4ddRx20FxcGU2ZJhateVym?si=RQNX_vP_SN6Ct4haVZeHDA", // intl + ?si=
        ];

        for uri in uris {
            assert_eq!(match_spotify_album(uri), Some(aid));
        }
    }

    #[test]
    fn match_spotify_playlist_parses() {
        let pid = "5Yy6oc82tIR8k25BdHcsdq";

        let uris: &'static [&'static str] = &[
            //regular
            "https://open.spotify.com/playlist/5Yy6oc82tIR8k25BdHcsdq",
            // ?si=
            "https://open.spotify.com/playlist/5Yy6oc82tIR8k25BdHcsdq\
                 ?si=574a09e801af4003",
            // intl
            "https://open.spotify.com/intl-de/playlist/5Yy6oc82tIR8k25BdHcsdq",
            // intl + ?si=
            "https://open.spotify.com/intl-de/playlist/5Yy6oc82tIR8k25BdHcsdq\
                 ?si=574a09e801af4003",
        ];

        for uri in uris {
            assert_eq!(match_spotify_playlist(uri), Some(pid));
        }
    }
}
