//! Tiny localStorage wrapper for column-layout persistence. On the `web` feature
//! it hits `window.localStorage`; on native targets every call is a no-op (the
//! grid simply doesn't persist layout there, which is the desired behavior).

#[cfg(feature = "web")]
fn store() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}

#[cfg(feature = "web")]
pub fn get(key: &str) -> Option<String> {
    store().and_then(|s| s.get_item(key).ok().flatten())
}

#[cfg(feature = "web")]
pub fn set(key: &str, value: &str) {
    if let Some(s) = store() {
        let _ = s.set_item(key, value);
    }
}

#[cfg(not(feature = "web"))]
pub fn get(_key: &str) -> Option<String> {
    None
}

#[cfg(not(feature = "web"))]
pub fn set(_key: &str, _value: &str) {}
