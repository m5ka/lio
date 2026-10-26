//! GTK widgets and window-level helpers. Content pages live in [`pages`].

pub mod adaptive;
pub mod add_to_playlist;
pub mod album_tile;
pub mod cert_dialog;
pub mod cover_view;
pub mod download_toggle;
pub mod editable_rows;
pub mod error_toast;
pub mod item_menu;
pub mod navigate;
pub mod now_playing;
pub mod pages;
pub mod player_bar;
pub mod preferences;
pub mod queue_sidebar;
pub mod storage;
pub mod tint;
pub mod track_list_row;
pub mod track_row;
pub mod window;

pub use album_tile::LioAlbumTile;
pub use now_playing::LioNowPlaying;
pub use pages::album_grid::LioAlbumGrid;
pub use pages::album_page::LioAlbumPage;
pub use pages::artist_page::LioArtistPage;
pub use pages::favourites_page::LioFavouritesPage;
pub use pages::home_page::LioHomePage;
pub use pages::list_page::{LioListPage, LioListRow};
pub use pages::playlist_page::LioPlaylistPage;
pub use pages::search_page::LioSearchPage;
pub use pages::sign_in_page::LioSignInPage;
pub use player_bar::LioPlayerBar;
pub use queue_sidebar::LioQueueSidebar;
pub use track_list_row::LioTrackListRow;
pub use track_row::LioTrackRow;
pub use window::{LioWindow, Section};
