// A server system broadcast line (HEAD of broadcast_message): relay the
// join/leave/death/advancement lines into the bridged Colibri channel under
// the server account. Ordinary chat and unknown lines are ignored by the
// translation key, matching atpcraft's ColibriServerEvents filter.
fn atmospheric_colibri_notice(line: &horizon_client::text::Text) {
    use crate::game::atmospheric as m;
    let Some(key) = line
        .component()
        .get("translate")
        .and_then(|value| value.as_str())
    else {
        return;
    };
    if !m::is_colibri_notice(key) {
        return;
    }
    let text = line.plain();
    if text.trim().is_empty() {
        return;
    }
    m::post_colibri_notice(&text);
}
