//! Checking the password with PAM, the system's authentication framework.
//!
//! PAM works as a conversation: it asks questions ("Password:") through
//! callbacks and we answer. Our answers come from what the user typed.

use std::ffi::{CStr, CString};

use pam_client::{Context, ConversationHandler, ErrorCode, Flag};
use zeroize::Zeroizing;

/// The PAM service name; its rules are in `/etc/pam.d/meridian-lock`.
pub const SERVICE: &str = "meridian-lock";
pub const SERVICE_FILE: &str = "/etc/pam.d/meridian-lock";

/// Answers PAM's prompts. `Zeroizing` overwrites the password in memory
/// when this struct is dropped. Nothing here is ever printed or logged.
struct Answers {
    password: Zeroizing<String>,
}

impl ConversationHandler for Answers {
    // A visible prompt would be the user name, which PAM already has.
    fn prompt_echo_on(&mut self, _prompt: &CStr) -> Result<CString, ErrorCode> {
        Err(ErrorCode::CONV_ERR)
    }

    // A hidden prompt is the password.
    fn prompt_echo_off(&mut self, _prompt: &CStr) -> Result<CString, ErrorCode> {
        CString::new(self.password.as_bytes()).map_err(|_| ErrorCode::CONV_ERR)
    }

    // Messages from PAM modules are deliberately dropped, not logged.
    fn text_info(&mut self, _msg: &CStr) {}
    fn error_msg(&mut self, _msg: &CStr) {}
}

/// True if `password` is `user`'s password and the account may log in.
///
/// This can take seconds (PAM delays after a wrong password), so call it
/// from a worker thread, never from the UI thread.
pub fn check(user: &str, password: Zeroizing<String>) -> bool {
    let Ok(mut context) = Context::new(SERVICE, Some(user), Answers { password }) else {
        return false;
    };
    // `authenticate` checks the password; `acct_mgmt` checks that the
    // account is not expired or locked.
    context.authenticate(Flag::NONE).is_ok() && context.acct_mgmt(Flag::NONE).is_ok()
}
