use crate::Client;
use crate::error::Result;
use crate::types::Playlist;

impl Client {
    /// `createPlaylist`: a new playlist with `song_ids` in order.
    pub async fn create_playlist(&self, name: &str, song_ids: &[String]) -> Result<Playlist> {
        let mut params = vec![("name", name.to_owned())];
        params.extend(song_ids.iter().map(|id| ("songId", id.clone())));
        self.get("createPlaylist", &params, "playlist").await
    }
}

impl Client {
    /// `updatePlaylist`: rename, append songs and/or remove songs by index.
    pub async fn update_playlist(
        &self,
        playlist_id: &str,
        name: Option<&str>,
        add_song_ids: &[String],
        remove_indexes: &[usize],
    ) -> Result<()> {
        let mut params = vec![("playlistId", playlist_id.to_owned())];
        if let Some(name) = name {
            params.push(("name", name.to_owned()));
        }
        params.extend(add_song_ids.iter().map(|id| ("songIdToAdd", id.clone())));
        params.extend(
            remove_indexes
                .iter()
                .map(|i| ("songIndexToRemove", i.to_string())),
        );
        self.call("updatePlaylist", &params).await
    }

    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        self.call("deletePlaylist", &[("id", playlist_id.to_owned())])
            .await
    }

    /// Replaces a playlist's songs with `song_ids` in order (`createPlaylist` with
    /// `playlistId`), the Subsonic way to reorder.
    pub async fn replace_playlist_tracks(
        &self,
        playlist_id: &str,
        song_ids: &[String],
    ) -> Result<()> {
        let mut params = vec![("playlistId", playlist_id.to_owned())];
        params.extend(song_ids.iter().map(|id| ("songId", id.clone())));
        self.call("createPlaylist", &params).await
    }
}
