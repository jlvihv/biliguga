//! The keys the window handles itself.

pub(crate) use super::*;

/// Keys the window handles itself. The transport keys only count while something is open:
/// with nothing playing, Space used to start a "playback" of nothing, and it swallowed the
/// key a reader might be using for something else.
pub(crate) fn on_key(app: &mut BiliGuga, event: &KeyEvent, tx: &mut Tx) -> bool {
    let mods = event.mods;
    let watching = app.playing_video.is_some();
    match event.key {
        Key::Text(ref typed) if typed == " " && !mods.ctrl && !mods.alt && watching => {
            app.toggle_pause();
            true
        }
        Key::Left if watching => {
            app.player.seek_relative(-10.);
            true
        }
        Key::Right if watching => {
            app.player.seek_relative(10.);
            true
        }
        Key::Up if watching => {
            app.volume = (app.volume + 5.).clamp(0., 100.);
            app.player.set_volume(app.volume);
            app.persist_player_settings();
            true
        }
        Key::Down if watching => {
            app.volume = (app.volume - 5.).clamp(0., 100.);
            app.player.set_volume(app.volume);
            app.persist_player_settings();
            true
        }
        Key::Text(ref typed)
            if typed.eq_ignore_ascii_case("f") && !mods.ctrl && !mods.alt && watching =>
        {
            app.player_fullscreen = !app.player_fullscreen;
            tx.toggle_full_screen();
            true
        }
        Key::Escape => {
            if app.player_fullscreen {
                app.player_fullscreen = false;
                tx.toggle_full_screen();
                return true;
            }
            // Windowed, Escape is the way out of the player: back to the browse grid.
            if watching {
                app.close_player();
                return true;
            }
            false
        }
        Key::Enter if mods.ctrl || mods.alt => {
            app.load_search();
            true
        }
        _ => false,
    }
}
