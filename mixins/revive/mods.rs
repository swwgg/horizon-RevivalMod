fn revive_mods(mods: &mut Vec<crate::game::native_hooks::NativeMod>) {
    mods.push(crate::game::native_hooks::NativeMod {
        id: crate::game::revive_glue::ID.to_owned(),
        title: format!("{} {}", crate::game::revive_glue::NAME, crate::game::revive_glue::VERSION),
        description: "Hardcore revive mod: downed players can be revived by teammates.".to_owned(),
        keys: Vec::new(),
    });
}
