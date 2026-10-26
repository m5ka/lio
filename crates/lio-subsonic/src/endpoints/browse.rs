use crate::Client;
use crate::error::Result;
use crate::types::{Album, AlbumList2};

/// `type` values for `getAlbumList2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlbumListType {
    Newest,
    AlphabeticalByName,
    AlphabeticalByArtist,
    ByYear { from: i32, to: i32 },
    Frequent,
    Recent,
    Random,
    Starred,
    ByGenre(String),
}

impl AlbumListType {
    fn params(&self) -> Vec<(&'static str, String)> {
        let kind = |k: &str| vec![("type", k.to_owned())];
        match self {
            Self::Newest => kind("newest"),
            Self::AlphabeticalByName => kind("alphabeticalByName"),
            Self::AlphabeticalByArtist => kind("alphabeticalByArtist"),
            Self::ByYear { from, to } => {
                let mut p = kind("byYear");
                p.push(("fromYear", from.to_string()));
                p.push(("toYear", to.to_string()));
                p
            }
            Self::Frequent => kind("frequent"),
            Self::Recent => kind("recent"),
            Self::Random => kind("random"),
            Self::Starred => kind("starred"),
            Self::ByGenre(genre) => {
                let mut p = kind("byGenre");
                p.push(("genre", genre.clone()));
                p
            }
        }
    }
}

/// Largest page `getAlbumList2` accepts.
pub const ALBUM_PAGE_SIZE: u32 = 500;

impl Client {
    /// One page of `getAlbumList2`. `size` is clamped to 500.
    pub async fn get_album_list2(
        &self,
        list: &AlbumListType,
        size: u32,
        offset: u32,
    ) -> Result<Vec<Album>> {
        let mut params = list.params();
        params.push(("size", size.min(ALBUM_PAGE_SIZE).to_string()));
        params.push(("offset", offset.to_string()));
        let page: AlbumList2 = self.get("getAlbumList2", &params, "albumList2").await?;
        Ok(page.album)
    }

    /// `getAlbum`: album with its songs.
    pub async fn get_album(&self, id: &str) -> Result<Album> {
        self.get("getAlbum", &[("id", id.to_owned())], "album")
            .await
    }
}

/// Result limits for `search3`.
#[derive(Debug, Clone, Copy)]
pub struct SearchLimits {
    pub artists: u32,
    pub albums: u32,
    pub songs: u32,
}

impl Default for SearchLimits {
    /// 20 artists, 20 albums, 50 songs.
    fn default() -> Self {
        Self {
            artists: 20,
            albums: 20,
            songs: 50,
        }
    }
}

impl Client {
    /// `getArtists`: the whole artist index.
    pub async fn get_artists(&self) -> Result<crate::types::ArtistsIndex> {
        self.get("getArtists", &[], "artists").await
    }

    /// `getArtist`: an artist with its albums.
    pub async fn get_artist(&self, id: &str) -> Result<crate::types::Artist> {
        self.get("getArtist", &[("id", id.to_owned())], "artist")
            .await
    }

    pub async fn get_genres(&self) -> Result<Vec<crate::types::Genre>> {
        let genres: crate::types::Genres = self.get("getGenres", &[], "genres").await?;
        Ok(genres.genre)
    }

    pub async fn get_song(&self, id: &str) -> Result<crate::types::Song> {
        self.get("getSong", &[("id", id.to_owned())], "song").await
    }

    pub async fn get_scan_status(&self) -> Result<crate::types::ScanStatus> {
        self.get("getScanStatus", &[], "scanStatus").await
    }

    /// `getStarred2`: favourite artists, albums and songs.
    pub async fn get_starred2(&self) -> Result<crate::types::ItemSet> {
        self.get("getStarred2", &[], "starred2").await
    }

    pub async fn get_playlists(&self) -> Result<Vec<crate::types::Playlist>> {
        let playlists: crate::types::Playlists = self.get("getPlaylists", &[], "playlists").await?;
        Ok(playlists.playlist)
    }

    /// `getPlaylist`: a playlist with its entries.
    pub async fn get_playlist(&self, id: &str) -> Result<crate::types::Playlist> {
        self.get("getPlaylist", &[("id", id.to_owned())], "playlist")
            .await
    }

    /// `search3` with result offsets of 0.
    pub async fn search3(
        &self,
        query: &str,
        limits: SearchLimits,
    ) -> Result<crate::types::ItemSet> {
        let params = [
            ("query", query.to_owned()),
            ("artistCount", limits.artists.to_string()),
            ("albumCount", limits.albums.to_string()),
            ("songCount", limits.songs.to_string()),
            ("artistOffset", "0".to_owned()),
            ("albumOffset", "0".to_owned()),
            ("songOffset", "0".to_owned()),
        ];
        self.get("search3", &params, "searchResult3").await
    }

    /// Pages through `getAlbumList2` until a short page (contract invariant 4).
    pub async fn get_all_albums(&self, list: &AlbumListType) -> Result<Vec<Album>> {
        let mut all = Vec::new();
        let mut offset = 0;
        loop {
            let page = self.get_album_list2(list, ALBUM_PAGE_SIZE, offset).await?;
            let n = page.len() as u32;
            all.extend(page);
            if n < ALBUM_PAGE_SIZE {
                return Ok(all);
            }
            offset += n;
        }
    }
}
