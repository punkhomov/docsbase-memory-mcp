//! Only known project visibility transitions may be retried by test polls.

use docsbase_memory::error::Error;

pub fn temporarily_unavailable(error: &Error) -> bool {
    matches!(error, Error::Project { message, .. }
        if message.contains("(status:")
            || message.contains("needs to be rebuilt")
            || message == "project has an incomplete index update"
            || message == "project generation changed during search preparation"
            || message == "project configuration changed before search preparation"
            || (message.starts_with("another writer holds ") && message.ends_with(".writer.lock")))
}
