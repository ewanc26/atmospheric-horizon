// The mod's row in Options > Mods, with its key.
fn atmospheric_listed(mods: &mut Vec<crate::game::native_hooks::NativeMod>) {
    use crate::game::atmospheric as m;
    mods.push(crate::game::native_hooks::NativeMod {
        id: m::ID.into(),
        title: format!("{} {}", m::NAME, m::VERSION),
        description: m::DESCRIPTION.into(),
        keys: vec![
            [
                m::KEY_POST.into(),
                m::KEY_POST_LABEL.into(),
                m::KEY_POST_DEFAULT.into(),
            ],
            [
                m::KEY_SHOT.into(),
                m::KEY_SHOT_LABEL.into(),
                m::KEY_SHOT_DEFAULT.into(),
            ],
        ],
    });
}
