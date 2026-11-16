//! Touch ID or the account password (LocalAuthentication's device-owner policy), for a change
//! that needs the person at this Mac: letting another of my Macs drive it (Settings ▸ Nearby).
//!
//! macOS shows its own prompt, "Midna is trying to <reason>.", and answers on a queue of its own;
//! `ask` hands that answer back over a channel. Dropping the `Asking` (or `cancel`) withdraws a
//! prompt that's still up.
use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::{NSError, NSString};

#[link(name = "LocalAuthentication", kind = "framework")]
unsafe extern "C" {}

/// `LAPolicyDeviceOwnerAuthentication`: Touch ID (or a paired Watch) with the account password
/// as the fallback, so a Mac without Touch ID still asks.
const DEVICE_OWNER: isize = 2;
/// `LAErrorUserCancel`, `LAErrorSystemCancel`, `LAErrorAppCancel`: nobody said no to a wrong
/// password, the prompt just went away.
const CANCELLED: [isize; 3] = [-2, -4, -9];

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The person proved they're at this Mac.
    Allowed,
    /// The prompt was cancelled (by them, by macOS, or by `Asking::cancel`).
    Cancelled,
    /// It failed: too many tries, no password set, … (macOS's own words).
    Failed(String),
}

/// A prompt that's up. The answer comes once on `answer`.
pub struct Asking {
    context: Option<Retained<AnyObject>>,
    pub answer: async_channel::Receiver<Outcome>,
}

impl Asking {
    /// Take the prompt down; `answer` gets `Cancelled`.
    pub fn cancel(&self) {
        if let Some(c) = &self.context {
            let _: () = unsafe { msg_send![&**c, invalidate] };
        }
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Ask macOS to check it's the Mac's owner. `reason` finishes "Midna is trying to …".
pub fn ask(reason: &str) -> Asking {
    let (tx, answer) = async_channel::bounded(1);
    let Some(class) = AnyClass::get(c"LAContext") else {
        let _ = tx.try_send(Outcome::Failed("Touch ID and password checks aren't available on this Mac".into()));
        return Asking { context: None, answer };
    };
    let context: Retained<AnyObject> = unsafe { msg_send![class, new] };
    let reply = RcBlock::new(move |ok: Bool, err: *mut NSError| {
        let outcome = match unsafe { err.as_ref() } {
            _ if ok.as_bool() => Outcome::Allowed,
            Some(e) if CANCELLED.contains(&e.code()) => Outcome::Cancelled,
            Some(e) => Outcome::Failed(e.localizedDescription().to_string()),
            None => Outcome::Failed("macOS didn't say why".into()),
        };
        let _ = tx.try_send(outcome);
    });
    let reason = NSString::from_str(reason);
    let _: () = unsafe { msg_send![&*context, evaluatePolicy: DEVICE_OWNER, localizedReason: &*reason, reply: &*reply] };
    Asking { context: Some(context), answer }
}
