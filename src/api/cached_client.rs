use futures::future::BoxFuture;
use futures::{join, FutureExt};
use regex::Regex;
use serde::de::DeserializeOwned;
use serde_json::from_slice;
use std::convert::Into;
use std::future::Future;

use super::api_models::*;
use super::cache::{CacheExpiry, CacheManager, CachePolicy, FetchResult};
use super::client::{SpotifyApiError, SpotifyClient, SpotifyResponse, SpotifyResponseKind};
use crate::app::models::*;

lazy_static! {
    pub static ref ME_ALBUMS_CACHE: Regex =
        Regex::new(r"^me_albums_\w+_\w+\.json\.expiry$").unwrap();
    pub static ref USER_CACHE: Regex =
        Regex::new(r"^me_(albums|playlists)_\w+_\w+\.json(\.expiry)?$").unwrap();
    pub static ref ALL_CACHE: Regex =
        Regex::new(r"^(me_albums_|me_playlists_|album_|playlist_|artist_)\w+\.json(\.expiry)?$")
            .unwrap();
}

pub type SpotifyResult<T> = Result<T, SpotifyApiError>;

pub trait SpotifyApiClient {
    fn get_artist(&self, id: &str) -> BoxFuture<SpotifyResult<ArtistDescription>>;

    fn get_album(&self, id: &str) -> BoxFuture<SpotifyResult<AlbumDescription>>;

    fn get_playlist(&self, id: &str) -> BoxFuture<SpotifyResult<PlaylistDescription>>;

    fn get_saved_albums(
        &self,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<SpotifyResult<Vec<AlbumDescription>>>;

    fn save_album(&self, id: &str) -> BoxFuture<SpotifyResult<AlbumDescription>>;

    fn remove_saved_album(&self, id: &str) -> BoxFuture<SpotifyResult<()>>;

    fn get_saved_playlists(
        &self,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<SpotifyResult<Vec<PlaylistDescription>>>;

    fn search(
        &self,
        query: &str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<SpotifyResult<SearchResults>>;

    fn get_artist_albums(
        &self,
        id: &str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<SpotifyResult<Vec<AlbumDescription>>>;

    fn update_token(&self, token: String);
}

enum SpotCacheKey<'a> {
    SavedAlbums(u32, u32),
    SavedPlaylists(u32, u32),
    Album(&'a str),
    AlbumLiked(&'a str),
    Playlist(&'a str),
    PlaylistTracks(&'a str, u32, u32),
    ArtistAlbums(&'a str, u32, u32),
    Artist(&'a str),
    ArtistTopTracks(&'a str),
}

impl<'a> SpotCacheKey<'a> {
    fn into_raw(self) -> String {
        match self {
            Self::SavedAlbums(offset, limit) => format!("me_albums_{}_{}.json", offset, limit),
            Self::SavedPlaylists(offset, limit) => {
                format!("me_playlists_{}_{}.json", offset, limit)
            }
            Self::Album(id) => format!("album_{}.json", id),
            Self::AlbumLiked(id) => format!("album_liked_{}.json", id),
            Self::Playlist(id) => format!("playlist_{}.json", id),
            Self::PlaylistTracks(id, offset, limit) => {
                format!("playlist_item_{}_{}_{}.json", id, offset, limit)
            }
            Self::ArtistAlbums(id, offset, limit) => {
                format!("artist_albums_{}_{}_{}.json", id, offset, limit)
            }
            Self::Artist(id) => format!("artist_{}.json", id),
            Self::ArtistTopTracks(id) => format!("artist_top_tracks_{}.json", id),
        }
    }
}

pub struct CachedSpotifyClient {
    client: SpotifyClient,
    cache: CacheManager,
}

impl CachedSpotifyClient {
    pub fn new() -> CachedSpotifyClient {
        CachedSpotifyClient {
            client: SpotifyClient::new(),
            cache: CacheManager::new(&["spot/net"]).unwrap(),
        }
    }

    fn default_cache_policy(&self) -> CachePolicy {
        if self.client.has_token() {
            CachePolicy::Default
        } else {
            CachePolicy::IgnoreExpiry
        }
    }

    async fn cache_get_or_write<T, O, F>(
        &self,
        key: SpotCacheKey<'_>,
        cache_policy: Option<CachePolicy>,
        write: F,
    ) -> SpotifyResult<T>
    where
        O: Future<Output = SpotifyResult<SpotifyResponse<T>>>,
        F: FnOnce(Option<String>) -> O,
        T: DeserializeOwned,
    {
        let raw = self
            .cache
            .get_or_write(
                &format!("spot/net/{}", key.into_raw()),
                cache_policy.unwrap_or_else(|| self.default_cache_policy()),
                move |etag| {
                    write(etag).map(|r| {
                        let SpotifyResponse {
                            kind,
                            max_age,
                            etag,
                        } = r?;
                        let expiry = CacheExpiry::expire_in_seconds(u64::max(max_age, 60), etag);
                        SpotifyResult::Ok(match kind {
                            SpotifyResponseKind::Ok(content, _) => {
                                FetchResult::Modified(content.into_bytes(), expiry)
                            }
                            SpotifyResponseKind::NotModified => FetchResult::NotModified(expiry),
                        })
                    })
                },
            )
            .await?;

        let result = from_slice::<T>(&raw);
        Ok(result?)
    }
}

impl SpotifyApiClient for CachedSpotifyClient {
    fn update_token(&self, new_token: String) {
        self.client.update_token(new_token)
    }

    fn get_saved_albums(
        &self,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<SpotifyResult<Vec<AlbumDescription>>> {
        Box::pin(async move {
            let page = self
                .cache_get_or_write(SpotCacheKey::SavedAlbums(offset, limit), None, |etag| {
                    self.client
                        .get_saved_albums(offset, limit)
                        .etag(etag)
                        .send()
                })
                .await?;

            let albums = page
                .items
                .into_iter()
                .map(|saved| saved.album.into())
                .collect::<Vec<AlbumDescription>>();

            Ok(albums)
        })
    }

    fn get_saved_playlists(
        &self,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<SpotifyResult<Vec<PlaylistDescription>>> {
        Box::pin(async move {
            let page = self
                .cache_get_or_write(SpotCacheKey::SavedPlaylists(offset, limit), None, |etag| {
                    self.client
                        .get_saved_playlists(offset, limit)
                        .etag(etag)
                        .send()
                })
                .await?;

            let albums = page
                .items
                .into_iter()
                .map(|playlist| playlist.into())
                .collect::<Vec<PlaylistDescription>>();

            Ok(albums)
        })
    }

    fn get_album(&self, id: &str) -> BoxFuture<SpotifyResult<AlbumDescription>> {
        let id = id.to_owned();

        Box::pin(async move {
            let album = self.cache_get_or_write(SpotCacheKey::Album(&id), None, |etag| {
                self.client.get_album(&id).etag(etag).send()
            });

            let liked = self.cache_get_or_write(
                SpotCacheKey::AlbumLiked(&id),
                Some(CachePolicy::AlwaysRevalidate),
                |etag| self.client.is_album_saved(&id).etag(etag).send(),
            );

            let (album, liked) = join!(album, liked);

            let mut album: AlbumDescription = album?.into();
            album.is_liked = liked?[0];

            Ok(album)
        })
    }

    fn save_album(&self, id: &str) -> BoxFuture<SpotifyResult<AlbumDescription>> {
        let id = id.to_owned();

        Box::pin(async move {
            self.cache
                .set_expired_pattern("net", &*ME_ALBUMS_CACHE)
                .await
                .unwrap_or(());
            self.client.save_album(&id).send_no_response().await?;
            self.get_album(&id[..]).await
        })
    }

    fn remove_saved_album(&self, id: &str) -> BoxFuture<SpotifyResult<()>> {
        let id = id.to_owned();

        Box::pin(async move {
            self.cache
                .set_expired_pattern("spot/net", &*ME_ALBUMS_CACHE)
                .await
                .unwrap_or(());
            self.client.remove_saved_album(&id).send_no_response().await
        })
    }

    fn get_playlist(&self, id: &str) -> BoxFuture<SpotifyResult<PlaylistDescription>> {
        let id = id.to_owned();

        Box::pin(async move {
            let playlist = self
                .cache_get_or_write(SpotCacheKey::Playlist(&id), None, |etag| {
                    self.client.get_playlist(&id).etag(etag).send()
                })
                .await?;

            let mut playlist: PlaylistDescription = playlist.into();
            let mut tracks: Vec<SongDescription> = vec![];

            let mut offset = 0u32;
            let limit = 100u32;
            loop {
                let songs = self
                    .cache_get_or_write(
                        SpotCacheKey::PlaylistTracks(&id, offset, limit),
                        None,
                        |etag| {
                            self.client
                                .get_playlist_tracks(&id, offset, limit)
                                .etag(etag)
                                .send()
                        },
                    )
                    .await?;

                let mut songs: Vec<SongDescription> = songs.into();

                let songs_loaded = songs.len() as u32;
                tracks.append(&mut songs);

                if songs_loaded < limit {
                    break;
                }

                offset += limit;
            }

            playlist.songs = tracks;
            Ok(playlist)
        })
    }

    fn get_artist_albums(
        &self,
        id: &str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<SpotifyResult<Vec<AlbumDescription>>> {
        let id = id.to_owned();

        Box::pin(async move {
            let albums = self
                .cache_get_or_write(
                    SpotCacheKey::ArtistAlbums(&id, offset, limit),
                    None,
                    |etag| {
                        self.client
                            .get_artist_albums(&id, offset, limit)
                            .etag(etag)
                            .send()
                    },
                )
                .await?;

            let albums = albums
                .items
                .into_iter()
                .map(|a| a.into())
                .collect::<Vec<AlbumDescription>>();

            Ok(albums)
        })
    }

    fn get_artist(&self, id: &str) -> BoxFuture<Result<ArtistDescription, SpotifyApiError>> {
        let id = id.to_owned();

        Box::pin(async move {
            let artist = self.cache_get_or_write(SpotCacheKey::Artist(&id), None, |etag| {
                self.client.get_artist(&id).etag(etag).send()
            });

            let albums = self.get_artist_albums(&id, 0, 20);

            let top_tracks =
                self.cache_get_or_write(SpotCacheKey::ArtistTopTracks(&id), None, |etag| {
                    self.client.get_artist_top_tracks(&id).etag(etag).send()
                });

            let (artist, albums, top_tracks) = join!(artist, albums, top_tracks);

            let artist = artist?;
            let result = ArtistDescription {
                id: artist.id,
                name: artist.name,
                albums: albums?,
                top_tracks: top_tracks?.into(),
            };
            Ok(result)
        })
    }

    fn search(
        &self,
        query: &str,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<SpotifyResult<SearchResults>> {
        let query = query.to_owned();

        Box::pin(async move {
            let results = self
                .client
                .search(query, offset, limit)
                .send()
                .await?
                .deserialize()
                .ok_or(SpotifyApiError::NoContent)?;

            let albums = results
                .albums
                .unwrap_or_else(Page::empty)
                .items
                .into_iter()
                .map(|saved| saved.into())
                .collect::<Vec<AlbumDescription>>();

            let artists = results
                .artists
                .unwrap_or_else(Page::empty)
                .items
                .into_iter()
                .map(|saved| saved.into())
                .collect::<Vec<ArtistSummary>>();

            Ok(SearchResults { albums, artists })
        })
    }
}

#[cfg(test)]
pub mod tests {

    use super::*;

    #[test]
    fn test_search_query() {
        let query = SearchQuery {
            query: "test".to_string(),
            types: vec![SearchType::Album, SearchType::Artist],
            limit: 5,
            offset: 0,
        };

        assert_eq!(
            query.into_query_string(),
            "type=album,artist&q=test&offset=0&limit=5&market=from_token"
        );
    }

    #[test]
    fn test_search_query_spaces_and_stuff() {
        let query = SearchQuery {
            query: "test??? wow".to_string(),
            types: vec![SearchType::Album],
            limit: 5,
            offset: 0,
        };

        assert_eq!(
            query.into_query_string(),
            "type=album&q=test+wow&offset=0&limit=5&market=from_token"
        );
    }

    #[test]
    fn test_search_query_encoding() {
        let query = SearchQuery {
            query: "кириллица".to_string(),
            types: vec![SearchType::Album],
            limit: 5,
            offset: 0,
        };

        assert_eq!(query.into_query_string(), "type=album&q=%D0%BA%D0%B8%D1%80%D0%B8%D0%BB%D0%BB%D0%B8%D1%86%D0%B0&offset=0&limit=5&market=from_token");
    }
}
