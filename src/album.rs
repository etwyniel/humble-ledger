use std::sync::Arc;

use serenity::async_trait;

#[derive(Debug, Default)]
pub struct Album {
    pub name: String,
    pub artist: String,
}

#[async_trait]
pub trait AlbumProvider: Send + Sync {
    fn url_matches(&self, _url: &str) -> bool;

    fn id(&self) -> &'static str;

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album>;

    async fn query_album(&self, _q: &str) -> anyhow::Result<Album>;
}

impl Album {
    pub fn format_name(&self) -> String {
        if self.artist.is_empty() {
            self.name.to_string()
        } else {
            format!("{} - {}", self.artist, self.name)
        }
    }
}

#[async_trait]
impl<P: AlbumProvider + Send> AlbumProvider for Arc<P> {
    fn url_matches(&self, url: &str) -> bool {
        self.as_ref().url_matches(url)
    }

    fn id(&self) -> &'static str {
        self.as_ref().id()
    }

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album> {
        self.as_ref().get_from_url(url).await
    }

    async fn query_album(&self, q: &str) -> anyhow::Result<Album> {
        self.as_ref().query_album(q).await
    }
}
