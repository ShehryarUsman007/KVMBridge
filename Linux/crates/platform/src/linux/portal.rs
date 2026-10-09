//! Keeping portal sessions from outliving the portal frontend.
//!
//! xdg-desktop-portal 1.18 crashes on some clipboard errors. When it comes
//! back it knows nothing about the sessions it had, but the desktop's portal
//! backend (xdg-desktop-portal-gnome) and the compositor keep them: our
//! handles stop working, while an input capture session left behind can hold
//! every key and mouse event (in mutter 46 even the power key) with nothing
//! able to release it. So when the frontend goes away, or a call to release
//! input fails, our sessions are closed directly at the backend, which also
//! ends them in the compositor, and set up again.

use ashpd::zbus;
use futures::{Stream, StreamExt};

const FRONTEND: &str = "org.freedesktop.portal.Desktop";
const BACKEND_PREFIX: &str = "org.freedesktop.impl.portal.desktop.";

/// The handle (object path) of a portal session.
pub(crate) fn handle_of<S: serde::Serialize>(session: &S) -> Option<String> {
    serde_json::to_value(session).ok()?.as_str().map(String::from)
}

/// Yields whenever the portal frontend loses its bus name (it crashed or was
/// replaced): sessions made through it are orphaned from then on.
pub(crate) async fn frontend_gone() -> zbus::Result<impl Stream<Item = ()> + Unpin> {
    let conn = zbus::Connection::session().await?;
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    let changes = dbus.receive_name_owner_changed_with_args(&[(0, FRONTEND)]).await?;
    Ok(changes
        .filter_map(|c| async move { c.args().ok().and_then(|a| a.old_owner().as_ref().map(|_| ())) })
        .boxed())
}

/// Close the session `handle` at the portal backend directly (the frontend
/// that made it is gone). `what` names it for the log.
pub(crate) async fn close_at_backend(handle: &str, what: &str) {
    let Ok(conn) = zbus::Connection::session().await else { return };
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else { return };
    let names = dbus.list_names().await.unwrap_or_default();
    for name in names.iter().filter(|n| n.as_str().starts_with(BACKEND_PREFIX)) {
        match conn
            .call_method(Some(name.as_str()), handle, Some("org.freedesktop.impl.portal.Session"), "Close", &())
            .await
        {
            Ok(_) => tracing::info!("closed the left-over {what} session at {name}"),
            Err(e) => tracing::debug!("closing the {what} session at {name}: {e}"),
        }
    }
}

/// `next()` of an optional stream; pending forever without one.
pub(crate) async fn next_opt<S: Stream + Unpin>(s: &mut Option<S>) -> Option<S::Item> {
    match s {
        Some(s) => s.next().await,
        None => std::future::pending().await,
    }
}
