//! Decoding of the Clipboard portal's `SelectionOwnerChanged` signal.
//!
//! ashpd's decoder expects `mime_types` to be `as`, but
//! xdg-desktop-portal-gnome 46 sends it wrapped in a struct, `(as)`. ashpd
//! then drops the whole signal without a word, so copies never reached KVM
//! Bridge. Both forms are accepted here.

use ashpd::zbus::Message;
use ashpd::zvariant::{OwnedObjectPath, OwnedValue, Value};
use std::collections::HashMap;

/// A new clipboard owner.
#[derive(Debug, Default, PartialEq)]
pub struct OwnerChange {
    /// Mime types the new owner offers.
    pub mime_types: Vec<String>,
    /// The new owner is our own session (we set the selection).
    pub session_is_owner: bool,
}

/// Decodes a `SelectionOwnerChanged (o, a{sv})` signal.
pub fn owner_change(msg: &Message) -> Option<OwnerChange> {
    match msg.body().deserialize::<(OwnedObjectPath, HashMap<String, OwnedValue>)>() {
        Ok((_session, options)) => Some(from_options(&options)),
        Err(e) => {
            tracing::warn!("cannot decode a clipboard change: {e}");
            None
        }
    }
}

/// Reads the signal's options, with `mime_types` as either `as` or `(as)`.
pub fn from_options(options: &HashMap<String, OwnedValue>) -> OwnerChange {
    let mut mime_types = Vec::new();
    if let Some(v) = options.get("mime_types") {
        strings(v, &mut mime_types);
    }
    let session_is_owner = matches!(options.get("session_is_owner").map(|v| &**v), Some(Value::Bool(true)));
    OwnerChange { mime_types, session_is_owner }
}

fn strings(v: &Value<'_>, out: &mut Vec<String>) {
    match v {
        Value::Str(s) => out.push(s.to_string()),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Structure(s) => s.fields().iter().for_each(|x| strings(x, out)),
        Value::Value(inner) => strings(inner, out),
        _ => {}
    }
}
