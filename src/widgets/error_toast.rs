//! User-facing messages for [`lio_subsonic::Error`].

use gettextrs::gettext;
use lio_subsonic::Error;

/// Short, user-facing message for an error.
pub fn message(error: &Error) -> String {
    match error {
        Error::Unreachable => gettext("Can't reach the server"),
        Error::Certificate { .. } => gettext("The server's certificate isn't trusted"),
        Error::Incompatible => gettext("This doesn't look like a Navidrome server"),
        Error::BadCredentials => gettext("Wrong username or password"),
        Error::ServerTooOld => gettext("Server version not supported"),
        Error::NotAuthorized => gettext("Your account isn't allowed to do this"),
        Error::NotFound => gettext("This item no longer exists on the server"),
        Error::InvalidUrl => gettext("Enter a valid server address"),
        Error::Server { message, .. } if !message.is_empty() => {
            gettext("The server reported an error: {}").replace("{}", message)
        }
        Error::Server { .. } | Error::Http(_) | Error::Decode(_) => {
            gettext("Something went wrong talking to the server")
        }
    }
}
