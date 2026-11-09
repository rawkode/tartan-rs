//! The identity actor's public interface.
//!
//! The identity actor is a forge singleton implemented in `tartan-forge`. Its name, its key and
//! the message types of its actions live here, so `tt admin` can call it through the Rivet client
//! without linking the forge.
//!
//! Every message type implements [`IdentityAction`], which names the action and its output. With
//! the `rivetkit` feature, which the forge enables, each also implements rivetkit's `Action` with
//! the same name and output. `tt` builds without the feature and sends the messages with
//! `rivetkit-client`'s untyped `ActorHandle::action(IdentityAction::NAME, args)`.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The actor name the identity actor is registered under.
pub const ACTOR_NAME: &str = "identity";

/// The key of the identity actor for the forge `forge_id`: one identity actor per forge.
pub fn identity_key(forge_id: &str) -> Vec<String> {
    vec![forge_id.to_owned()]
}

/// An action of the identity actor: its wire name and the type it answers with.
///
/// This mirrors rivetkit's `Action` trait so callers need not link the rivetkit server SDK.
pub trait IdentityAction: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// What the action answers with.
    type Output: Serialize + DeserializeOwned + Send + 'static;

    /// The action's name on the wire.
    const NAME: &'static str;
}

/// Declares that `$message` is the identity action `$name` answering with `$output`, implementing
/// [`IdentityAction`] and, with the `rivetkit` feature, rivetkit's `Action`. It is used only in this
/// crate, where the feature check applies.
#[allow(unused_macros)]
macro_rules! identity_action {
    ($message:ty, $name:literal, $output:ty) => {
        impl crate::IdentityAction for $message {
            type Output = $output;
            const NAME: &'static str = $name;
        }

        #[cfg(feature = "rivetkit")]
        impl ::rivetkit::Action for $message {
            type Output = $output;
            const NAME: &'static str = $name;
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Ping {
        n: u32,
    }

    identity_action!(Ping, "Ping", u32);

    #[test]
    fn declared_actions_carry_their_name() {
        assert_eq!(<Ping as IdentityAction>::NAME, "Ping");
        #[cfg(feature = "rivetkit")]
        assert_eq!(<Ping as rivetkit::Action>::NAME, "Ping");
    }

    #[test]
    fn key_is_the_forge_id() {
        assert_eq!(identity_key("forge"), vec!["forge".to_owned()]);
    }
}
