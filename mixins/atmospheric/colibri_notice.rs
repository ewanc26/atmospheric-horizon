// A server system broadcast line (HEAD of broadcast_message): relay the
// join/leave/death/advancement lines into the bridged Colibri channel under
// the server account. Ordinary chat and unknown lines are ignored by the
// translation key, matching atpcraft's ColibriServerEvents filter.
//
// M29 also marks join/leave lines handled for the presence fallback, and a
// blank line for every kind: a chat mod that hides a line broadcasts textless
// output, which must suppress the connection-event fallback.
fn atmospheric_colibri_notice(line: &horizon_client::text::Text) {
    use crate::game::atmospheric as m;
    let key = line
        .component()
        .get("translate")
        .and_then(|value| value.as_str());
    let text = line.plain();
    let kind = key.and_then(m::colibri_notice_kind);
    if text.trim().is_empty() {
        m::post_colibri_handled(kind);
        return;
    }
    let Some(kind) = kind else {
        return;
    };
    if matches!(kind, "join" | "leave") {
        m::post_colibri_handled(Some(kind));
    }
    m::post_colibri_notice(&text);
}
