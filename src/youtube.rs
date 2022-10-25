use std::borrow::Cow;

use anyhow::{anyhow, Context};
use google_youtube3::api;
use hyper::client::HttpConnector;
use hyper_tls::HttpsConnector;
use reqwest::Url;
use serenity::async_trait;

use crate::album::{Album, AlbumProvider};

pub struct Youtube {
    client: api::YouTube<HttpsConnector<HttpConnector>>,
}

impl Youtube {
    pub fn new(
        client: &hyper::Client<hyper_tls::HttpsConnector<HttpConnector>>,
        authenticator: &yup_oauth2::authenticator::Authenticator<
            hyper_tls::HttpsConnector<HttpConnector>,
        >,
    ) -> Self {
        let client = api::YouTube::new(client.clone(), authenticator.clone());
        Youtube { client }
    }
}

#[async_trait]
impl AlbumProvider for Youtube {
    fn url_matches(&self, url: &str) -> bool {
        url.contains("youtube.com") || url.contains("youtu.be")
    }

    fn id(&self) -> &'static str {
        "youtube"
    }

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album> {
        let url: Url = url.parse().context("Invalid URL")?;
        let id = url
            .query_pairs()
            .find_map(|(key, value)| if key == "v" { Some(value) } else { None })
            .or_else(|| {
                url.path_segments()
                    .and_then(|path| path.last())
                    .map(Cow::Borrowed)
            })
            .ok_or_else(|| anyhow!("Invalid youtube url"))?;
        let title = self
            .client
            .videos()
            .list(&vec!["snippet".to_string()])
            .add_id(id.as_ref())
            .doit()
            .await?
            .1
            .items
            .and_then(|videos| videos.into_iter().next())
            .and_then(|video| video.snippet)
            .and_then(|snippet| snippet.title)
            .ok_or_else(|| anyhow!("Could not find video title"))?;
        Ok(Album {
            name: title,
            artist: String::new(),
        })
    }

    async fn query_album(&self, _q: &str) -> anyhow::Result<crate::album::Album> {
        todo!()
    }
}
