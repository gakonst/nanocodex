## Spotify Computer Use

### Playing media

The Spotify app doesn't immediately update after requesting playback, so the result from a click might indicate paused media or outdated media. Instead of acting again, first: run `get-state` to confirm it didn't take. You may be pleasantly surprised. Do not sleep any time, it should be updated by the time you notice and request another `get-state`.

### Searching

Be sure the search field is focused before pressing return to search. If you press return without the search field focused, it may affect playback inadvertently.

### General Navigation

This app is not fully local state. That means you must sometimes wait for network to give you a response. When searching, that means it might say "no results" momentarily. Err on running `get-state` again before changing course.
