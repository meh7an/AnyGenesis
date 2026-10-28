//! The player as a static library for game programs: anygenesis links it with a game's
//! translated C, whose `ag_game` table (`AgGame` in the abi crate) it reads, and the C
//! `main` hands over to [`ag_player_main`].

use std::ffi::c_int;

use anygenesis_abi::AgGame;
use anygenesis_player::Game;

unsafe extern "C" {
    static ag_game: AgGame;
}

/// Runs the program's game as its command line asks and returns the exit status: 0, or 1
/// after printing the error as a Rust `main` would.
#[unsafe(no_mangle)]
pub extern "C" fn ag_player_main() -> c_int {
    // SAFETY: every program that links this library defines `ag_game` in its read-only
    // data, where it never changes.
    let table = unsafe { &ag_game };
    match Game::from_table(table).and_then(anygenesis_player::main) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error:?}");
            1
        }
    }
}
